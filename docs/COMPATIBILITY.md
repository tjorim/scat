# Catalog compatibility, upgrade and rollback

> **Status:** accepted. This is the decision record and operating contract for
> how a published catalog, its embeddings sidecar, host-local caches and scat
> binaries of different versions relate. Tests in `tests/compat.rs` keep it
> true; [RELEASING.md](RELEASING.md) is the checklist that exercises it.

## Context

One publisher builds `scripts.sqlite` nightly and replaces it atomically.
Clients are independently deployed (RPM) and open the catalog read-only
([ARCHITECTURE.md §3](ARCHITECTURE.md)). Four artifacts carry a version:

| Artifact | Version | Where it is stored | Checked by |
|----------|---------|--------------------|------------|
| Catalog | schema version | `index_metadata.schema_version` | `core::db::open_db` |
| Embeddings sidecar | format version | `embedding_metadata.format_version` (absent = format 1) | `EmbeddingsSidecar::open_checked` |
| Host-local cache entry | none of its own | a byte-for-byte copy of the catalog, keyed by published path + file identity | the checks above, on open |
| scat binary | the two constants above | `SCHEMA_VERSION`, `SIDECAR_FORMAT_VERSION` | — |

Until now a client opened a catalog only if the schema matched exactly, and
told the user to "re-index" — advice that is wrong for a read-only client.

## Decision

**Coordinated upgrades with an exact schema match.** A client reads exactly the
catalog schema it was built for; anything else is refused, unmodified, with a
message that says which side to upgrade. The policy lives in one function,
`core::db::schema_compatibility`.

| Option | Cost | Verdict |
|--------|------|---------|
| **Coordinated upgrade, exact match** | A short window per client between its upgrade and the catalog flip, where it is refused. No reader code beyond the existing check. | **Chosen.** One publisher, RPM-managed clients and nightly publication make a maintenance window cheap; every schema bump so far has changed columns, which a reader window would have to emulate in every query. |
| Parallel versioned catalogs (`scripts.sqlite` per schema, publisher runs both builds) | Double build time and storage, a path convention, and the publisher needs both scat versions. Zero client downtime. | Not now. It needs no *client* machinery, so it is the next step if a window becomes unacceptable. |
| Reader window (N reads N-1 …) | Every query tolerates old layouts; a `min_reader` field in the catalog; permanent test matrix. | Not now. Disproportionate for a fleet that can upgrade together. |

**Revisit** (to parallel catalogs first) when clients can no longer be upgraded
within one maintenance window, or when the window's outage is no longer
acceptable. Changing the policy means changing `schema_compatibility`, this
table, and the combination matrix below together.

Retained regardless: clients never open the catalog read-write, publication is
an atomic `rename(2)`, and no migration framework exists. Catalogs are rebuilt,
never migrated; a build whose predecessor has a different schema is a full
rebuild automatically.

## Supported combinations

Catalog schema (`SCHEMA_VERSION` in `src/core/db.rs`):

| Schema | Status | Read by |
|--------|--------|---------|
| 14 | current | scat built from this tree |

Embeddings sidecar format (`SIDECAR_FORMAT_VERSION` in `src/core/embeddings.rs`):

| Format | Status | Notes |
|--------|--------|-------|
| 1 | current | Includes sidecars written before `embedding_metadata` existed (no provenance) |

A test fails if either table stops listing the compiled-in version as
`current`, so a bump cannot merge without updating this document.

| Client | Catalog | Result |
|--------|---------|--------|
| schema N | schema N | Works. |
| schema N | schema < N | Refused: *the catalog is older than this scat; the catalog publisher must upgrade scat and run `scat catalog build`*. |
| schema N | schema > N | Refused: *this scat is older than the catalog; upgrade scat on this host (do not rebuild the shared catalog)*. |
| any | no `index_metadata` | Refused: not a scat catalog, or published by a much older scat. |

"Refused" holds for every command that reads the catalog (CLI and TUI, which
checks once before taking over the terminal), with the cache on or off.
`scat catalog info` and `scat doctor` are the exception that explain the
refusal: they print both versions and a `compatibility` verdict
(`compatible`, `catalog_older`, `catalog_newer`), and `catalog info` then exits
non-zero. Nothing in a refusal writes to the share.

`scat catalog diff` against a rotated `.1`/`.2` copy needs both files to be the
current schema, so across a schema change it fails (with the message above)
until two builds share the new schema.

## Host-local cache

A cache entry is a faithful copy of the published file, keyed by the file's path
and identified by its `stat` identity (length, mtime, inode, device). It is
refreshed whenever that identity changes — which every atomic publication and
every rollback by `rename` does.

- **Unreadable catalogs are not cached.** If the published catalog's schema is
  not the client's, nothing is copied and any older entry is not served in its
  place; the client reads the published file in place and reports the
  mismatch. (Only an empty lock file can be left behind.)
- **Same build, different schema is not the same build.** When the identity
  changes but the new file carries the build timestamp already cached, the copy
  is re-stamped instead of recopied only if the schema version also matches.
- **Rollback by restoring the previous file** (the `.1` hard link) brings back
  the inode the cache already holds, so the entry is reused without a recopy.
  Rollback by rebuilding with the old binary produces a new file and a normal
  refresh.
- Cache entries live in a per-user tmpfs directory and may be written by
  different scat versions on one host during a rollout. Entries are never
  interpreted without the checks above, so a copy made by one version is safe
  for another to open or ignore.

## Embeddings sidecar

The sidecar pairs with the catalog **by `logical_path`**, so it does not depend
on the catalog schema: a schema bump needs no sidecar rebuild, and sidecar and
catalog may be republished in either order. It is built out of band
(`scat-embed`) and is normally older than the nightly catalog.

- **Format.** A sidecar newer than the client's format is refused with an
  upgrade message (`similar` fails; `catalog audit` still runs and prints what
  it skipped). A sidecar with no `embedding_metadata` table is format 1.
  Additive changes (a new optional table) do not bump the format; only a
  layout older clients cannot read does.
- **Provenance.** `scat-embed` records the `build_timestamp` of the catalog it
  embedded. When that differs from the catalog being queried, `similar` prints
  a note naming both builds. It is a warning, not a refusal, because the
  pairing by path stays valid for unchanged scripts. Legacy sidecars have no
  provenance and no note; the existing coverage note (`covers N/M scripts`)
  still applies.
- **Cache.** The sidecar is cached under its own entry and follows republish
  and rollback exactly like the catalog.

## Rolling out a schema change

Run during a maintenance window; announce it, because clients are refused
between their upgrade and the flip.

1. **Publisher.** Install the new RPM. Pause the nightly cron.
2. **Stage.** Build to a path beside the catalog so nothing changes for
   clients yet, and check it:
   ```bash
   scat --config /path/to/site.yaml --db /share/scripts.sqlite.next catalog build
   scat --db /share/scripts.sqlite.next catalog info   # compatibility: compatible
   scat --db /share/scripts.sqlite.next search <known term>
   ```
3. **Keep the outgoing catalog** for rollback, then flip (`rename(2)` is atomic
   and readers holding the old file keep reading it):
   ```bash
   ln -f /share/scripts.sqlite /share/scripts.sqlite.pre-upgrade
   mv /share/scripts.sqlite.next /share/scripts.sqlite
   ```
4. **Clients.** Upgrade scat on every client host immediately before or after
   the flip (a refused client prints which side is behind). Clients not yet
   upgraded report *this scat is older than the catalog* until they are.
5. **Resume the cron** (now running the new binary). Republish the sidecar if
   its format changed, after clients are upgraded.

For a small catalog the staging steps can be skipped: install the new RPM on
the publisher and run `scat catalog build`; the old-schema predecessor forces
a full rebuild and is rotated to `.1`.

## Rolling back

Pick the matching case; rollback never needs a migration.

- **Before the flip:** delete `scripts.sqlite.next`. Nothing else changed.
- **After the flip, restore the previous catalog:** reinstall the previous RPM
  on clients (and the publisher), then
  `mv /share/scripts.sqlite.pre-upgrade /share/scripts.sqlite` (or `.1` if the
  direct-build route rotated it). Confirm with the old binary's
  `scat catalog info`. Caches follow on their own.
- **After the flip, rebuild instead:** reinstall the previous RPM on the
  publisher and all clients before or alongside running `scat catalog build`.
  A catalog of another schema is never used to seed a build, so it is a full
  rebuild. Verify the rebuilt catalog with the old binary's `scat catalog info`.
- **Never leave the publisher on an older binary than the catalog it
  published** without intending to roll back: its next nightly build will
  republish the older schema and refuse every upgraded client.

## Changing a schema or format

Before merging a change to `SCHEMA_VERSION` or `SIDECAR_FORMAT_VERSION`:

1. Update the version tables above and the "current schema" note in
   [QUICKSTART.md](QUICKSTART.md); `tests/compat.rs` fails otherwise.
2. Add a `CHANGELOG.md` entry that says the release is a **coordinated
   upgrade** (or, for the sidecar, which clients can no longer read it).
3. `tests/compat.rs` derives its older/newer fixtures from the constants, so
   it needs no edits; run it and read the failures as a to-do list. The
   release smoke job derives the fixture version from `src/core/db.rs` too.
4. Walk the rollout and rollback steps against a staging catalog and tick the
   compatibility items in [RELEASING.md](RELEASING.md).

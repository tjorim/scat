# Licensing and distribution

## Approved license and scope

On 2026-10-22, maintainer Jorim Tielemans approved **Apache-2.0** for all
original project material, including both crates and artwork, subject to
checking existing contributions and third-party exclusions. The approval also
confirms the same scope for the separate ElevenClock-rs project; its own
licensing record and legacy exclusions remain authoritative for that project.

The root [LICENSE](../LICENSE), both Cargo manifests and the RPM spec agree.
Copyright is attributed to scat contributors; individual authors retain their
rights. The scope includes `scat` (Linux client, TUI and catalog publisher),
`scat-embed`, tooling, documentation, configuration examples, generated help,
completions/man pages, the static website, `assets/icon.svg` and its PNG/ICO
derivatives. The website build copies LICENSE and NOTICE alongside its assets.

`scat-embed` is a separate utility for producing embedding sidecars, with a
Windows CI build. Neither Linux clients nor the catalog publisher need it
installed. It has its own dependency graph and notice bundle.

## Provenance review and exclusions

A repository-history and explicit-attribution review on 2026-10-22 found commits
by the maintainer, dependency bots, and one Vibe Nuage Agent change
(`0e14bb2842d4649a435aaae95ee4604cc22c8615`, database indexes/tests), co-authored
by the maintainer. No explicit copied-code attribution was found in `src/`,
`crates/`, `assets/` or `site/`. The artwork source and generation process are
recorded in [assets/README.md](../assets/README.md). This is a limited technical
review, not an exhaustive authorship audit or a claim that Git authorship alone
establishes ownership. Newly discovered upstream material or contributor rights
must be reviewed before inclusion under the project license.

Third-party dependencies, bundled SQLite/native code, Rust toolchain components,
and downloaded embedding models retain their own terms. Apache-2.0 does not
relicense them or grant trademark rights. Cataloged scripts and their contents
also remain under their owners' terms; using scat does not relicense a catalog.
Model downloads/cache contents are not part of the executable release bundles.
Review the selected model's terms separately before deployment or redistribution.

## Artifact notices

The release workflow runs `scripts/release-notices.py` separately for each
executable and target. Each standalone artifact contains a `licenses/` directory:

- `LICENSE` and `NOTICE` for the original project material.
- `inventory.json` with dependency versions, declared terms, original notice
  locations, pinned supplemental provenance and bundle file SHA-256 values.
- `third-party-sources.tar.gz` with complete published sources for normal/build
  dependencies of that package/target, preserving nested native-code notices and
  MPL-covered sources, plus Rust toolchain copyright/license texts and native
  runtime/toolchain notices. Build dependencies are included conservatively;
  this inventory is not a claim that each one is linked into the executable.

The RPM installs that same Linux bundle under `/usr/share/licenses/scat`, marked
`%license`. Keep the bundle alongside a standalone executable when distributing
it. Do not distribute only the executable. RPM construction verifies the input
bundle; UBI install checks verify the installed hashes and required notice/source
entries. Missing, empty or changed notice material fails these checks.

The Windows ONNX Runtime binary archive omits license files. Pinned original
ONNX Runtime 1.28.0 LICENSE and ThirdPartyNotices.txt are supplied separately;
the checker verifies the ort-sys version and distribution-table digest so a
runtime update requires notice review. These native notices supplement the Rust
`ort` wrapper's licenses. Linux packaging
retains the installed musl and GCC runtime package notices from the build host.
A new runtime/backend or dependency requires review of its upstream terms,
bundled subcomponents and any additional redistribution obligations.

Four published crates omit original license files. Version-specific upstream
copies and digests are retained under
[dependency-notices/](dependency-notices/README.md). These restore notices rather
than waive their presence. `zune-inflate` uses its offered Zlib alternative.
The workspace license gate adds version-specific CDLA-Permissive-2.0 allowances
for webpki-roots/root-certs 1.0.9 certificate data; its agreement text is retained
in the source bundle as required when sharing the data.
The existing cargo-deny gate checks permitted dependency terms; private workspace
packages now participate too. Run that gate on dependency updates in addition to
the notice checks; an inventory alone does not approve a new license.

## Distribution checklist

- [ ] Confirm the approved scope and provenance record above still cover the
      release, including new contributions and any third-party exclusions.
- [ ] Confirm LICENSE, NOTICE, both Cargo manifests, the RPM spec, README,
      website and deployment guidance agree.
- [ ] Run the existing `cargo deny check licenses bans` and `cargo deny --workspace check licenses` gate in
      [CI](../.github/workflows/ci.yml). Review the resolved, target-specific
      dependencies for each shipped executable, including bundled native code;
      retain required license texts, copyright notices and any source obligations.
- [ ] Confirm both standalone artifacts contain their matching `licenses/`
      bundle, `python3 scripts/release-notices.py PATH/TO/licenses --check` passes,
      and RPM/UBI notice checks pass. Review changes in the inventory and native
      components against the previous release.
- [ ] Follow [RELEASING.md](RELEASING.md) for the catalog compatibility gate,
      coordinated upgrades and rollback policy in
      [COMPATIBILITY.md](COMPATIBILITY.md).
- [ ] Confirm the existing reproducible-RPM and UBI 8/9/10 install/upgrade/remove
      checks in [Rust Release Artifacts](../.github/workflows/rust-release.yml)
      passed for the revision being distributed. Preserve their image digests,
      logs and checksums. See [VENDORING.md](VENDORING.md#optional-rpm-x86_64)
      for packaging, signing and offline installation, and its
      [host-validation requirements](VENDORING.md#compatibility-and-release-evidence)
      before making RHEL host-support claims.
- [ ] Preserve the source revision, license/provenance approval, dependency and
      notice inventory, artifact checksums and validation results in the release
      record. Regenerate checksums after signing an RPM.

This checklist links the existing RPM readiness work rather than introducing
another RPM validation process. Preserve the
matching notice/source bundle throughout downstream distribution.

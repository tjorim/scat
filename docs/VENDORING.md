# Binary Deployment

> **Goal:** deploy `scat` on RHEL as a compiled Rust binary with no runtime
> language or package manager setup on target hosts.

---

## Release artifacts

GitHub Actions builds and uploads one artifact:

| Platform | Artifact | CI target |
|---|---|---|
| RHEL / Linux x86-64 | `scat` | `x86_64-unknown-linux-musl` |

The binary is built for the musl target to avoid host glibc version coupling
on older RHEL systems. Windows is not a supported target: the build fails
fast on non-Unix hosts rather than producing a binary whose atomic swap and
`/dev/shm` catalog cache have no working equivalent.

---

## Install

Copy the artifact to the deployment directory and make it executable:

```bash
install -m 0755 scat /catalog/scat/scat
```

Set `SCAT_DB` once so users do not need to pass `--db` on every command:

```bash
export SCAT_DB=/catalog/scat/scripts.sqlite
```

Nothing else is needed per host. The first `scat` run after each nightly
rebuild copies the catalog into `/dev/shm` and every later run on that host
queries the copy; hosts without a usable `/dev/shm` fall back to reading the
shared drive directly.

---

## Verify

Run these checks before promoting a release artifact:

```bash
scat --help
scat catalog info
scat search patch --limit 5
scat search patch --limit 5 --output json
```

The binary must also open the existing shared `scripts.sqlite` catalog
read-only and return expected results for a known search term.

---

## CI release checklist

- Tests pass with `cargo test --locked`.
- The release binary builds successfully.
- The binary passes `--help`.
- The binary opens a compatibility `scripts.sqlite` fixture and runs
  `catalog info` and `search`.
- The RPM job builds the package twice and checks the two builds are
  byte-identical, that its dependencies are only `rpmlib(...)`, that it has no
  scriptlets and that `/etc/scat/scat.yaml` is a config file.
- The RPM is installed, upgraded and removed in UBI 8, 9 and 10 containers
  using `packaging/rpm/smoke.sh`.
- Binary sizes are reported in the workflow logs for release review.

---

## TUI

The compiled Rust binary includes both CLI commands and the interactive TUI.
Users launch the TUI with:

```bash
scat tui
```

## Optional RPM (x86_64)

The RPM wraps the same static musl executable; the standalone copy and central
cron catalog publisher above remain supported. No daemon, language runtime,
package download, account, catalog or cache is installed by the RPM.

Build on an x86_64 Linux packaging host with Bash, GNU tar/coreutils, Git and
`rpmbuild`, using the checked-out source revision matching the executable:

```bash
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)
packaging/rpm/build.sh /absolute/path/to/scat out 1
rpm -qpl out/*.rpm
rpm -qpR out/*.rpm
rpm -qp --scripts out/*.rpm
sha256sum /absolute/path/to/scat out/*.rpm
```

Keep the source revision, executable digest, packaging tool versions and epoch
with the release record. CI builds twice and compares unsigned RPM bytes using
the same inputs/toolchain; reproducibility across different RPM/compressor
versions is not promised. Automatic dependency scanning and binary rewriting
are disabled because the payload is static. CI checks for an ELF interpreter;
RPM requirements should contain only RPM format capabilities, no runtime library
or interpreter dependencies. The repository currently has no LICENSE file despite
the README link; the spec records `LicenseRef-Unspecified`. Resolve the project
license before distributing packages outside your organization.

The payload installs `/usr/bin/scat`, generated Bash/Zsh/Fish completions under
`/usr/share/{bash-completion/completions,zsh/site-functions,fish/vendor_completions.d}`,
gzip-compressed man pages (`scat(1)` plus one `scat-<command>(1)` per command, generated
by `scat man --dir`) under `/usr/share/man/man1`, and generated CLI/TUI help plus deployment documentation in `/usr/share/doc/scat`.
Shells and their completion frameworks are optional. The example configuration
is `/etc/scat/scat.yaml`, marked `%config(noreplace)`. It is explicitly opt-in:

```bash
export SCAT_CONFIG=/etc/scat/scat.yaml
```

Edit `db_path` for your site or set `SCAT_DB`. The application does not implicitly
load `/etc/scat/scat.yaml`. Upgrade preserves administrator changes (a changed
vendor default may appear as `.rpmnew`); uninstall saves modified configuration
as `.rpmsave`. No scriptlets delete external catalogs, shared files, user caches
or these saved configurations. CI tests install, upgrade to a higher RPM release,
uninstall, configuration preservation and catalog checksums.

### Signing and offline installation

CI artifacts are unsigned. An organization can sign the reviewed RPM on a
separate secured signing host with `rpmsign --addsign scat-*.rpm`; provision the
private signing key only there, never in this repository. Distribute the public
key and its fingerprint through an authenticated channel. On the client:

```bash
# Verify the key fingerprint against the authenticated release record first.
rpm --import organization-rpm-public-key.asc
rpmkeys --checksig --verbose scat-*.rpm
sha256sum -c SHA256SUMS
sudo rpm -Uvh scat-*.rpm
```

Require a successful signature from the expected key, not merely an unsigned
package's digest check. Generate the published checksum manifest *after* signing,
since signing changes RPM bytes. Copy all files onto disconnected hosts; `rpm`
does not contact repositories. The static executable needs no Rust or Python on
clients. Python is used only by CI's PTY harness. Offline standalone installation
continues to use `install -m 0755 scat /catalog/scat/scat` after checksum
verification; it does not require RPM. `vc`, editors and external viewers are
optional site tools; the built-in viewer remains available when absent.

### Host controls

Use standard `/usr/bin`, `/usr/share` and `/etc` locations so the host's SELinux
file-context policy applies. On a real host check `matchpathcon /usr/bin/scat
/etc/scat/scat.yaml` and `ls -Z`, and restore expected labels with your site's
approved `restorecon` procedure. Validate access to catalog and cache locations
under enforcing SELinux with the intended user/domain. The RPM adds no SELinux
policy and never switches enforcement off.

RPM-managed files can enter fapolicyd's RPM trust database, depending on host
configuration; installing an unsigned RPM does not establish organizational
trust. Follow site signature policy, refresh the trust database using the
site-approved fapolicyd procedure, and test execution as an ordinary user.
Standalone binaries need explicit site trust onboarding. Never resolve denials
with broad allow rules or by disabling fapolicyd.

### Compatibility and release evidence

The intended support matrix is RHEL 8, 9 and 10 on x86_64 only. Other major
versions and architectures are unvalidated. CI runs the installed executable in
UBI 8/9/10 x86_64 userspaces (`ubi:latest` within each major); this is compatibility
evidence, not RHEL certification or proof of host support. Record the resolved
container image digest and `/etc/redhat-release` for each run. A release is
host-validated only after the following checks on actual RHEL hosts/VMs for
each claimed major version:

- Record exact RHEL minor version, architecture, kernel, RPM and package version,
  signed package and standalone executable SHA-256, optional tools and versions.
- Install, upgrade with modified config, uninstall, and inspect saved config,
  catalogs, cache ownership/permissions and completion/help locations.
- Run catalog info/search against the production schema with cache enabled,
  disabled and unavailable; verify read-only access and refresh after publication.
- Run `scat tui --terminal-info`, then PTY/SSH sessions with Unicode/full color
  and ASCII/reduced/mono modes; verify resize, exit/terminal restoration, missing
  and failing external viewer fallback. CI reuses `tests/support/tui_pty.py` for
  capability override fixtures and PTY protocol checks on the packaged binary.
- Under enforcing SELinux and enabled fapolicyd, verify ordinary-user CLI/TUI
  startup, trust and labels, review audit denials, and exercise actual NFS/autofs
  catalogs including cold mount, reconnect and publisher atomic replacement.
- Verify central cron publication and standalone deployment still operate with
  the same catalog; no Rust/Python setup is required on client hosts.

Container results cannot establish kernel, SELinux, fapolicyd, NFS, autofs,
physical-terminal or production SSH behavior. Keep a dated host-validation
record with release checksums and results; no real RHEL host validation has been
performed merely by adding this workflow.

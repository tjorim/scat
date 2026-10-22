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
installed. It has its own dependency graph and generated notices.

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

## Release notices

Release CI uses [cargo-about](https://github.com/EmbarkStudios/cargo-about) to
produce `THIRD_PARTY_NOTICES.txt` from Cargo.lock for each executable.
The small `about.toml` and `about.hbs` files configure license selection and
plain-text formatting; two entries retrieve missing MIT attribution from upstream. Build-only
and development dependencies are excluded. The Windows file also includes native
ONNX Runtime notices; Linux includes musl and GCC runtime notices.

Standalone artifacts contain LICENSE, NOTICE and THIRD_PARTY_NOTICES.txt beside
the executable. The RPM installs those three files under
`/usr/share/licenses/scat` as `%license`; the existing UBI smoke check verifies
that they are present and nonempty. The notice file links to the exact published crate sources; dependency sources
are not bundled.

## Distribution checklist

- [ ] Confirm the approved scope still covers new contributions and artwork,
      and review any third-party exclusions or changed dependency terms.
- [ ] Confirm LICENSE, Cargo/RPM metadata and public documentation agree.
- [ ] Confirm the existing cargo-deny checks and generated release notices pass.
      Preserve applicable attribution and satisfy any component-specific source
      availability obligations when redistributing binaries.
- [ ] Follow [RELEASING.md](RELEASING.md) and the catalog upgrade/rollback policy
      in [COMPATIBILITY.md](COMPATIBILITY.md).
- [ ] Confirm the existing reproducible-RPM and UBI 8/9/10 checks in
      [Rust Release Artifacts](../.github/workflows/rust-release.yml) pass.
      [VENDORING.md](VENDORING.md#optional-rpm-x86_64) covers packaging, signing
      and offline installation; its
      [host validation guidance](VENDORING.md#compatibility-and-release-evidence)
      distinguishes container evidence from RHEL host-support claims.
- [ ] Keep the notices with distributed executables and preserve the source
      revision, checksums and validation results. Regenerate RPM checksums after
      signing.

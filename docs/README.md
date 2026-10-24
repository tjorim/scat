# scat documentation

Guides for using, deploying and maintaining scat. These Markdown files are the
single source of truth: GitHub renders them in place, and the
[project website](https://tjorim.github.io/scat/docs/) publishes the same files
with shared navigation.

## Using scat

- [Quick start](QUICKSTART.md): setup, search, browsing and troubleshooting.
- [TUI controls and CLI coverage](TUI_OPTIONS.md): workflows and
  [terminal modes and remote sessions](TUI_OPTIONS.md#terminal-modes-and-remote-sessions).

## Deploying and operating

- [Deployment](VENDORING.md): standalone binary, RPM and RHEL validation.
- [Architecture](ARCHITECTURE.md): paths, read-only access and publication.
- [Compatibility](COMPATIBILITY.md): catalog, client and sidecar versions,
  upgrade and rollback.
- [vc contract](VC_CONTRACT.md): checkout and revision semantics.

## Maintaining scat

- [Releasing](RELEASING.md): release checklist.
- [Licensing and distribution](DISTRIBUTION.md): approved license, scope and
  artifact notices.
- [TUI workflow and worker contracts](TUI_WORKERS.md): search and detail worker
  lifecycle.
- [Website development and publishing](../site/README.md): Astro build, routing
  and CI for this site.

Annotated configuration examples: [scat.example.yaml](scat.example.yaml) and
[scat-mapping.example.yaml](scat-mapping.example.yaml). The
[contributing guide](../CONTRIBUTING.md) covers the toolchain and development
checks.

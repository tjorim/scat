# Website development and publishing

The website uses Astro to build a static landing page and the project
documentation with shared navigation. It needs no server runtime, client
framework, remote fonts or analytics. The landing page's fictional, browser-only
sample catalog is a small script that illustrates discovery; it does not
implement scat's FTS, ranking or advanced search semantics.

## Local development

From the repository root, use Node **24.14.0** (`nvm use`) and npm **10.9.8**.
Astro and the Markdown processor are pinned in `package.json`; commit changes to
`package-lock.json` together with dependency changes. CI explicitly installs the
pinned npm version before running `npm ci`.

```sh
npm install --global npm@10.9.8
npm ci
npm run dev
```

Open <http://127.0.0.1:4321/scat/>. Markdown, layout and CSS edits refresh in
development. After adding assets, restart the command to copy them again. To
inspect exactly what will be published:

```sh
npm test
npm run build
npm run check:site
npm run preview
```

Preview uses the same URL and repository prefix. `npm test` runs focused routing
tests and works immediately after `npm ci`, without a prior build. After
`npm run build`, `npm run check:site` validates all output pages, internal links,
fragments, local assets and legacy redirect targets. The old
`python3 scripts/build-site.py` entry point now delegates to the Astro build;
install dependencies first.

## Project structure

- `site/src/pages/index.astro`: the custom landing page, with its original content.
- `site/src/scripts/catalog.js`: the sample-catalog demo, bundled by Astro.
- `site/src/layouts/`: shared metadata, skip link and footer (`SiteLayout`) and the
  header navigation (`SiteNav`).
- `site/src/pages/docs/[...slug].astro`: documentation shell, navigation, table of
  contents and explicit GitHub source/edit links.
- `site/src/styles/global.css`: shared design and responsive documentation styles.
- `docs/*.md`: the canonical guides. `docs/README.md` is the documentation index.
  These stay in place so existing GitHub links and contributor habits still work.
- `site/README.md`: this guide, also rendered at `docs/website/`.
- `site/lib/paths.mjs` and `remark-links.mjs`: shared routing and Markdown URL
  rewriting, without changing the source Markdown.
- `scripts/prepare-site.mjs`: copies the `assets/` artwork, optional `docs/assets/`,
  `LICENSE` and `NOTICE` into generated `site/public/`, and adds `.nojekyll`.
- `_site/`: generated static output, ignored together with `site/public/` and
  Astro's cache. There are no hand-maintained HTML copies of the Markdown.

Edit `assets/icon.svg` and regenerate raster derivatives with
`uv run scripts/build-icon.py`.

## Markdown routes and links

`docs/README.md` renders at `/scat/docs/`; every other `docs/*.md` file renders at
`/scat/docs/<name>/`, using a lowercase name with `_` replaced by `-` (for
example `TUI_OPTIONS.md` becomes `/scat/docs/tui-options/`). The route discovers
all Markdown in that directory automatically. Add new pages to the documentation
navigation groups in `[...slug].astro`; unlisted pages are still published.

Astro's Unified Markdown processor supplies GitHub-flavored tables, fenced code
highlighting and heading IDs. A Remark transform resolves relative links from the
**source file's directory**, maps Markdown destinations to published routes, and
preserves query strings and anchors. Markdown links and reference definitions
share the same transform. Relative images under `assets/` or `docs/assets/` are
served from those paths within the prefix. Put documentation images in
`docs/assets/` and reference them as `assets/example.png`; use Markdown image
syntax so the transform sees them. Links to other repository files, including the
example YAML files, use GitHub. External URLs, mail links and fragment-only links
are left intact. Keep raw HTML URLs prefix-aware yourself; they do not pass
through the Markdown URL transform.

Astro generates static refresh pages for legacy `docs/*.md` paths and
`site/README.md`, since GitHub Pages cannot issue HTTP redirects. Existing GitHub
URLs remain valid source links.

## Repository prefix

`site/astro.config.mjs` sets `site: 'https://tjorim.github.io'`, static output and
trailing slashes. `site/lib/paths.mjs` defines `/scat/` once; layouts, landing
page links, Markdown transforms and compatibility routes all use it. Astro
prefixes its generated CSS and script assets. Do not add a second prefix manually.

## CI and deployment

In GitHub **Settings → Pages**, choose **GitHub Actions** as the source.
The Website workflow installs the pinned Node and npm toolchain, runs `npm ci`,
builds all pages and validates the output for both pull requests and pushes to
`main`. Its path filters include Markdown, site sources, assets, build scripts,
dependency manifests and the Node pin. It uploads `_site/` with
`upload-pages-artifact` and uses the Pages deploy job only on `main`. A manual
run on `main` can redeploy; manual runs on other branches build and test without
publishing. Expected public URL: <https://tjorim.github.io/scat/>.

The page links to successful CI builds because the project has no tagged
releases yet. When releases are published, update the install section and
artifact link together. Keep platform and licensing claims aligned with the
main README and [deployment guide](../docs/VENDORING.md).

The approved Apache-2.0 scope and artifact notice requirements are tracked in
[the distribution checklist](../docs/DISTRIBUTION.md). The build publishes the
root `LICENSE` and `NOTICE` alongside the site.

Search is deferred: the eleven documentation pages are reachable through the
grouped navigation and per-page contents list.

# scat website

A buildless static landing page with a fictional, browser-only sample catalog.
No package manager, remote fonts, analytics, service or Rust build is needed.
The demo illustrates discovery; it does not implement scat's FTS, ranking or
advanced search semantics. All assets are served locally.

## Preview

From the repository root:

```sh
python3 scripts/build-site.py
python3 -m http.server 8080 --directory _site --bind 127.0.0.1
```

Open http://127.0.0.1:8080. The build copies the shared icon assets from
`assets/`; edit `assets/icon.svg` and regenerate raster derivatives with
`uv run scripts/build-icon.py`. Generated `_site/` output is ignored by Git.

## Publish

In GitHub **Settings → Pages**, choose **GitHub Actions** as the source.
The Website workflow assembles an artifact for pull requests and deploys
main-branch changes after merge. A manual workflow run can redeploy it.
Expected public URL: https://tjorim.github.io/scat/.

The page links to successful CI builds because the project has no tagged
releases yet. When releases are published, update the install section and
artifact link together. Keep platform and licensing claims aligned with the
main README and deployment guide.

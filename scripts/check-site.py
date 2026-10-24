#!/usr/bin/env python3
"""Validate the actual prefixed static output without network dependencies."""
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urljoin, urlsplit
import re

ROOT = Path(__file__).resolve().parent.parent / "_site"
BASE = "/scat/"
ORIGIN = "https://tjorim.github.io"
MIN_PAGES = 14


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.ids = set()
        self.urls = []
        self.meta = {}
        self.canonical = None
        self.redirect = False
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        for key in ("href", "src"):
            if attrs.get(key):
                self.urls.append(attrs[key])
        if tag == "link" and attrs.get("rel") == "canonical":
            self.canonical = attrs.get("href")
        if tag == "meta":
            if attrs.get("http-equiv", "").lower() == "refresh":
                self.redirect = True
                self.urls.append(re.split(r"url=", attrs["content"], flags=re.I)[1])
            name = attrs.get("property") or attrs.get("name")
            if name:
                self.meta[name] = attrs.get("content", "")
            if name == "og:image":
                self.urls.append(attrs["content"])


pages = {file: Page(file.read_text()) for file in ROOT.rglob("*.html")}
assert len(pages) >= MIN_PAGES, "Build the landing page and all documentation pages first"
errors = []
for file, page in pages.items():
    relative = file.relative_to(ROOT).as_posix()
    route = relative.removesuffix("index.html")
    current = ORIGIN + BASE + route
    # Redirect stubs and the 404 page are not shared or indexed, so they carry no social metadata.
    if not page.redirect and page.meta.get("robots") != "noindex":
        if page.canonical != current:
            errors.append(f"{relative}: canonical is {page.canonical}, expected {current}")
        if page.meta.get("og:url") != current:
            errors.append(f"{relative}: og:url is {page.meta.get('og:url')}, expected {current}")
        for name in ("description", "og:title", "og:description", "og:image", "theme-color"):
            if not page.meta.get(name):
                errors.append(f"{relative}: missing <meta {name}>")
    for link in page.urls:
        target = urlsplit(urljoin(current, link))
        if target.netloc != "tjorim.github.io" or target.scheme not in ("http", "https"):
            continue
        if not target.path.startswith(BASE):
            errors.append(f"{relative}: URL escapes repository prefix: {link}")
            continue
        dest = ROOT / unquote(target.path[len(BASE):])
        if dest.is_dir():
            dest /= "index.html"
        if not dest.is_file():
            errors.append(f"{relative}: missing target: {link}")
        elif target.fragment and dest in pages and unquote(target.fragment) not in pages[dest].ids:
            errors.append(f"{relative}: missing anchor: {link}")
assert not errors, "\n".join(errors)
print(f"Checked {len(pages)} HTML pages: metadata, internal links, anchors, assets and redirects resolve under {BASE}")

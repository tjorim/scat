// Documentation navigation: groups of [Markdown source, label]. scripts/site-links.test.mjs
// fails when a published page is missing here, so new pages cannot be forgotten.
export const navigation = [
  ['Start here', [
    ['docs/QUICKSTART.md', 'Quick start'],
    ['docs/TUI_OPTIONS.md', 'TUI and CLI options'],
  ]],
  ['Deploy and operate', [
    ['docs/VENDORING.md', 'Deployment'],
    ['docs/ARCHITECTURE.md', 'Architecture'],
    ['docs/COMPATIBILITY.md', 'Compatibility'],
    ['docs/VC_CONTRACT.md', 'vc contract'],
  ]],
  ['Maintain', [
    ['docs/RELEASING.md', 'Releasing'],
    ['docs/DISTRIBUTION.md', 'Licensing and distribution'],
    ['docs/TUI_WORKERS.md', 'TUI workers'],
    ['site/README.md', 'Website development'],
  ]],
];

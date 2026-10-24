import test from 'node:test';
import assert from 'node:assert/strict';
import { documentRoute, listSources, rewriteUrl } from '../site/lib/paths.mjs';
import { describe } from '../site/lib/describe.mjs';
import { navigation } from '../site/lib/nav.mjs';
import remarkLinks from '../site/lib/remark-links.mjs';
import { fileURLToPath } from 'node:url';

test('Markdown routes use lowercase slugs and keep index and contributor locations', () => {
  assert.equal(documentRoute('docs/README.md'), 'docs/');
  assert.equal(documentRoute('docs/QUICKSTART.md'), 'docs/quickstart/');
  assert.equal(documentRoute('docs/TUI_OPTIONS.md'), 'docs/tui-options/');
  assert.equal(documentRoute('site/README.md'), 'docs/website/');
  assert.equal(documentRoute('README.md'), undefined);
  assert.equal(documentRoute('docs/scat.example.yaml'), undefined);
});

test('every published page is in the documentation navigation', () => {
  const navigable = navigation.flatMap(([, pages]) => pages.map(([source]) => source));
  const published = listSources().filter(source => source !== 'docs/README.md');
  assert.deepEqual([...navigable].sort(), published);
});

test('page descriptions are the first prose paragraph as plain text', () => {
  const markdown = '# Title\n\n![badge](x.svg)\n\n> note\n\nA [guide](a.md) with `code` and **bold**\ntext.\n\nSecond paragraph.';
  assert.equal(describe(markdown, 'fallback'), 'A guide with code and bold text.');
  assert.equal(describe('# Only a title\n\n- item', 'fallback'), 'fallback');
  const long = describe(`${'word '.repeat(60)}end`, 'fallback');
  assert.ok(long.length <= 160 && long.endsWith('…'));
});

test('source-relative links retain fragments and query strings under the base', () => {
  assert.equal(rewriteUrl('TUI_OPTIONS.md#terminal-modes-and-remote-sessions', 'docs/README.md'), '/scat/docs/tui-options/#terminal-modes-and-remote-sessions');
  assert.equal(rewriteUrl('../site/README.md#local-development', 'docs/README.md'), '/scat/docs/website/#local-development');
  assert.equal(rewriteUrl('RELEASING.md?from=index#tag', 'docs/COMPATIBILITY.md'), '/scat/docs/releasing/?from=index#tag');
  assert.equal(rewriteUrl('../docs/VENDORING.md', 'site/README.md'), '/scat/docs/vendoring/');
});

test('images and downloads use the prefixed static assets', () => {
  assert.equal(rewriteUrl('assets/diagram.png', 'docs/ARCHITECTURE.md'), '/scat/docs/assets/diagram.png');
  assert.equal(rewriteUrl('../assets/icon.svg', 'docs/README.md'), '/scat/assets/icon.svg');
  assert.equal(rewriteUrl('../LICENSE', 'docs/README.md'), '/scat/LICENSE');
  assert.equal(rewriteUrl('../NOTICE', 'docs/DISTRIBUTION.md'), '/scat/NOTICE');
});

test('external, mail and same-page links remain intact', () => {
  for (const url of ['https://example.com/page.md', 'mailto:team@example.com', '#setup', '//example.com/image.png']) {
    assert.equal(rewriteUrl(url, 'docs/README.md'), url);
  }
  assert.equal(rewriteUrl('../Cargo.toml', 'docs/README.md'), 'https://github.com/tjorim/scat/blob/main/Cargo.toml');
  assert.equal(rewriteUrl('../assets/README.md', 'docs/DISTRIBUTION.md'), 'https://github.com/tjorim/scat/blob/main/assets/README.md');
  assert.equal(rewriteUrl('scat.example.yaml', 'docs/QUICKSTART.md'), 'https://github.com/tjorim/scat/blob/main/docs/scat.example.yaml');
});

test('Remark handles direct links, images and reference definitions', () => {
  const nodes = ['link', 'image', 'definition'].map(type => ({ type, url: '../assets/icon.svg' }));
  remarkLinks()({ children: nodes }, { path: fileURLToPath(new URL('../docs/README.md', import.meta.url)) });
  for (const node of nodes) assert.equal(node.url, '/scat/assets/icon.svg');
});

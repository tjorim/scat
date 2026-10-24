import { defineConfig } from 'astro/config';
import { unified } from '@astrojs/markdown-remark';
import { base, documentRoute, listSources, origin, siteUrl } from './lib/paths.mjs';
import remarkLinks from './lib/remark-links.mjs';

// GitHub Pages cannot issue HTTP redirects. Astro emits static refresh pages from the
// Markdown file locations to the published routes, for links written against them.
const redirects = Object.fromEntries(listSources()
  .map(source => [`/${source}`, siteUrl(documentRoute(source))]));

export default defineConfig({
  site: origin,
  base,
  output: 'static',
  outDir: '../_site',
  trailingSlash: 'always',
  redirects,
  markdown: { processor: unified({ remarkPlugins: [remarkLinks] }) },
});

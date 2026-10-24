import { defineConfig } from 'astro/config';
import { unified } from '@astrojs/markdown-remark';
import { readdirSync } from 'node:fs';
import { base, documentRoute, siteUrl } from './lib/paths.mjs';
import remarkLinks from './lib/remark-links.mjs';

// GitHub Pages cannot issue HTTP redirects. Astro emits static refresh pages.
const redirects = Object.fromEntries(readdirSync(new URL('../docs/', import.meta.url))
  .filter(name => name.endsWith('.md'))
  .map(name => [`/docs/${name}`, siteUrl(documentRoute(`docs/${name}`))]));
redirects['/site/README.md'] = siteUrl('docs/website/');

export default defineConfig({
  site: 'https://tjorim.github.io',
  base,
  output: 'static',
  outDir: '../_site',
  trailingSlash: 'always',
  redirects,
  markdown: { processor: unified({ remarkPlugins: [remarkLinks] }) },
});

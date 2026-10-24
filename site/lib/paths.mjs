// Kept independent of Astro so config, Markdown transforms and tests share routing.
import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const base = '/scat/';
export const origin = 'https://tjorim.github.io';
export const repository = 'https://github.com/tjorim/scat';
export const siteUrl = (route = '') => base + route.replace(/^\/+/, '');

// Published at docs/website/; the only Markdown outside docs/ that is rendered.
const websiteSource = 'site/README.md';

export function documentRoute(source) {
  if (source === websiteSource) return 'docs/website/';
  if (!source.startsWith('docs/') || !source.endsWith('.md')) return undefined;
  const slug = source.slice(5, -3)
    .replace(/(^|\/)README$/, '$1')
    .toLowerCase()
    .replaceAll('_', '-');
  return `docs/${slug}${slug && !slug.endsWith('/') ? '/' : ''}`;
}

// Resolve relative URLs from the Markdown source, never from its published URL.
export function rewriteUrl(url, source) {
  if (!url || /^(?:[a-z][a-z\d+.-]*:|\/\/|#)/i.test(url)) return url;
  const [, pathname, suffix = ''] = url.match(/^([^?#]*)(.*)$/);
  if (!pathname) return url;
  const target = path.posix.normalize(pathname.startsWith('/')
    ? pathname.slice(1) : path.posix.join(path.posix.dirname(source), pathname));
  const route = documentRoute(target);
  if (route !== undefined) return siteUrl(route) + suffix;
  const published = target.startsWith('assets/') || target.startsWith('docs/assets/');
  if ((published && !target.endsWith('.md')) || ['LICENSE', 'NOTICE'].includes(target)) {
    return siteUrl(target) + suffix;
  }
  // Other repository files remain available from their source.
  return `${repository}/blob/main/${target}${suffix}`;
}

// Every published Markdown source in the working tree, as repository-relative paths.
export function listSources() {
  const root = fileURLToPath(new URL('../../docs/', import.meta.url));
  const docs = readdirSync(root, { recursive: true })
    .map(name => `docs/${name.replaceAll(path.sep, '/')}`)
    .filter(source => documentRoute(source) !== undefined);
  return [...docs, websiteSource].sort();
}

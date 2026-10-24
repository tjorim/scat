import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { rewriteUrl } from './paths.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
export default function remarkLinks() {
  return (tree, file) => {
    const source = path.relative(root, file.path).split(path.sep).join('/');
    function walk(node) {
      if (['link', 'image', 'definition'].includes(node.type)) {
        node.url = rewriteUrl(node.url, source);
      }
      node.children?.forEach(walk);
    }
    walk(tree);
  };
}

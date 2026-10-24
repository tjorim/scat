// A search-result summary for a documentation page: its first prose paragraph, as plain text.
export function describe(markdown, fallback, limit = 160) {
  const paragraph = markdown
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/^```[\s\S]*?^```/gm, '')
    .split(/\n\s*\n/)
    .map(block => block.trim())
    .find(block => block && !/^(#|>|[-*+] |\d+[.)] |\||<|!\[|\[[^\]]+\]:|---|===)/.test(block));
  if (!paragraph) return fallback;
  const text = paragraph
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]*)\]\[[^\]]*\]/g, '$1')
    .replace(/[*_`]+/g, '')
    .replace(/\s+/g, ' ')
    .trim();
  if (text.length <= limit) return text;
  return text.slice(0, limit - 1).replace(/\s+\S*$/, '').replace(/[.,;:]$/, '') + '…';
}

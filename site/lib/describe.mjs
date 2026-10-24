// A search-result summary for a documentation page: its first prose paragraph, as plain text.
export function describe(markdown, fallback, limit = 160) {
  // Repeat until stable so removing one comment cannot splice a new one together.
  let source = markdown;
  for (let previous = ''; previous !== source;) {
    previous = source;
    source = source.replace(/<!--[\s\S]*?-->/g, '');
  }
  const paragraph = source
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

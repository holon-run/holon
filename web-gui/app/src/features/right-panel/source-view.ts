/**
 * Source-view helpers that keep the plain-text fallback and the shiki
 * highlight output structurally identical, so the async highlight swap only
 * adds colors and never changes layout metrics.
 */

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Build a plain-text fallback that mirrors the shiki output skeleton
 * (`pre.shiki > code > span.line`) without separator newlines. The line spans
 * are rendered as block boxes (see `.file-browser-code > .shiki .line`), so
 * joining them without newlines keeps one span per visual line.
 */
export function buildPlainCodeHtml(content: string): string {
  const lines = content
    .split("\n")
    .map((line) => `<span class="line">${escapeHtml(line.replace(/\r$/, ""))}</span>`);
  return `<pre class="shiki file-browser-plain"><code>${lines.join("")}</code></pre>`;
}

/**
 * shiki joins its `span.line` elements with literal newlines. Once lines are
 * rendered as block boxes those separator newlines would render as extra
 * blank lines inside the `pre-wrap` container, so strip them. Newlines cannot
 * appear inside a line span's content (shiki splits by line first), which
 * keeps this replacement safe.
 */
export function normalizeShikiLineBreaks(html: string): string {
  return html.replace(/\n(?=<span class="line")/g, "");
}

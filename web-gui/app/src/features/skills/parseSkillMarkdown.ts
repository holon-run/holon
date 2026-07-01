const FRONTMATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;

/**
 * Strip the YAML frontmatter block (`---\n...\n---\n?`) from a SKILL.md
 * document so the trailing `---` does not render as an `<hr>` separator.
 * The body is returned verbatim otherwise.
 */
export function parseSkillMarkdown(raw: string): string {
  const match = raw.match(FRONTMATTER_RE);
  if (!match) return raw;
  return raw.slice(match[0].length).replace(/^\s+/, "");
}

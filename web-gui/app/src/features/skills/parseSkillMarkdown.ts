const FRONTMATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/;

/**
 * Strip the leading YAML frontmatter block (`---\n...\n---\n?`) from a
 * SKILL.md document so the closing `---` does not render as an `<hr>`
 * separator. Returns the remainder of the document unchanged.
 */
export function parseSkillMarkdown(raw: string): string {
  const match = raw.match(FRONTMATTER_RE);
  return match ? raw.slice(match[0].length) : raw;
}

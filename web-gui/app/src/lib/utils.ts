export function cn(...classes: Array<string | false | null | undefined>): string {
  return classes.filter(Boolean).join(" ");
}

// Width-aware truncation for compact one-line summaries: CJK and other wide
// glyphs count as double so mixed-language text cuts at a similar rendered
// width instead of a fixed character count.
const WIDE_GLYPH = /[\u1100-\u115F\u2E80-\uA4CF\uAC00-\uD7A3\uF900-\uFAFF\uFE30-\uFE6F\uFF00-\uFF60\uFFE0-\uFFE6]/;

export function truncateToWidth(text: string, maxWidth: number): string {
  if (maxWidth <= 0) {
    return "";
  }
  const chars = Array.from(text);
  let width = 0;
  for (let index = 0; index < chars.length; index += 1) {
    width += WIDE_GLYPH.test(chars[index]) ? 2 : 1;
    if (width > maxWidth) {
      return `${chars.slice(0, index).join("").trimEnd()}…`;
    }
  }
  return text;
}

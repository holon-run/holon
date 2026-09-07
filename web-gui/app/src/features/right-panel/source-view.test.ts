import { describe, expect, it } from "vitest";

import { buildPlainCodeHtml, escapeHtml, normalizeShikiLineBreaks } from "./source-view";

describe("escapeHtml", () => {
  it("escapes characters that would break out of text nodes", () => {
    expect(escapeHtml(`<b> & </b>`)).toBe("&lt;b&gt; &amp; &lt;/b&gt;");
  });
});

describe("buildPlainCodeHtml", () => {
  it("renders one span.line per source line without separator newlines", () => {
    const html = buildPlainCodeHtml("let a = 1\nlet b = 2");
    expect(html).toBe(
      '<pre class="shiki file-browser-plain"><code>' +
        '<span class="line">let a = 1</span>' +
        '<span class="line">let b = 2</span>' +
        "</code></pre>",
    );
    expect(html).not.toContain("\n");
  });

  it("escapes HTML-special content", () => {
    const html = buildPlainCodeHtml("<script>alert('x')</script>");
    expect(html).toContain("&lt;script&gt;alert('x')&lt;/script&gt;");
    expect(html).not.toContain("<script>");
  });

  it("normalizes CRLF line endings and keeps empty lines as empty spans", () => {
    const html = buildPlainCodeHtml("one\r\n\r\ntwo\r");
    expect(html).toBe(
      '<pre class="shiki file-browser-plain"><code>' +
        '<span class="line">one</span>' +
        '<span class="line"></span>' +
        '<span class="line">two</span>' +
        "</code></pre>",
    );
  });
});

describe("normalizeShikiLineBreaks", () => {
  const shikiFixture =
    '<pre class="shiki github-light" style="background-color:#fff"><code>' +
    '<span class="line"><span style="color:#D73A49">let</span> a = 1</span>\n' +
    '<span class="line"></span>\n' +
    '<span class="line">  b(2)</span></code></pre>';

  it("strips only the newlines shiki inserts between line spans", () => {
    expect(normalizeShikiLineBreaks(shikiFixture)).toBe(
      '<pre class="shiki github-light" style="background-color:#fff"><code>' +
        '<span class="line"><span style="color:#D73A49">let</span> a = 1</span>' +
        '<span class="line"></span>' +
        '<span class="line">  b(2)</span></code></pre>',
    );
  });

  it("keeps normalized output structurally identical to the plain fallback", () => {
    const normalized = normalizeShikiLineBreaks(shikiFixture);
    const lineCount = (html: string) => (html.match(/<span class="line[ "]/g) ?? []).length;
    expect(lineCount(normalized)).toBe(3);
    expect(normalized).not.toMatch(/\n(?=<span class="line")/);
    expect(normalized.startsWith('<pre class="shiki')).toBe(true);
    expect(buildPlainCodeHtml("x\n\ny")).toMatch(
      /^<pre class="shiki file-browser-plain"><code><span class="line">/,
    );
  });
});

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { MarkdownReference } from "./file-references/markdown";

vi.mock("./file-references/use-references", () => ({
  useFileIdentity: () => "scope",
  useReferences: (references: Map<string, MarkdownReference>) => ({
    results: new Map(
      [...references.keys()].map((key) => [
        key,
        {
          status: "unresolved" as const,
          reason: "ambiguous_root",
          message: "multiple equally specific execution roots contain the path",
        },
      ]),
    ),
    retry: () => {},
  }),
}));

import { MarkdownContent } from "./MarkdownContent";

describe("MarkdownContent GFM formatting", () => {
  it.each([
    "latency dropped from 1.5~2.5s to 0.4~0.6s after cache warmup (~4s)",
    "耗时 20~50ms，启动 8.8~9.3s，约 ~4s",
    "~plain text~",
  ])("preserves single tildes in %s", (text) => {
    const html = renderToStaticMarkup(<MarkdownContent text={text} />);

    expect(html).toContain(`<p>${text}</p>`);
    expect(html).not.toContain("<del>");
  });

  it("keeps explicit double-tilde strikethrough alongside numeric ranges", () => {
    const html = renderToStaticMarkup(
      <MarkdownContent text={"~~old~~ now 1.5~2.5s (~4s)"} />,
    );

    expect(html).toContain("<p><del>old</del> now 1.5~2.5s (~4s)</p>");
  });

  it("preserves tilde literals in inline and fenced code", () => {
    const html = renderToStaticMarkup(
      <MarkdownContent text={"`~literal~ ~~literal~~`\n\n```\n1.5~2.5s ~~literal~~\n```"} />,
    );

    expect(html).toContain("<code>~literal~ ~~literal~~</code>");
    expect(html).toContain("<pre><code>1.5~2.5s ~~literal~~\n</code></pre>");
    expect(html).not.toContain("<del>");
  });

  it("keeps GFM tables, task lists and autolinks", () => {
    const html = renderToStaticMarkup(
      <MarkdownContent text={"| latency |\n| --- |\n| 20~50ms |\n\n- [x] checked\n\nhttps://example.com"} />,
    );

    expect(html).toContain("<table>");
    expect(html).toContain("<td>20~50ms</td>");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain('checked=""');
    expect(html).toContain('href="https://example.com"');
  });
});

describe("MarkdownContent resolve degradation", () => {
  it("renders the raw reference text without an error state or retry when resolution fails", () => {
    const html = renderToStaticMarkup(
      <MarkdownContent text={"See `/tmp/repo/ambiguous/file.rs` for details."} />,
    );

    expect(html).toContain("/tmp/repo/ambiguous/file.rs");
    expect(html).toContain("file-reference-unresolved");
    expect(html).toContain("ambiguous_root: multiple equally specific execution roots contain the path");
    expect(html).not.toContain("<button");
    expect(html).not.toContain("<small");
  });
});

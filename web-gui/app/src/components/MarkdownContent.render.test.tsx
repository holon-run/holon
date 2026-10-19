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
  it("preserves invalid App targets as inline code without resolving them", () => {
    for (const path of ["/apps/a/b/%2e%2e/x", "/apps/a/b/%ZZ", "/apps/a/b/../x", "/apps/a/b\\x"]) {
      const html = renderToStaticMarkup(<MarkdownContent text={`\`${path}\``} />);
      expect(html).toContain(`<code>${path}</code>`);
      expect(html).not.toContain("<a");
      expect(html).not.toContain("file-reference");
    }
  });

  it("renders App links without a resolver result, preserving hrefs, titles and code", () => {
    const app = "/apps/tuptup-agent-dev/voice-audition-20261019/";
    const html = renderToStaticMarkup(<MarkdownContent text={[
      `[试听](${app}?mode=demo#part "direct")`,
      `[再次][app]\n\n[app]: ${app} "reference"`,
      `正文：${app}，结束。`,
      `\`${app}\`\n\n\`\`\`\n${app}\n\`\`\``,
      "[GUI](/files?workspace=ws&root=opaque%3Aone&path=x)",
      "[remote](//example.org/path) ![asset](/apps/a/b/asset.png)",
    ].join("\n\n")} />);
    expect(html).toContain(`title="direct" href="${app}?mode=demo#part"`);
    expect(html).toContain(`title="reference" href="${app}"`);
    expect(html).toContain(`href="${app}" rel="noopener noreferrer" target="_blank">${app}</a>，结束。`);
    expect(html).toContain(`<code>${app}</code>`);
    expect(html).toContain(`<pre><code>${app}\n</code></pre>`);
    expect(html).toContain('href="/files?workspace=ws&amp;root=opaque%3Aone&amp;path=x"');
    expect(html).toContain('href="//example.org/path"');
    expect(html).toContain('src="/apps/a/b/asset.png"');
    expect(html).not.toContain("file-reference-");
    expect(html).not.toContain("<iframe");
  });

  it("only autolinks whole bare App paths, retaining balanced parentheses and punctuation", () => {
    const html = renderToStaticMarkup(<MarkdownContent text={
      "(/apps/a/b/clip(1).mp3?q=x#part)。 /apps/a/b。 /apps/a/b!\n\n" +
      "prefix/apps/a/b /tmp/apps/a/b /apps/a /apps-evil/a/b\n\n" +
      "[**/apps/a/b**](https://example.org) [**`/apps/a/b`**](https://example.org)"
    } />);
    expect(html).toContain('href="/apps/a/b/clip(1).mp3?q=x#part"');
    expect(html.match(/href="\/apps\/a\/b"/g)).toHaveLength(2);
    expect(html.match(/<a /g)).toHaveLength(5);
    expect(html).toContain("prefix/apps/a/b /tmp/apps/a/b /apps/a /apps-evil/a/b");
    expect(html).not.toContain('href="#"');
  });

  it("never emits executable or confused App hrefs while retaining host file degradation", () => {
    const html = renderToStaticMarkup(<MarkdownContent text={
      "[js](javascript:alert(1)) [data](data:text/html,hello) " +
      "[confused](/apps/a/b/%2e%2e/c) [file](file:///apps/a/b) [host](/tmp/a.md)"
    } />);
    expect(html).not.toMatch(/href="(?:javascript:|data:|\/apps\/)/i);
    expect(html.match(/file-reference-unresolved/g)).toHaveLength(2);
  });

  it("keeps collection and rendering aligned for bare workspace URIs with single tildes", () => {
    const html = renderToStaticMarkup(
      <MarkdownContent text="see workspace://ws/a~b and 1~2s" />,
    );

    expect(html).toContain("workspace://ws/a~b");
    expect(html).toContain("file-reference-unresolved");
    expect(html).not.toContain("<del>");
  });

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

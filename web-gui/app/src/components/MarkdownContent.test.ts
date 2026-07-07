import { describe, expect, it } from "vitest";

import { markdownUrlTransform, parseWorkspaceImageRef, remarkWorkspaceAutolink, resolveWorkspaceRelativePath } from "./MarkdownContent";

describe("parseWorkspaceImageRef", () => {
  it("parses workspace image URIs", () => {
    expect(parseWorkspaceImageRef("workspace://ws_123/outputs/chart.png")).toEqual({
      workspaceId: "ws_123",
      path: "outputs/chart.png",
    });
    expect(parseWorkspaceImageRef("workspace://agent_home:holon-pm/media/inbox/screenshot.png")).toEqual({
      workspaceId: "agent_home:holon-pm",
      path: "media/inbox/screenshot.png",
    });
  });

  it("decodes path segments without accepting non-workspace URLs", () => {
    expect(parseWorkspaceImageRef("workspace://ws_123/out%20dir/chart%201.png")).toEqual({
      workspaceId: "ws_123",
      path: "out dir/chart 1.png",
    });
    expect(parseWorkspaceImageRef("https://example.com/chart.png")).toBeUndefined();
    expect(parseWorkspaceImageRef("workspace://agent_home:holon-pm/../secret.png")).toBeUndefined();
  });
});

describe("resolveWorkspaceRelativePath", () => {
  it("resolves markdown image paths relative to the current file", () => {
    expect(resolveWorkspaceRelativePath("docs/report.md", "images/chart.png")).toBe("docs/images/chart.png");
    expect(resolveWorkspaceRelativePath("docs/nested/report.md", "../images/chart%201.png")).toBe("docs/images/chart 1.png");
    expect(resolveWorkspaceRelativePath("docs/report.md", "/assets/logo.png")).toBe("assets/logo.png");
  });

  it("does not rewrite external or escaping image URLs", () => {
    expect(resolveWorkspaceRelativePath("docs/report.md", "https://example.com/chart.png")).toBeUndefined();
    expect(resolveWorkspaceRelativePath("docs/report.md", "data:image/png;base64,abc")).toBeUndefined();
    expect(resolveWorkspaceRelativePath("report.md", "../secret.png")).toBeUndefined();
  });
});

describe("markdownUrlTransform", () => {
  it("keeps workspace image sources while preserving default URL sanitization", () => {
    const src = "workspace://agent_home:holon-pm/media/inbox/screenshot.png";

    expect(markdownUrlTransform(src, "src")).toBe(src);
    expect(markdownUrlTransform(src, "href")).toBe(src);
    expect(markdownUrlTransform("javascript:alert(1)", "src")).toBe("");
    expect(markdownUrlTransform("https://example.com/chart.png", "src")).toBe("https://example.com/chart.png");
  });
});

describe("remarkWorkspaceAutolink", () => {
  function makeTextTree(text: string) {
    return {
      type: "root",
      children: [
        { type: "paragraph", children: [{ type: "text", value: text }] },
      ],
    };
  }

  function runPlugin(text: string) {
    const tree = makeTextTree(text);
    remarkWorkspaceAutolink()(tree as any);
    return (tree as any).children[0].children;
  }

  it("autolinks bare workspace:// URLs in text", () => {
    const nodes = runPlugin("See workspace://agent_home:holon-test/notes/demo.pdf for details");
    expect(nodes).toHaveLength(3);
    expect(nodes[0]).toEqual({ type: "text", value: "See " });
    expect(nodes[1]).toEqual({
      type: "link",
      url: "workspace://agent_home:holon-test/notes/demo.pdf",
      children: [{ type: "text", value: "workspace://agent_home:holon-test/notes/demo.pdf" }],
    });
    expect(nodes[2]).toEqual({ type: "text", value: " for details" });
  });

  it("does not autolink inside link text nodes", () => {
    const tree = {
      type: "root",
      children: [
        { type: "paragraph", children: [
          { type: "link", url: "https://example.com", children: [
            { type: "text", value: "workspace://agent_home:holon-test/notes/demo.pdf" },
          ] },
        ] },
      ],
    };
    remarkWorkspaceAutolink()(tree as any);
    const linkNode = (tree as any).children[0].children[0];
    expect(linkNode.type).toBe("link");
    expect(linkNode.children[0].value).toBe("workspace://agent_home:holon-test/notes/demo.pdf");
  });

  it("does not modify text without workspace:// URLs", () => {
    const nodes = runPlugin("Just a regular http://example.com link");
    expect(nodes).toEqual([{ type: "text", value: "Just a regular http://example.com link" }]);
  });
});

import { describe, expect, it } from "vitest";

import { markdownUrlTransform, parseWorkspaceImageRef, resolveWorkspaceRelativePath } from "./MarkdownContent";

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

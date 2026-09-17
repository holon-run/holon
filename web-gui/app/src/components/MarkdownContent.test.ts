import { describe, expect, it } from "vitest";

import {
  parseWorkspaceImageRef,
  safeCitation,
  stripOpenAiCitationSentinels,
} from "./MarkdownContent";

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

  it("extracts the ?root= execution-root token and keeps it opaque", () => {
    expect(parseWorkspaceImageRef("workspace://ws_1/docs/a.png?root=git_worktree_root:ws_1:/tmp/wt")).toEqual({
      workspaceId: "ws_1",
      path: "docs/a.png",
      executionRootId: "git_worktree_root:ws_1:/tmp/wt",
    });
    expect(parseWorkspaceImageRef("workspace://ws_1/docs/a.png?root=git%5Fworktree%3Aws")).toEqual({
      workspaceId: "ws_1",
      path: "docs/a.png",
      executionRootId: "git_worktree:ws",
    });
    expect(parseWorkspaceImageRef("workspace://ws_1/docs/a.png#frag")).toEqual({
      workspaceId: "ws_1",
      path: "docs/a.png",
    });
    expect(parseWorkspaceImageRef("workspace://ws_1/docs/a.png?root=one&extra=two")).toBeUndefined();
    expect(parseWorkspaceImageRef("workspace://ws_1/docs/a.png?root=one&root=two")).toBeUndefined();
  });
});

describe("OpenAI citations", () => {
  it("strips complete and malformed sentinels without removing following text", () => {
    expect(
      stripOpenAiCitationSentinels(
        "Before \uE200cite\uE202turn0search0\uE202turn0search1\uE201 after",
      ),
    ).toBe("Before  after");
    expect(
      stripOpenAiCitationSentinels(
        "Before \uE200cite\uE202turn0search0\uE202turn0search1 normal answer",
      ),
    ).toBe("Before  normal answer");
  });

  it("accepts only absolute http and https links", () => {
    expect(safeCitation({ url: "https://example.com/path", title: " Example " })).toEqual({
      url: "https://example.com/path",
      title: "Example",
    });
    expect(safeCitation({ url: "https://example.com/path" })).toEqual({
      url: "https://example.com/path",
      title: "example.com",
    });
    expect(safeCitation({ url: "javascript:alert(1)", title: "Unsafe" })).toBeUndefined();
    expect(safeCitation({ url: "/relative", title: "Relative" })).toBeUndefined();
  });
});

import { describe, expect, it } from "vitest";

import { parseWorkspaceImageRef } from "./MarkdownContent";

describe("parseWorkspaceImageRef", () => {
  it("parses workspace image URIs", () => {
    expect(parseWorkspaceImageRef("workspace://ws_123/outputs/chart.png")).toEqual({
      workspaceId: "ws_123",
      path: "outputs/chart.png",
    });
  });

  it("decodes path segments without accepting non-workspace URLs", () => {
    expect(parseWorkspaceImageRef("workspace://ws_123/out%20dir/chart%201.png")).toEqual({
      workspaceId: "ws_123",
      path: "out dir/chart 1.png",
    });
    expect(parseWorkspaceImageRef("https://example.com/chart.png")).toBeUndefined();
  });
});

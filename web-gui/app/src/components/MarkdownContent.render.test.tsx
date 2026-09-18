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

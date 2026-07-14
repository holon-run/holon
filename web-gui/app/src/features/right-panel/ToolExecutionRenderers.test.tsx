import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import "../../i18n/config";

import { ToolExecutionContent } from "./ToolExecutionRenderers";
import type { RuntimeToolExecutionRecord } from "../../runtime/types";

function renderTool(record: RuntimeToolExecutionRecord): string {
  return renderToStaticMarkup(<ToolExecutionContent record={record} />);
}

describe("ToolExecutionContent", () => {
  it("renders ViewImage details with path, dimensions, and visual observation", () => {
    const html = renderTool({
      tool_name: "ViewImage",
      input: { path: "media/generated/tool-timeline-smoke.png" },
      output: {
        envelope: {
          result: {
            visual_reference: {
              id: "img_test123",
              path: "media/generated/tool-timeline-smoke.png",
              mime: "image/png",
              byte_count: 2048,
              sha256: "abc123",
              size: { width: 1024, height: 768 },
            },
            observation: {
              summary: "A timeline screenshot.",
              generated_by: { provider: "openai", model: "gpt-4o" },
            },
          },
        },
      },
    });

    expect(html).toContain("Path");
    expect(html).toContain("media/generated/tool-timeline-smoke.png");
    expect(html).toContain("1024×768");
    expect(html).toContain("A timeline screenshot.");
    expect(html).toContain("image/png");
    expect(html).toContain("openai/gpt-4o");
  });

  it("renders GenerateImage details with prompt and generated image URI", () => {
    const html = renderTool({
      tool_name: "GenerateImage",
      input: { name: "hero", size: "1536x1024", prompt: "A Holon agent dashboard" },
      output: {
        envelope: {
          result: {
            image_uri: "workspace://agent_home/holon-dev/media/generated/hero.png",
            output_format: "png",
          },
        },
      },
    });

    expect(html).toContain("hero");
    expect(html).toContain("1536x1024");
    expect(html).toContain("workspace://agent_home/holon-dev/media/generated/hero.png");
    expect(html).toContain("A Holon agent dashboard");
  });

  it("renders WebSearch details as a structured result list", () => {
    const html = renderTool({
      tool_name: "WebSearch",
      input: { query: "rust runtime" },
      output: {
        envelope: {
          result: {
            query: "rust runtime",
            provider: "brave",
            mode: "single",
            results: [{ title: "Tokio", url: "https://tokio.rs", source: "tokio.rs", snippet: "Async runtime" }],
          },
        },
      },
    });

    expect(html).toContain("Query");
    expect(html).toContain("rust runtime");
    expect(html).toContain("brave");
    expect(html).toContain("1 found");
    expect(html).toContain("tool-detail-result-index");
    expect(html).toContain("https://tokio.rs");
    expect(html).toContain("Async runtime");
  });

  it("renders WebFetch details with URL metadata and content", () => {
    const html = renderTool({
      tool_name: "WebFetch",
      input: { url: "https://example.com" },
      output: {
        envelope: {
          result: {
            url: "https://example.com",
            final_url: "https://www.example.com",
            status: 200,
            content_type: "text/html",
            bytes_read: 2048,
            truncated: true,
            text: "# Example\n\nFetched content.",
          },
        },
      },
    });

    expect(html).toContain("https://example.com");
    expect(html).toContain("https://www.example.com");
    expect(html).toContain("200");
    expect(html).toContain("text/html");
    expect(html).toContain("2048");
    expect(html).toContain("# Example");
    expect(html).toContain("Fetched content.");
  });

  it("renders MemorySearch details with source refs and previews", () => {
    const html = renderTool({
      tool_name: "MemorySearch",
      input: { query: "device oauth" },
      output: {
        envelope: {
          result: {
            query: "device oauth",
            results: [{ source_ref: "turn:abc", score: 0.95, preview: "Device OAuth implementation notes" }],
          },
        },
      },
    });

    expect(html).toContain("device oauth");
    expect(html).toContain("1 found");
    expect(html).toContain("1. turn:abc");
    expect(html).toContain("score: 0.95");
    expect(html).toContain("Device OAuth implementation notes");
  });

  it("renders MemoryGet details with source ref and fetched content", () => {
    const html = renderTool({
      tool_name: "MemoryGet",
      input: { source_ref: "turn:abc" },
      output: {
        envelope: {
          result: {
            source_ref: "turn:abc",
            truncated: false,
            content: "Full memory content",
          },
        },
      },
    });

    expect(html).toContain("turn:abc");
    expect(html).toContain("Full memory content");
  });

  it("renders UseWorkspace details with workspace path and file browser link", () => {
    const html = renderTool({
      tool_name: "UseWorkspace",
      input: { path: "/home/user/project", mode: "direct" },
      output: {
        envelope: {
          result: {
            workspace_id: "ws_abc123",
            workspace_anchor: "/home/user/project",
            execution_root_id: "root-xyz",
            execution_root: "/home/user/project",
            cwd: "/home/user/project",
            mode: "direct",
            projection_kind: "canonical_root",
          },
        },
      },
    });

    expect(html).toContain("ws_abc123");
    expect(html).toContain("/home/user/project");
    expect(html).toContain("direct");
    expect(html).toContain("canonical_root");
  });

  it("renders input params and error for failed MemoryGet call", () => {
    const html = renderTool({
      tool_name: "MemoryGet",
      status: "error",
      input: { source_ref: "memory:nonexistent_ref_12345" },
      output: {
        is_error: true,
        error: {
          kind: "not_found",
          message: "No memory content found for source_ref 'nonexistent_ref_12345'",
          recovery_hint: "Use MemorySearch to find valid source_ref values.",
          retryable: false,
        },
      },
    });

    // Input parameters should be visible
    expect(html).toContain("Input");
    expect(html).toContain("nonexistent_ref_12345");
    // Error message should be visible
    expect(html).toContain("Error");
    expect(html).toContain("No memory content found");
    // Error kind and recovery hint should be included
    expect(html).toContain("not_found");
    expect(html).toContain("Hint:");
    expect(html).toContain("MemorySearch");
  });
});

import { describe, expect, it } from "vitest";
import { toolExecutionPresentation } from "./tool-execution-presentation";

describe("canonical tool execution presentation", () => {
  it("prefers the runtime's command display and keeps output in the inspector", () => {
    expect(toolExecutionPresentation({
      tool_name: "ExecCommand", status: "success", input: { cmd: "curl --token secret" },
      output: { envelope: { result: { cmd_display: "curl --token [redacted]", stdout: "full output", exit_status: 0 } } },
      duration_ms: 1240,
    })).toEqual({ text: "curl --token [redacted]", toolName: "ExecCommand", status: "success", durationMs: 1240, command: true });
  });

  it("summarizes batch commands and patch paths using the existing tool projections", () => {
    expect(toolExecutionPresentation({ tool_name: "ExecCommandBatch", input: { items: [{ cmd: "pwd" }, { cmd: "git status" }] } }).text)
      .toBe("pwd\ngit status");
    expect(toolExecutionPresentation({ tool_name: "ApplyPatch", output: { envelope: { result: {
      changed_paths: ["src/main.rs", "README.md"], diff: "full patch text",
    } } } }).text).toBe("2 files · src/main.rs, README.md");
  });

  it("shows query, URL, image and work item context instead of raw arguments", () => {
    expect(toolExecutionPresentation({ tool_name: "WebSearch", input: { query: "Holon runtime" } }).text).toContain("Holon runtime");
    expect(toolExecutionPresentation({ tool_name: "XSearch", input: { query: "Holon runtime" }, summary: "Search completed" }).text).toBe("XSearch · Holon runtime");
    expect(toolExecutionPresentation({ tool_name: "WebFetch", input: { url: "https://example.com/docs" } }).text).toContain("https://example.com/docs");
    expect(toolExecutionPresentation({ tool_name: "ViewImage", input: { path: "/tmp/layout.png" } }).text).toContain("layout.png");
    expect(toolExecutionPresentation({ tool_name: "GetWorkItem", input: { work_item_id: "work-123" } }).text).toContain("work-123");
    expect(toolExecutionPresentation({ tool_name: "CustomTool", input: { token: "secret", body: "large content" }, summary: "Custom action" }).text).toBe("Custom action");
  });

  it("bounds long commands and suppresses meaningless read-tool durations", () => {
    const text = toolExecutionPresentation({ tool_name: "ExecCommand", input: { cmd: "x".repeat(1000) } }).text;
    expect(text).toHaveLength(240);
    expect(text.endsWith("…")).toBe(true);
    expect(toolExecutionPresentation({ tool_name: "TaskStatus", input: { task_id: "task-1" }, duration_ms: 20 }).durationMs).toBeUndefined();
  });
});

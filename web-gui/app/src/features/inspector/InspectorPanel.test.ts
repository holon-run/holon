import { describe, expect, it } from "vitest";

import { formatToolExecutionDetail } from "./InspectorPanel";

describe("formatToolExecutionDetail", () => {
  it("extracts readable command output from tool execution records", () => {
    expect(
      formatToolExecutionDetail({
        tool_name: "ExecCommand",
        status: "success",
        summary: "command completed",
        input: { cmd: "cargo test" },
        output: {
          exit_status: 0,
          stdout_preview: "test result: ok",
          stderr_preview: "",
        },
      }),
    ).toEqual({
      tone: "output",
      text: ["Summary:\ncommand completed", "Command:\ncargo test", "Stdout:\ntest result: ok", "Exit:\n0"].join("\n\n"),
    });
  });

  it("falls back to result and error fields for legacy records", () => {
    expect(
      formatToolExecutionDetail({
        tool_name: "ExecCommand",
        status: "error",
        result: {
          stderr: "failed",
        },
        error: "command exploded",
      }).text,
    ).toContain("Stderr:\nfailed");
  });
});

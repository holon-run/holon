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

  it("extracts command output from tool result envelopes", () => {
    expect(
      formatToolExecutionDetail({
        tool_name: "ExecCommand",
        status: "success",
        input: { cmd: "npm test" },
        output: {
          envelope: {
            result: {
              exit_status: 0,
              stdout_preview: "tests passed",
              summary_text: "command exited with status 0",
            },
          },
        },
      }).text,
    ).toContain("Stdout:\ntests passed");
  });

  it("extracts command output from batch item result envelopes", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "ExecCommandBatch",
      status: "success",
      input: {
        items: [{ cmd: "git status" }, { cmd: "rg TODO src" }],
      },
      output: {
        envelope: {
          result: {
            items: [
              {
                index: 1,
                result: {
                  exit_status: 0,
                  stdout_preview: "## main",
                },
              },
              {
                cmd: "rg TODO src",
                index: 2,
                result: {
                  exit_status: 1,
                  stderr_preview: "no matches",
                },
              },
            ],
          },
        },
      },
    });

    expect(detail.text).toContain("Batch item 1:\nCommand:\ngit status\n\nStdout:\n## main");
    expect(detail.text).toContain("Batch item 2:\nCommand:\nrg TODO src\n\nStderr:\nno matches");
  });
});

import { describe, expect, it } from "vitest";

import { normalizeTaskDetailContent } from "./TaskDetailRenderers";

describe("normalizeTaskDetailContent", () => {
  it("prefers split stdout/stderr over duplicate raw output for command tasks", () => {
    expect(
      normalizeTaskDetailContent(
        {
          id: "task_1",
          kind: "command_task",
          status: "completed",
          summary: "cargo test",
          command: "cargo test",
        },
        {
          summary: "cargo test",
          task: {
            summary: "cargo test",
            result_summary: "command exited with status 0",
            output_preview: "tests passed",
            exit_status: 0,
          },
          stdout: "tests passed",
          output: "tests passed",
        },
      ),
    ).toMatchObject({
      summary: "cargo test",
      command: "cargo test",
      result: "command exited with status 0",
      stdout: "tests passed",
      rawOutput: "",
      exitStatus: 0,
    });
  });

  it("falls back to preview output when no full output or split streams exist", () => {
    expect(
      normalizeTaskDetailContent(
        {
          id: "task_2",
          kind: "command_task",
          status: "running",
          summary: "npm run dev",
        },
        {
          task: {
            summary: "npm run dev",
            output_preview: "server booting",
            output_truncated: true,
          },
        },
      ),
    ).toMatchObject({
      summary: "npm run dev",
      rawOutput: "server booting",
      rawOutputTruncated: true,
    });
  });

  it("does not repeat the header summary as a result block", () => {
    expect(
      normalizeTaskDetailContent(
        {
          id: "task_3",
          kind: "command_task",
          status: "completed",
          summary: "npm run build",
        },
        {
          summary: "npm run build",
          task: {
            summary: "npm run build",
          },
        },
      ).result,
    ).toBe("");
  });
});

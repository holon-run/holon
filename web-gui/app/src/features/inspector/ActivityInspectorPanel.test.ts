import { beforeAll, describe, expect, it } from "vitest";
import i18next from "i18next";
import en from "../../i18n/resources/en";

import { formatToolExecutionDetail } from "./ActivityInspectorPanel";

beforeAll(() => {
  if (!i18next.isInitialized) {
    i18next.init({ lng: "en", resources: { en: { translation: en } } });
  }
});

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
      text: ["Command:\ncargo test", "Stdout:\ntest result: ok", "Result:\ncommand completed", "Exit:\n0"].join("\n\n"),
    });
  });

  it("shows ApplyPatch input as the full patch when available", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "ApplyPatch",
      status: "success",
      summary: "updated files",
      input: "*** Begin Patch\n*** Update File: app.ts\n@@\n-old\n+new\n*** End Patch\n",
      output: {
        envelope: {
          result: {
            changed_files: [{ path: "app.ts", action: "M", diff_preview: "@@\n-old\n+new" }],
          },
        },
      },
    });

    expect(detail.text).toContain("Changed files:\nM · app.ts");
    expect(detail.text).toContain("Patch:\n*** Begin Patch");
    expect(detail.text).not.toContain("Patch preview");
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

  it("formats ListTasks as an active task list", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "ListTasks",
      status: "success",
      output: {
        envelope: {
          result: {
            total_active: 1,
            returned: 1,
            tasks: [
              {
                task_id: "task_1",
                kind: "command_task",
                status: "running",
                summary: "Run command: npm run dev",
                command: { cmd_preview: "npm run dev" },
              },
            ],
          },
        },
      },
    });

    expect(detail.text).toContain("Tasks:\nRun command: npm run dev · running · command_task · task_1");
    expect(detail.text).toContain("Total active:\n1");
  });

  it("formats ListWorkItems as a readable work item list", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "ListWorkItems",
      status: "success",
      input: { filter: "current" },
      output: {
        list_work_items_result: {
          total: 1,
          returned: 1,
          work_items: [
            {
              id: "work_1",
              objective: "Improve inspector details",
              lifecycle: "open",
              plan_status: "ready",
              current: true,
            },
          ],
        },
      },
    });

    expect(detail.text).toContain("Filter:\ncurrent");
    expect(detail.text).toContain("Work items:\nImprove inspector details · open · ready · current · work_1");
  });

  it("formats single work item tool records with state, plan, and todo context", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "GetWorkItem",
      status: "success",
      output: {
        get_work_item_result: {
          work_item: {
            id: "work_2",
            objective: "Track global stream",
            lifecycle: "open",
            plan_status: "needs_input",
            plan_artifact: { path: "/agent/work-items/work_2/plan.md" },
            todo_list: [{ state: "pending", text: "Decide API shape" }],
          },
        },
      },
    });

    expect(detail.text).toContain("Objective:\nTrack global stream");
    expect(detail.text).toContain("State:\nopen · needs_input");
    expect(detail.text).toContain("Todo:\npending · Decide API shape");
  });

  it("formats WebSearch tool results with query and structured result list", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "WebSearch",
      status: "success",
      input: {
        query: "rust async runtime",
        max_results: 5,
      },
      output: {
        envelope: {
          result: {
            query: "rust async runtime",
            provider: "brave",
            mode: "single",
            results: [
              {
                title: "Tokio — Async Runtime",
                url: "https://tokio.rs",
                source: "tokio.rs",
                snippet: "Tokio is an async runtime for writing reliable network applications.",
              },
              {
                title: "async-std",
                url: "https://async.rs",
                snippet: "A small and fast async runtime.",
              },
            ],
          },
        },
      },
    });

    expect(detail.tone).toBe("output");
    expect(detail.text).toContain("Query:\nrust async runtime");
    expect(detail.text).toContain("Results:\n2 found");
    expect(detail.text).toContain("1. Tokio — Async Runtime");
    expect(detail.text).toContain("https://tokio.rs");
    expect(detail.text).toContain("(tokio.rs)");
    expect(detail.text).toContain("2. async-std");
  });

  it("formats WebFetch tool results with URL and content preview", () => {
    const detail = formatToolExecutionDetail({
      tool_name: "WebFetch",
      status: "success",
      input: {
        url: "https://example.com/article",
        max_chars: 10000,
      },
      output: {
        envelope: {
          result: {
            url: "https://example.com/article",
            final_url: "https://example.com/article",
            status: 200,
            content_type: "text/html",
            bytes_read: 2048,
            truncated: false,
            text: "# Example Article\n\nThis is the content of the article.",
          },
        },
      },
    });

    expect(detail.tone).toBe("output");
    expect(detail.text).toContain("URL:\nhttps://example.com/article");
    expect(detail.text).toContain("Status:\n200");
    expect(detail.text).toContain("Content:\n# Example Article");
  });
});

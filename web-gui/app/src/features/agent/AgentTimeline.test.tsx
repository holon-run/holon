import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import i18next from "i18next";

import "../../i18n";
import type { AgentTimelineActivity } from "../../runtime/types";
import { ActivityTrail, BriefHydrationStatus } from "./AgentTimeline";

function activity(overrides: Partial<AgentTimelineActivity> = {}): AgentTimelineActivity {
  return {
    id: "activity-1",
    kind: "tool",
    label: "Command completed",
    body: "npm test with a deliberately long command summary",
    timestamp: "2026-07-17T10:00:00Z",
    meta: "tool_executed",
    minDisplayLevel: "verbose",
    sourceIds: ["event-1"],
    ...overrides,
  };
}

describe("ActivityTrail", () => {
  it("renders fixed execution metadata separately from the truncatable summary", () => {
    const markup = renderToStaticMarkup(
      <ActivityTrail
        activities={[
          activity({
            executionMeta: {
              outcome: "completed",
              exitStatus: 0,
              durationMs: 320,
              outputTruncated: true,
              taskId: "task_1234567890",
            },
          }),
        ]}
        displayLevel="verbose"
        onOpenInspector={() => undefined}
        onInspectActivity={() => undefined}
      />,
    );

    expect(markup).toContain('class="activity-body"');
    expect(markup).toContain('class="activity-tail"');
    expect(markup).toContain("Exit status 0");
    expect(markup).toContain("Duration 320ms");
    expect(markup).toContain("Output truncated");
    expect(markup).toContain("task_1234567…");
  });

  it("renders semantic task status steps and keeps the active running step spinning", () => {
    const markup = renderToStaticMarkup(
      <ActivityTrail
        activities={[
          activity({
            executionMeta: { outcome: "running" },
            statusTrail: [{ status: "queued" }, { status: "running" }],
          }),
        ]}
        displayLevel="verbose"
        onOpenInspector={() => undefined}
        onInspectActivity={() => undefined}
      />,
    );

    expect(markup).toContain("Queued → Running");
    expect(markup).toContain('data-status="queued"');
    expect(markup).toContain('data-status="running"');
    expect(markup).toContain("is-spinning");
    expect(markup).not.toContain("Task queued");
  });
});

describe("BriefHydrationStatus", () => {
  it("renders a localized loading state without a retry button", async () => {
    await i18next.changeLanguage("en");
    const markup = renderToStaticMarkup(
      <BriefHydrationStatus
        hydration={{ briefId: "brief-1", status: "loading", attempt: 1 }}
        onRetry={() => undefined}
      />,
    );

    expect(markup).toContain("Loading full result");
    expect(markup).not.toContain("<button");
  });

  it("renders localized retry actions for failed and not found states", async () => {
    await i18next.changeLanguage("zh-CN");
    const failed = renderToStaticMarkup(
      <BriefHydrationStatus
        hydration={{ briefId: "brief-1", status: "failed", attempt: 3 }}
        onRetry={() => undefined}
      />,
    );
    const notFound = renderToStaticMarkup(
      <BriefHydrationStatus
        hydration={{ briefId: "brief-2", status: "not_found", attempt: 1 }}
        onRetry={() => undefined}
      />,
    );

    expect(failed).toContain("完整结果加载失败");
    expect(failed).toContain("重试");
    expect(notFound).toContain("未找到完整结果");
    expect(notFound).toContain("重试");
    await i18next.changeLanguage("en");
  });
});

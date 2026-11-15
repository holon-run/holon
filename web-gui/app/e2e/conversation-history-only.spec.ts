import { expect, test, type Page } from "@playwright/test";
import type { ConversationSummaryResponse } from "@holon/conversation-sdk";

import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const historyText = "Cold independently admitted history.";
const pending = {
  status: 503,
  json: { code: "agent_reconciliation_pending", error: "Reconciliation pending", retryable: true },
};

async function seedHistory(request: { post: (url: string, options: { data: unknown }) => Promise<unknown> }, session: string) {
  const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
  // The fixture server answers detail/state only for visible agents; roster
  // presence is not admission (State 503 below withholds it).
  await request.post(control("/__e2e__/configure"), { data: {
    visibleAgentIds: [agentId],
    briefsById: { history: {
      id: "history", agent_id: agentId, workspace_id: "holon", kind: "result",
      text: historyText, created_at: "2026-11-14T08:00:00Z",
      content_source: { kind: "inline" }, attachments: null,
      related_message_id: null, related_task_id: null,
    } },
  } });
  await request.post(control("/__e2e__/conversation"), { data: {
    agentId, turns: [{
      turn_id: "history-turn", key: { turn_id: "history-turn", turn_index: 1 }, revision: 1,
      presentation_class: "operator", inputs: [{ message_id: "history-input", preview: "Cold history question" }],
      execution: { kind: "active" }, result: { kind: "available" }, settled: false,
      attention: null, detail_coverage: { kind: "complete" }, brief_ids: ["history"],
    }],
  } });
}

async function trackRequests(page: Page) {
  const mutations: string[] = [];
  const rawEvents: string[] = [];
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (!url.pathname.startsWith("/api/")) return;
    // Locate is a read-only POST, not a business mutation.
    const locateRead = request.method() === "POST" && url.pathname === `/api/agents/${agentId}/conversation/locate`;
    if (!["GET", "HEAD", "OPTIONS"].includes(request.method()) && !locateRead) mutations.push(`${request.method()} ${url.pathname}`);
    if (/\/events(?:\/stream)?$/.test(url.pathname)) rawEvents.push(url.pathname);
  });
  await page.route("**/api/auth/session/me", (route) => route.fulfill({
    json: { user_id: "history-only-user", auth_method: "static_token" },
  }));
  await page.route(`**/api/agents/${agentId}/events*`, (route) => route.fulfill({
    status: 500, json: { error: "Raw events unavailable" },
  }));
  return { mutations, rawEvents };
}

test("cold State 503 admits only independent history; controls and callbacks are closed, stream failure retains history, recovery does not replay", async ({
  page, context, request,
}, info) => {
  const session = sessionFor(info, "cold-history");
  await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
  await seedHistory(request, session);
  const { mutations, rawEvents } = await trackRequests(page);
  let unavailable = true;
  await page.route(`**/api/agents/${agentId}/state`, (route) =>
    unavailable ? route.fulfill(pending) : route.continue());
  let streamAttempts = 0;
  await page.route(`**/api/agents/${agentId}/conversation/stream*`, (route) => {
    streamAttempts += 1;
    return route.fulfill({ status: 503, json: { error: "Stream unavailable", retryable: true } });
  });
  await page.goto(`/agents/${agentId}/conversation?incarnation=1`);
  const shell = page.getByTestId("history-only-agent");
  await expect(shell).toBeVisible();
  await expect(page.getByText(historyText, { exact: true })).toBeVisible();
  await expect(shell.getByText(/Read-only history/)).toBeVisible();
  await expect(shell.getByText("Execution unconfirmed", { exact: true }).first()).toBeVisible();
  await expect(shell.locator("textarea")).toBeDisabled();
  await expect(shell.locator('input[type="file"]')).toBeDisabled();
  for (const label of ["Attach files", "Switch agent model", "Stop this turn", "Send"]) {
    await expect(shell.getByRole("button", { name: label, exact: true })).toBeDisabled();
  }
  // Disabled DOM is not the security boundary: force form/button events too.
  await shell.locator("form").evaluate((form) => {
    form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    form.dispatchEvent(new Event("drop", { bubbles: true, cancelable: true }));
    for (const button of form.querySelectorAll("button")) {
      button.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    }
  });
  await expect.poll(() => streamAttempts).toBeGreaterThan(0);
  await expect(page.getByText(historyText, { exact: true })).toBeVisible();
  expect(mutations).toEqual([]);
  expect(rawEvents).toEqual([]);
  unavailable = false;
  await shell.getByRole("button", { name: "Retry", exact: true }).first().click();
  await expect(shell).toHaveCount(0);
  await expect(page.locator(".composer textarea")).toBeEnabled();
  expect(mutations.filter((entry) => !entry.includes("read"))).toEqual([]);
  expect(rawEvents).toEqual([]);
});

for (const navigation of ["newer", "latest"] as const) {
  test(`cold source deep link reveals a folded old machine input and reads ${navigation} without mutation`, async ({
    page, context, request,
  }, info) => {
    const session = sessionFor(info, `cold-source-${navigation}`);
    const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
    await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
    await seedHistory(request, session);
    const snapshot = await request.get(control(`/api/agents/${agentId}/conversation`))
      .then((response) => response.json()) as ConversationSummaryResponse;
    const oldInput = {
      message_id: "old-machine-input", preview: "Located old machine input",
      presentation_class: "internal" as const, interjected: true,
    };
    const oldTurn = {
      ...snapshot.turns[0],
      inputs: [...snapshot.turns[0].inputs, oldInput],
      execution: { kind: "terminal", outcome: "completed" },
      settled: true,
    };
    const latestTurn = {
      ...oldTurn, turn_id: "latest-turn", key: { turn_id: "latest-turn", turn_index: 2 },
      inputs: [{ message_id: "latest-input", preview: "Latest cold history question" }], brief_ids: [],
    };
    await request.post(control("/__e2e__/conversation"), { data: {
      agentId, turns: [oldTurn, latestTurn],
    } });
    // Freeze one canonical snapshot scope for bootstrap, locate and pagination.
    const head = await request.get(control(`/api/agents/${agentId}/conversation`))
      .then((response) => response.json()) as ConversationSummaryResponse;
    const windowFor = (turns: typeof head.turns, hasNewer: boolean) => ({
      ...head, turns, active_turns: [], has_more: false, next_before_cursor: null,
      next_after_cursor: hasNewer ? "cold-next-page" : null,
      coverage: {
        first_turn_key: turns[0]?.key, last_turn_key: turns.at(-1)?.key,
        membership_upper_bound: latestTurn.key, has_older: false, has_newer: hasNewer,
        reaches_oldest: true, reaches_latest: !hasNewer,
      },
    });
    const { mutations, rawEvents } = await trackRequests(page);
    await page.route("**/api/handshake", async (route) => {
      const response = await route.fetch();
      const handshake = await response.json();
      await route.fulfill({ json: {
        ...handshake, capabilities: [...handshake.capabilities,
          "agents.conversation-locate.v1", "agents.conversation-history-bidirectional.v1"],
      } });
    });
    await page.route(`**/api/agents/${agentId}/state`, (route) => route.fulfill(pending));
    await page.route((url) => url.pathname === `/api/agents/${agentId}/conversation`, (route) =>
      route.fulfill({ json: windowFor([head.turns[1]], false) }));
    const sourceRef = `message:${oldInput.message_id}`;
    await page.route(`**/api/agents/${agentId}/conversation/locate`, (route) => route.fulfill({ json: {
      status: "located", source_ref: sourceRef, reason: null,
      runtime_id: head.runtime_id, event_log_epoch: head.event_log_epoch, visibility_scope_id: head.visibility_scope_id,
      target: { kind: "input_message", message_id: oldInput.message_id, turn_id: oldTurn.turn_id, activity_id: null },
      window: windowFor([head.turns[0]], true),
    } }));
    await page.goto(`/agents/${agentId}/conversation?incarnation=1&source_ref=${encodeURIComponent(sourceRef)}`);
    const shell = page.getByTestId("history-only-agent");
    await expect(shell.getByText(oldInput.preview, { exact: true })).toBeVisible();
    await expect(shell.getByText("No visible activity in this turn.", { exact: true })).toBeVisible();
    await expect(shell.locator('[data-location-target="true"]')).toHaveAttribute("data-conversation-anchor", `input:${oldInput.message_id}`);
    await expect(shell.getByText("Conversation source located.", { exact: true })).toBeVisible();
    await expect(shell.getByText("Latest cold history question", { exact: true })).toHaveCount(0);
    if (navigation === "newer") {
      await shell.getByRole("button", { name: "Load newer conversation", exact: true }).click();
      await expect(shell.getByText("Latest cold history question", { exact: true })).toBeVisible();
      await expect(shell.getByRole("button", { name: "Load newer conversation", exact: true })).toHaveCount(0);
    } else {
      await shell.getByRole("button", { name: "Back to latest", exact: true }).click();
      await expect(shell.getByText("Latest cold history question", { exact: true })).toBeVisible();
      await expect(page).not.toHaveURL(/source_ref=/);
      await expect(shell.getByText("Conversation source located.", { exact: true })).toHaveCount(0);
    }
    await expect(shell.locator("textarea")).toBeDisabled();
    expect(mutations).toEqual([]);
    expect(rawEvents).toEqual([]);
  });
}

for (const source of ["identity", "summary", "stream"] as const) {
  for (const status of [401, 403, 404]) {
    test(`cold history is revoked by ${source} ${status}`, async ({ page, context, request }, info) => {
      const session = sessionFor(info, `history-${source}-${status}`);
      await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
      await seedHistory(request, session);
      const { mutations, rawEvents } = await trackRequests(page);
      await page.route(`**/api/agents/${agentId}/state`, (route) => route.fulfill(pending));
      let reject = false;
      const endpoint = source === "identity" ? `**/api/control/agents/${agentId}/detail`
        : `**/api/agents/${agentId}/conversation${source === "summary" ? "" : "/stream"}*`;
      await page.route(endpoint, (route) => reject ? route.fulfill({
        status, json: { ok: false, code: "access_denied", error: "Access denied", retryable: false },
      }) : route.continue());
      await page.goto(`/agents/${agentId}/conversation?incarnation=1`);
      await expect(page.getByTestId("history-only-agent")).toBeVisible();
      await expect(page.getByText(historyText, { exact: true })).toBeVisible();
      reject = true;
      // Start a fresh admission attempt. The previous online read must not
      // retain permission, and a rejected attempt must revoke old content.
      await page.reload();
      await expect(page.getByTestId("history-only-agent")).toHaveCount(0);
      await expect(page.getByText(historyText, { exact: true })).toHaveCount(0);
      expect(mutations).toEqual([]);
      expect(rawEvents).toEqual([]);
    });
  }
}

for (const change of ["deleting", "deleted", "incarnation"] as const) {
  test(`cold history identity ${change} revokes content and ignores a late summary`, async ({ page, context, request }, info) => {
    const session = sessionFor(info, `history-${change}`);
    await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
    await seedHistory(request, session);
    const { mutations, rawEvents } = await trackRequests(page);
    await page.route(`**/api/agents/${agentId}/state`, (route) => route.fulfill(pending));
    let changed = false;
    await page.route(`**/api/control/agents/${agentId}/detail`, async (route) => {
      const response = await route.fetch();
      const detail = await response.json();
      if (changed) {
        if (change === "incarnation") detail.identity.incarnation = 2;
        else detail.identity.status = change;
      }
      await route.fulfill({ json: detail });
    });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let summaryStarted = false;
    await page.route(`**/api/agents/${agentId}/conversation*`, async (route) => {
      if (route.request().url().includes("/conversation/stream")) return route.continue();
      const response = await route.fetch();
      summaryStarted = true;
      await gate;
      await route.fulfill({ response });
    });
    try {
      await page.goto(`/agents/${agentId}/conversation?incarnation=1`);
      await expect(page.getByTestId("history-only-agent")).toBeVisible();
      await expect.poll(() => summaryStarted).toBe(true);
      changed = true;
      // A fresh admission observes the changed identity. The in-flight summary
      // may finish later, but must not revive the revoked history.
      await page.reload();
      await expect(page.getByTestId("history-only-agent")).toHaveCount(0);
      release();
      await expect(page.getByText(historyText, { exact: true })).toHaveCount(0);
      expect(mutations).toEqual([]);
      expect(rawEvents).toEqual([]);
    } finally {
      release();
    }
  });
}

import { expect, test } from "@playwright/test";

import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const buddyId = "e2e-agent";
const cachedText = "Previously verified conversation history.";

for (const result of ["available", "forbidden", "missing", "replaced"] as const) {
  test(`warm conversation is read-only before parallel revalidation: ${result}`, async ({
    page, context, request,
  }, info) => {
    const session = sessionFor(info, "warm-preview");
    const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
    const conversationPath = `/agents/${agentId}/conversation${result === "replaced" ? "?incarnation=1" : ""}`;
    await context.addCookies([{
      name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/",
    }]);
    // Warm previews require a verified user; the generic fixture is anonymous.
    await page.route("**/api/auth/session/me", (route) => route.fulfill({
      json: { user_id: "warm-preview-user", auth_method: "static_token" },
    }));
    await request.post(control("/__e2e__/configure"), { data: {
      visibleAgentIds: [agentId, buddyId],
      briefsById: { cached: {
        id: "cached", agent_id: agentId, workspace_id: "holon", kind: "result",
        text: cachedText, created_at: "2026-10-16T08:00:00Z",
        content_source: { kind: "inline" },
      } },
    } });
    await request.post(control("/__e2e__/conversation"), { data: {
      agentId,
      turns: [{
        turn_id: "cached-turn", key: { turn_id: "cached-turn", turn_index: 1 }, revision: 1,
        presentation_class: "operator", inputs: [{ message_id: "input", preview: "Cached question" }],
        execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" },
        settled: true, attention: null, detail_coverage: { kind: "complete" }, brief_ids: ["cached"],
      }],
    } });
    await page.goto(conversationPath);
    await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
    await expect(page.locator(".composer textarea")).toBeEnabled();
    await page.locator(`.agent-row[title*="${buddyId}"]`).click();
    await expect(page.locator(".composer textarea")).toBeEnabled();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let identities = 0;
    let states = 0;
    let preview = true;
    const previewWrites: string[] = [];
    page.on("request", (outgoing) => {
      if (preview && outgoing.method() === "POST"
        && outgoing.url().includes(`/agents/${agentId}/brief-read-cursor`)) {
        previewWrites.push(outgoing.url());
      }
    });
    await page.route(`**/api/control/agents/${agentId}/detail`, async (route) => {
      identities++;
      await gate;
      if (result === "forbidden" || result === "missing") {
        await route.fulfill({
          status: result === "forbidden" ? 403 : 404,
          json: { error: result === "forbidden" ? "forbidden" : "agent_not_found" },
        });
        return;
      }
      const response = await route.fetch();
      const detail = await response.json();
      if (result === "replaced") detail.identity.incarnation = 2;
      await route.fulfill({ json: detail });
    });
    await page.route(`**/api/agents/${agentId}/state`, async (route) => {
      states++;
      await gate;
      const response = await route.fetch();
      const state = await response.json();
      if (result === "replaced") state.agent.identity.incarnation = 2;
      await route.fulfill({ json: state });
    });

    try {
      await page.evaluate((path) => {
        history.pushState(null, "", path);
        window.dispatchEvent(new PopStateEvent("popstate"));
      }, conversationPath);
      // These assertions run while BOTH network responses are held indefinitely.
      // A missing access/cache split therefore fails rather than passing after a timeout.
      await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
      await expect(page.getByText("Loading agent…", { exact: true })).toHaveCount(0);
      await expect(page.locator(".composer textarea")).toBeDisabled();
      await expect.poll(() => identities).toBeGreaterThan(0);
      await expect.poll(() => states).toBeGreaterThan(0);
      expect(previewWrites).toEqual([]);
      preview = false;
      release();
      if (result === "available") {
        await expect(page.locator(".composer textarea")).toBeEnabled();
        await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
      } else {
        await expect(page.getByText(cachedText, { exact: true })).toHaveCount(0);
        await expect(page.locator(".composer textarea:enabled")).toHaveCount(0);
      }
    } finally {
      release();
    }
  });
}

import { expect, test } from "@playwright/test";

import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const buddyId = "e2e-agent";
const cachedText = "Previously verified conversation history.";

test("cold access does not expose a composer before identity validation", async ({ page, context, request }, info) => {
  const session = sessionFor(info, "cold-access");
  await context.addCookies([{
    name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/",
  }]);
  await page.route("**/api/auth/session/me", (route) => route.fulfill({
    // Without a verified user, even a roster summary cannot seed a warm preview.
    json: {},
  }));
  await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, { data: {
    visibleAgentIds: [agentId],
  } });
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route(`**/api/control/agents/${agentId}/detail`, async (route) => {
    await gate;
    await route.continue();
  });
  await page.route(`**/api/agents/${agentId}/state`, async (route) => {
    await gate;
    await route.continue();
  });
  try {
    await page.goto(`/agents/${agentId}/conversation`);
    await expect(page.getByText("Loading agent…", { exact: true })).toBeVisible();
    await expect(page.locator(".composer")).toHaveCount(0);
    release();
    await expect(page.locator(".composer textarea")).toBeEnabled();
  } finally {
    release();
  }
});

for (const result of ["available", "reloaded", "offline", "forbidden", "missing", "replaced",
  "send-forbidden", "send-missing", "send-replaced", "stopped"] as const) {
  test(`warm history revalidates silently while explicit sends remain server-validated: ${result}`, async ({
    page, context, request,
  }, info) => {
    const session = sessionFor(info, "warm-preview");
    const control = (path: string) => `${path}?session=${encodeURIComponent(session)}`;
    const conversationPath = `/agents/${agentId}/conversation${result === "replaced" ? "?incarnation=1" : ""}`;
    await context.addCookies([{
      name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/",
    }]);
    // Keep the confirmed subject stable across navigation and reload.
    await page.route("**/api/auth/session/me", (route) => route.fulfill({
      json: { user_id: "warm-preview-user", auth_method: "static_token" },
    }));
    await request.post(control("/__e2e__/configure"), { data: {
      visibleAgentIds: [agentId, buddyId],
      briefsById: { cached: {
        id: "cached", agent_id: agentId, workspace_id: "holon", kind: "result",
        text: cachedText, created_at: "2026-10-16T08:00:00Z",
        content_source: { kind: "inline" },
        attachments: null, related_message_id: null, related_task_id: null,
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
    if (result === "stopped") {
      await page.route(`**/api/agents/${agentId}/state`, async (route) => {
        const response = await route.fetch();
        const state = await response.json();
        state.agent.lifecycle = { accepts_external_messages: false, operator_hint: "Stopped by operator" };
        await route.fulfill({ json: state });
      });
    }
    await page.goto(conversationPath);
    await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
    if (result === "reloaded" || result === "stopped") {
      // Reload only after the best-effort brief write has actually committed.
      await expect.poll(() => page.evaluate(async () => {
        const db = await new Promise<IDBDatabase>((resolve, reject) => {
          const open = indexedDB.open("holon-webgui-cache");
          open.onsuccess = () => resolve(open.result);
          open.onerror = () => reject(open.error);
        });
        try {
          return await new Promise<unknown[]>((resolve, reject) => {
            const read = db.transaction("briefs").objectStore("briefs").getAll();
            read.onsuccess = () => resolve(read.result);
            read.onerror = () => reject(read.error);
          });
        } finally {
          db.close();
        }
      })).toEqual(expect.arrayContaining([expect.objectContaining({
        agentId, briefId: "cached", brief: expect.objectContaining({ text: cachedText }),
      })]));
    }
    await expect(page.locator(".composer textarea")).toBeEnabled();
    await page.locator(`.agent-row[title*="${buddyId}"]`).click();
    await expect(page.locator(".composer textarea")).toBeEnabled();

    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let identities = 0;
    let states = 0;
    let preview = true;
    let offline = result === "offline";
    const prompts: Record<string, unknown>[] = [];
    await page.route(`**/api/control/agents/${agentId}/prompt`, async (route) => {
      prompts.push(route.request().postDataJSON());
      if (result.startsWith("send-")) {
        await route.fulfill({
          status: result === "send-forbidden" ? 403 : result === "send-missing" ? 404 : 409,
          json: { code: result === "send-replaced" ? "agent_incarnation_mismatch" : "access_denied",
            error: "Prompt rejected" },
        });
        return;
      }
      if (offline) {
        await route.fulfill({ status: 503, json: { error: "Send temporarily unavailable" } });
        return;
      }
      await route.fulfill({ json: { message_id: "draft-input" } });
    });
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
      if (offline) {
        await route.abort("internetdisconnected");
        return;
      }
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
      if (offline) {
        await route.abort("internetdisconnected");
        return;
      }
      const response = await route.fetch();
      const state = await response.json();
      if (result === "replaced") state.agent.identity.incarnation = 2;
      if (result === "stopped") {
        state.agent.lifecycle = { accepts_external_messages: false, operator_hint: "Stopped by operator" };
      }
      await route.fulfill({ json: state });
    });

    try {
      if (result === "reloaded" || result === "stopped") {
        // Discard in-memory access records and exercise the persisted lifecycle hint.
        await page.goto(conversationPath);
      } else {
        await page.evaluate((path) => {
          history.pushState(null, "", path);
          window.dispatchEvent(new PopStateEvent("popstate"));
        }, conversationPath);
      }
      // These assertions run while BOTH network responses are held indefinitely.
      // A missing access/cache split therefore fails rather than passing after a timeout.
      await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
      await expect(page.getByText("Loading agent…", { exact: true })).toHaveCount(0);
      const textarea = page.locator(".composer textarea");
      const send = page.locator(".composer .send-button");
      await expect(textarea).toBeEnabled();
      await textarea.fill("Draft typed before verification");
      await page.locator(".composer-file-input").setInputFiles({
        name: "draft.txt", mimeType: "text/plain", buffer: Buffer.from("Local attachment"),
      });
      await expect(page.locator(".composer-attachments")).toContainText("draft.txt");
      const accessNotice = page.locator(".conversation-sync-status .conversation-status-banner[role=status]");
      await expect(accessNotice).toHaveCount(0);
      await expect(page.getByText("Verifying access", { exact: false })).toHaveCount(0);
      await expect(send).toBeEnabled({ enabled: result !== "stopped" });
      await expect.poll(() => identities).toBeGreaterThan(0);
      await expect.poll(() => states).toBeGreaterThan(0);
      expect(previewWrites).toEqual([]);
      if (result === "available" || result === "reloaded" || result.startsWith("send-")) {
        // Identity and state remain held: sending must not depend on read admission.
        await send.click();
        await expect.poll(() => prompts.length).toBe(1);
        expect(prompts[0]).toMatchObject({
          text: "Draft typed before verification", expected_incarnation: 1,
          attachments: [{ name: "draft.txt", media_type: "text/plain" }],
        });
        if (result === "available" || result === "reloaded") {
          await expect(textarea).toHaveValue("");
          await expect(page.locator(".composer-attachments")).toHaveCount(0);
          expect(previewWrites).toEqual([]);
        } else {
          await expect(page.getByText(cachedText, { exact: true })).toHaveCount(0);
          await expect(page.locator(".composer textarea:enabled")).toHaveCount(0);
          // Revocation unmounts the composer, but must not discard the unsent draft.
          expect(await page.evaluate(() => Object.values(localStorage)
            .some((value) => value.includes("Draft typed before verification")))).toBe(true);
        }
      }
      if (result === "stopped") {
        await textarea.press("Enter");
        await page.locator("form.composer").evaluate((form: HTMLFormElement) => form.requestSubmit());
        expect(prompts).toEqual([]);
      }
      preview = false;
      release();
      if (result === "offline") {
        await expect(accessNotice).toContainText("Agent status refresh is delayed");
        await expect(page.locator(".composer-status[role=status]")).toHaveCount(0);
        await expect(textarea).toBeEnabled();
        await textarea.fill("Draft edited while offline");
        await send.click();
        await expect(page.locator(".composer-status[role=alert]")).toContainText("Send temporarily unavailable");
        await expect(send).toBeEnabled();
        await expect(textarea).toHaveValue("Draft edited while offline");
        await expect(page.locator(".composer-attachments")).toContainText("draft.txt");
        expect(prompts).toHaveLength(1);
        await page.setViewportSize({ width: 390, height: 844 });
        await expect(accessNotice.locator(":scope > svg")).toBeInViewport();
        await expect(accessNotice.locator(":scope > span")).toBeInViewport();
        await page.setViewportSize({ width: 1280, height: 720 });
        offline = false;
        await accessNotice.getByRole("button").click();
        await expect(accessNotice).toHaveCount(0);
        // Recovery never automatically retries the failed send.
        expect(prompts).toHaveLength(1);
        await send.click();
        await expect.poll(() => prompts.length).toBe(2);
        expect(prompts[1]).toMatchObject({
          text: "Draft edited while offline", expected_incarnation: 1,
          attachments: [{ name: "draft.txt", media_type: "text/plain" }],
        });
        await expect(textarea).toHaveValue("");
      } else if (result === "forbidden" || result === "missing" || result === "replaced") {
        await expect(page.getByText(cachedText, { exact: true })).toHaveCount(0);
        await expect(page.locator(".composer textarea:enabled")).toHaveCount(0);
        expect(prompts).toEqual([]);
      }
    } finally {
      release();
      await page.unrouteAll({ behavior: "wait" });
    }
  });
}

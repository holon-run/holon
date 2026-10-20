import { expect, test } from "@playwright/test";

import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const buddyId = "e2e-agent";
const cachedText = "Previously verified conversation history.";

for (const result of ["available", "offline", "forbidden", "missing", "replaced"] as const) {
  test(`warm history is read-only but drafts remain editable before revalidation: ${result}`, async ({
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
    let offline = result === "offline";
    const prompts: Record<string, unknown>[] = [];
    await page.route(`**/api/control/agents/${agentId}/prompt`, async (route) => {
      prompts.push(route.request().postDataJSON());
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
      const textarea = page.locator(".composer textarea");
      const send = page.locator(".composer .send-button");
      await expect(textarea).toBeEnabled();
      await textarea.fill("Draft typed before verification");
      await page.locator(".composer-file-input").setInputFiles({
        name: "draft.txt", mimeType: "text/plain", buffer: Buffer.from("Local attachment"),
      });
      await expect(page.locator(".composer-attachments")).toContainText("draft.txt");
      await expect(send).toBeDisabled();
      await textarea.press("Enter");
      await page.locator("form.composer").evaluate((form: HTMLFormElement) => form.requestSubmit());
      expect(prompts).toEqual([]);
      const accessNotice = page.locator(".conversation-sync-status .conversation-status-banner[role=status]");
      await expect(accessNotice).toContainText("Verifying access");
      await expect(accessNotice.locator(":scope > svg")).toBeVisible();
      await expect(accessNotice).toHaveCSS("font-size", "13px");
      await expect(accessNotice).toHaveCSS("display", "flex");
      await expect(accessNotice).toHaveCSS("height", "32px");
      await expect(accessNotice.getByRole("button")).toHaveCount(0);
      // The existing fixed status row must remain readable on narrow screens too.
      if (result === "available") {
        const assertNoticeAlignment = async () => {
          await expect(page.locator(".conversation-pane > .composer-status")).toBeVisible();
          await expect.poll(async () => {
            const composer = (await page.locator(".composer").boundingBox())!;
            const notices = await Promise.all([
              page.locator(".conversation-header").boundingBox(),
              page.locator(".conversation-pane > .composer-status").boundingBox(),
            ]);
            return Math.max(...notices.flatMap((notice) => notice
              ? [Math.abs(notice.x - composer.x), Math.abs(notice.width - composer.width)]
              : [Number.POSITIVE_INFINITY]));
          }).toBeLessThanOrEqual(1);
          await expect(page.locator(".conversation-sync-status")).toHaveCSS("height", "32px");
        };
        for (const width of [1600, 1100, 760]) {
          await page.setViewportSize({ width, height: 900 });
          const toggle = page.getByRole("button", { name: "Context side panel", exact: true });
          if (await page.locator(".side-panel").isVisible()) await toggle.click();
          await assertNoticeAlignment();
          if (width > 760) {
            await toggle.click();
            await expect(page.locator(".side-panel")).toBeVisible();
            await assertNoticeAlignment();
            await toggle.click();
            await expect(page.locator(".side-panel")).not.toBeVisible();
            await assertNoticeAlignment();
          }
        }
        await page.setViewportSize({ width: 390, height: 844 });
        await expect.poll(async () => {
          const sidebar = await page.locator(".sidebar").boundingBox();
          return sidebar ? sidebar.x + sidebar.width : 0;
        }).toBeLessThanOrEqual(0);
        await expect(accessNotice.locator(":scope > svg")).toBeInViewport();
        await expect(accessNotice.locator(":scope > span")).toBeInViewport();
        await expect(accessNotice.locator(":scope > span")).toHaveCSS("text-overflow", "ellipsis");
        await assertNoticeAlignment();
        await page.screenshot({ path: info.outputPath("access-notice-mobile.png"), animations: "disabled" });
        await page.setViewportSize({ width: 844, height: 390 });
        await assertNoticeAlignment();
        await page.setViewportSize({ width: 1280, height: 720 });
        await assertNoticeAlignment();
        await page.screenshot({ path: info.outputPath("access-notice-desktop.png"), animations: "disabled" });
      }
      await expect.poll(() => identities).toBeGreaterThan(0);
      await expect.poll(() => states).toBeGreaterThan(0);
      expect(previewWrites).toEqual([]);
      preview = false;
      release();
      if (result === "offline") {
        await expect(accessNotice).toContainText("Agent refresh is delayed");
        await expect(textarea).toBeEnabled();
        await textarea.fill("Draft edited while offline");
        await textarea.press("Enter");
        await page.locator("form.composer").evaluate((form: HTMLFormElement) => form.requestSubmit());
        await expect(send).toBeDisabled();
        expect(prompts).toEqual([]);
        offline = false;
        await accessNotice.getByRole("button").click();
      }
      if (result === "available" || result === "offline") {
        const draft = result === "offline" ? "Draft edited while offline" : "Draft typed before verification";
        await expect(textarea).toHaveValue(draft);
        await expect(send).toBeEnabled();
        await expect(page.locator(".composer-attachments")).toContainText("draft.txt");
        expect(prompts).toEqual([]);
        await expect(page.getByText(cachedText, { exact: true })).toBeVisible();
        await send.click();
        await expect.poll(() => prompts.length).toBe(1);
        expect(prompts[0]).toMatchObject({
          text: draft, expected_incarnation: 1,
          attachments: [{ name: "draft.txt", media_type: "text/plain" }],
        });
        await expect(textarea).toHaveValue("");
      } else {
        await expect(page.getByText(cachedText, { exact: true })).toHaveCount(0);
        await expect(page.locator(".composer textarea:enabled")).toHaveCount(0);
        expect(prompts).toEqual([]);
      }
    } finally {
      release();
    }
  });
}

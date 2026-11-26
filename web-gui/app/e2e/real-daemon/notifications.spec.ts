import http from "node:http";

import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

const REPLY = "inbox-e2e-reply";
const COMPLETE = "inbox-e2e-complete";
const WAIT = "inbox-e2e-wait";
const SILENT = "inbox-e2e-silent";
const markers = [REPLY, WAIT, SILENT] as const;
type Marker = typeof markers[number];

// Only the model is scripted. The runtime admits authenticated input, executes
// the lifecycle tools, and commits the waits and briefs in its own database.
async function startProvider() {
  let sequence = 0;
  const toolTurns = new Map<string, { marker: Marker; callId: string }>();
  const calls: string[] = [];
  const server = http.createServer(async (request, response) => {
    try {
      let raw = "";
      for await (const chunk of request) raw += chunk.toString();
      const body = JSON.parse(raw || "{}") as { input?: unknown; previous_response_id?: string };
      const input = JSON.stringify(body.input ?? "");
      const id = `resp_inbox_${++sequence}`;
      const continuation = toolTurns.get(body.previous_response_id ?? "")
        ?? [...toolTurns.values()].find(({ callId }) => input.includes(`"${callId}"`));
      const marker = continuation?.marker ?? markers.reduce<Marker | undefined>((latest, candidate) =>
        input.lastIndexOf(candidate) > (latest ? input.lastIndexOf(latest) : -1) ? candidate : latest, undefined);
      let output: unknown[] = [{
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: marker ? `Formal brief: ${marker}` : "probe" }],
      }];
      const tool = (name: string, args: unknown) => {
        const callId = `call_inbox_${sequence}`;
        calls.push(name);
        if (marker) toolTurns.set(id, { marker, callId });
        output = [{
          type: "function_call",
          id: `fc_inbox_${sequence}`,
          call_id: callId,
          name,
          arguments: JSON.stringify(args),
        }];
      };
      if (!continuation && (marker === WAIT || marker === SILENT)) {
        tool("WaitFor", {
          wake: "operator_input",
          delivery: marker === SILENT ? "silent" : "final",
          reason: "An operator decision is needed",
        });
      }
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({
        id, status: "completed", output, usage: { input_tokens: 1, output_tokens: 1 },
      }));
    } catch (error) {
      response.writeHead(500, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ error: { message: String(error) } }));
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("provider port unavailable");
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    calls,
    async stop() {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

async function installLocalToken(page: Page, token: string) {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token: value }));
    localStorage.setItem("holon.webGui.languageMode.v1", "en");
  }, token);
}

test("the real daemon delivers inbox changes and recovers them without replaying toasts", async ({
  daemonFactory, page, context,
}) => {
  test.setTimeout(120_000);
  const provider = await startProvider();
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    await installLocalToken(page, daemon.token);
    const send = async (text: string) => {
      const response = await daemon.api(`/control/agents/${daemon.agentId}/prompt`, {
        method: "POST", body: JSON.stringify({ text, client_request_id: text }),
      });
      expect(response.ok, await response.text()).toBe(true);
    };
    const list = async (lane: "pending" | "recent") => {
      const response = await daemon.api(`/notifications/${lane}`);
      expect(response.ok).toBe(true);
      expect(response.headers.get("Cache-Control")).toBe("no-store");
      return (await response.json() as { items: { id: string; kind: string }[] }).items;
    };
    const kinds = async () => (await list("recent")).map((item) => item.kind);
    const navBadge = page.locator(".primary-global-nav .nav-attention-badge");
    const inboxPanel = page.locator(".dashboard-triage-panel");
    const toasts = page.getByTestId("notification-toast");

    expect((await fetch(`${daemon.baseUrl}/api/notifications/recent`)).status).toBe(401);
    const stream = page.waitForResponse((response) =>
      new URL(response.url()).pathname === "/api/events/stream" && response.ok());
    await page.goto(`${daemon.baseUrl}/`);
    await stream;
    await expect(navBadge).toHaveCount(0);
    await expect(page.getByText("Notifications may be out of date.")).toHaveCount(0);
    await send(REPLY);
    await expect.poll(kinds, { timeout: 15_000 }).toEqual(["operator_first_brief"]);
    await expect(toasts).toHaveText(/New reply/, { timeout: 15_000 });
    await page.getByRole("button", { name: "Dismiss notification" }).click();

    await context.setOffline(true);
    await daemon.restart(); // also guarantees the original SSE transport closes
    const created = await daemon.api(`/control/agents/${daemon.agentId}/work-items`, {
      method: "POST", body: JSON.stringify({ objective: COMPLETE }),
    });
    expect(created.ok).toBe(true);
    const work = await created.json() as { id: string };
    const completed = await daemon.api(`/control/agents/${daemon.agentId}/work-items/${work.id}/complete`, {
      method: "POST", body: JSON.stringify({ report_text: "Canonical result from the control API" }),
    });
    expect(completed.ok, await completed.text()).toBe(true);
    await expect.poll(kinds, { timeout: 25_000 }).toEqual(["work_item_completed", "operator_first_brief"]);
    await context.setOffline(false);
    await inboxPanel.getByRole("tab", { name: /Recent/ }).click();
    await expect(inboxPanel.getByText("Result ready")).toBeVisible();
    await expect(inboxPanel.getByText("New reply")).toBeVisible();
    await expect(toasts).toHaveCount(0);

    await send(WAIT);
    await expect.poll(async () => (await list("pending")).length, { timeout: 15_000 }).toBe(1);
    await expect(navBadge).toHaveText("1");
    await inboxPanel.getByRole("tab", { name: /Recent/ }).click();
    await expect(inboxPanel.locator(".dashboard-triage-list").getByText("Needs your input")).toHaveCount(1);
    await expect.poll(kinds).toEqual(["operator_input_required", "work_item_completed", "operator_first_brief"]);
    await inboxPanel.getByRole("tab", { name: /Needs your action/ }).click();
    await inboxPanel.locator(".dashboard-triage-main").first().click();
    await expect(page).toHaveURL(new RegExp(`/agents/${daemon.agentId}/conversation`));
    await expect(navBadge).toHaveText("1");

    const pendingBefore = await list("pending");
    await daemon.restart();
    await page.reload();
    await expect(navBadge).toHaveText("1");
    expect(await list("pending")).toEqual(pendingBefore);
    expect(await kinds()).toEqual(["operator_input_required", "work_item_completed", "operator_first_brief"]);
    await expect(toasts).toHaveCount(0);
    expect(provider.calls).toEqual(["WaitFor"]);
  } finally {
    await provider.stop();
  }
});

test("a silent operator wait appears only in Needs your input", async ({ daemonFactory, page }) => {
  const provider = await startProvider();
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    await installLocalToken(page, daemon.token);
    await page.goto(`${daemon.baseUrl}/settings`);
    const navBadge = page.locator(".primary-global-nav .nav-attention-badge");
    await expect(navBadge).toHaveCount(0);
    const sent = await daemon.api(`/control/agents/${daemon.agentId}/prompt`, {
      method: "POST", body: JSON.stringify({ text: SILENT }),
    });
    expect(sent.ok, await sent.text()).toBe(true);
    await expect(navBadge).toHaveText("1", { timeout: 15_000 });
    const recent = await daemon.api("/notifications/recent");
    expect(recent.ok).toBe(true);
    expect(await recent.json()).toEqual({ items: [] });
    await expect(page.getByTestId("notification-toast")).toHaveCount(0);
    expect(provider.calls).toEqual(["WaitFor"]);
  } finally {
    await provider.stop();
  }
});

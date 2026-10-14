import http from "node:http";
import type { Locator, Page } from "@playwright/test";

import { expect, test as daemonTest } from "./daemon-fixture";

interface Provider {
  baseUrl: string;
  stop(): Promise<void>;
}

interface ConversationDaemon {
  baseUrl: string;
  token: string;
  api(pathname: string, init?: RequestInit): Promise<Response>;
  enqueue(text: string): Promise<string>;
}

interface Turn {
  turn_id: string;
  inputs: Array<{ message_id: string }>;
  brief_ids: string[];
  execution: { kind: string; outcome?: string };
}

interface Summary {
  runtime_id: string;
  turns: Turn[];
  active_turns: Turn[];
  coverage: { has_older: boolean; has_newer: boolean };
}

interface ReadState {
  unread_count: number;
  read_through_event_seq: number;
}

interface Seed {
  runtimeId: string;
  turns: Turn[];
  messages: string[];
}

const CONVERSATION_URL = /\/api\/agents\/default\/conversation(?:\?|$)/;
const LOCATE_URL = /\/api\/agents\/default\/conversation\/locate$/;
const TARGET = '[data-location-target="true"]';

// Only the model provider is local. Every conversation, locate, history, and
// unread response comes from the isolated real daemon and its durable state.
async function startProvider(): Promise<Provider> {
  let sequence = 0;
  const server = http.createServer((request, response) => {
    request.resume();
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({
      id: `resp_location_${++sequence}`,
      status: "completed",
      output: [{
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: `location brief ${sequence}` }],
      }],
      usage: { input_tokens: 1, output_tokens: 1 },
    }));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("failed to allocate the location provider port");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    async stop() {
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

const test = daemonTest.extend<{ locationProvider: Provider }>({
  locationProvider: async ({}, use) => {
    const provider = await startProvider();
    try {
      await use(provider);
    } finally {
      await provider.stop();
    }
  },
});

async function json<T>(daemon: ConversationDaemon, pathname: string): Promise<T> {
  const response = await daemon.api(pathname);
  expect(response.ok, `${pathname}: HTTP ${response.status}`).toBe(true);
  return await response.json() as T;
}

async function enqueueCompleted(daemon: ConversationDaemon, text: string): Promise<{
  messageId: string;
  turn: Turn;
  runtimeId: string;
}> {
  const messageId = await daemon.enqueue(text);
  let summary: Summary | undefined;
  let turn: Turn | undefined;
  await expect.poll(async () => {
    summary = await json<Summary>(daemon, "/agents/default/conversation?limit=30");
    turn = summary.turns.find((candidate) =>
      candidate.inputs.some((input) => input.message_id === messageId));
    // Delivery finality (`settled`) is independent of execution completion.
    return turn?.execution.kind === "terminal"
      && turn.execution.outcome === "completed" && turn.brief_ids.length > 0
      ? "completed"
      : JSON.stringify({ messageId, summary });
  }, { timeout: 30_000, message: `real daemon completed ${text} with a Brief` }).toBe("completed");
  if (!summary || !turn) throw new Error("completed turn was not projected");
  return { messageId, turn, runtimeId: summary.runtime_id };
}

async function seed(daemon: ConversationDaemon): Promise<Seed> {
  const entries = [];
  // Wait between enqueues so there are three distinct real turns, not three
  // inputs batched into one execution or a hundred-turn pagination fixture.
  for (const marker of ["location oldest", "location middle", "location newest"]) {
    entries.push(await enqueueCompleted(daemon, marker));
  }
  return {
    runtimeId: entries[0].runtimeId,
    turns: entries.map((entry) => entry.turn),
    messages: entries.map((entry) => entry.messageId),
  };
}

async function installToken(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
  }, token);
}

function deepLink(daemon: ConversationDaemon, sourceRef: string, runtimeId: string): string {
  const url = new URL("/agents/default/conversation", daemon.baseUrl);
  url.searchParams.set("source_ref", sourceRef);
  url.searchParams.set("runtime_id", runtimeId);
  return url.href;
}

async function navigate(page: Page, url: string): Promise<void> {
  // Exercise the same browser-route/popstate entry point as back/forward,
  // without destroying the in-flight controller or starting a new document.
  await page.evaluate((target) => {
    history.pushState(null, "", target);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, url);
}

async function smallWindows(page: Page): Promise<void> {
  await page.route(CONVERSATION_URL, async (route) => {
    const url = new URL(route.request().url());
    url.searchParams.set("limit", "1");
    await route.continue({ url: url.href });
  });
  await page.route(LOCATE_URL, async (route) => {
    await route.continue({
      postData: JSON.stringify({ ...route.request().postDataJSON(), limit: 1 }),
    });
  });
}

async function expectTarget(page: Page, anchor: string): Promise<Locator> {
  const target = page.locator(TARGET);
  await expect(target).toHaveCount(1);
  await expect(target).toHaveAttribute("data-conversation-anchor", anchor);
  await expect(target).toBeVisible();
  await expect(target).toBeInViewport();
  await expect(target).toBeFocused();
  return target;
}

async function turnIds(page: Page): Promise<string[]> {
  return await page.locator("[data-turn-id]").evaluateAll((elements) =>
    elements.map((element) => element.getAttribute("data-turn-id")!));
}

async function liveCacheContains(page: Page, turnId: string): Promise<boolean> {
  return await page.evaluate((id) => new Promise<boolean>((resolve, reject) => {
    const request = indexedDB.open("holon-webgui-cache");
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains("snapshots")) {
        db.close();
        resolve(false);
        return;
      }
      const transaction = db.transaction("snapshots", "readonly");
      const entries = transaction.objectStore("snapshots").getAll();
      transaction.oncomplete = () => {
        db.close();
        resolve(entries.result.some((entry) => entry.agentId === "default"
          && entry.summary.turns.some((turn: Turn) => turn.turn_id === id
            && turn.execution.kind === "terminal"
            && turn.execution.outcome === "completed" && turn.brief_ids.length > 0)));
      };
      transaction.onabort = () => { db.close(); reject(transaction.error); };
    };
  }), turnId);
}

test("in-app return to search restores query and advanced scope across agent navigation", async ({
  daemonFactory, page,
}) => {
  const daemon = await daemonFactory({ webDist: "dist-e2e" });
  await installToken(page, daemon.token);
  const searchUrl = new URL("/search?q=location&scope=all", daemon.baseUrl).href;
  await page.goto(searchUrl);
  await expect(page.locator(".search-page input").first()).toHaveValue("location");
  await expect(page.locator(".search-page select").nth(1)).toHaveValue("all");
  await page.locator(".agent-row").first().click();
  await expect(page).toHaveURL(/\/agents\/default\/conversation$/);
  await page.locator(".global-nav button").nth(1).click();
  await expect(page).toHaveURL(searchUrl);
  await expect(page.locator(".search-page input").first()).toHaveValue("location");
  await expect(page.locator(".search-page select").nth(1)).toHaveValue("all");
  await page.reload();
  await expect(page.locator(".search-page select").nth(1)).toHaveValue("all");
});

test("canonical input and Brief deep links highlight exactly across reload and browser back", async ({
  daemonFactory, locationProvider, page,
}) => {
  test.setTimeout(120_000);
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: { HOLON_OPENAI_BASE_URL: locationProvider.baseUrl },
  });
  const data = await seed(daemon);
  await installToken(page, daemon.token);
  await smallWindows(page);
  const inputUrl = deepLink(daemon, `message:${data.messages[1]}`, data.runtimeId);
  const briefUrl = deepLink(daemon, `brief:${data.turns[1].brief_ids[0]}`, data.runtimeId);
  await page.goto(inputUrl);
  await expectTarget(page, `input:${data.messages[1]}`);
  await expect.poll(() => turnIds(page)).toEqual([data.turns[1].turn_id]);

  await navigate(page, briefUrl);
  const brief = await expectTarget(page, `brief:${data.turns[1].brief_ids[0]}`);
  await expect(brief).toContainText("location brief");
  await expect(page).toHaveURL(briefUrl);
  await page.reload();
  await expectTarget(page, `brief:${data.turns[1].brief_ids[0]}`);
  await page.goBack();
  await expect(page).toHaveURL(inputUrl);
  await expectTarget(page, `input:${data.messages[1]}`);
  await page.reload();
  await expectTarget(page, `input:${data.messages[1]}`);
});

test("historical location pages both ways in one chat without consuming unread Briefs", async ({
  daemonFactory, locationProvider, page,
}) => {
  test.setTimeout(120_000);
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: { HOLON_OPENAI_BASE_URL: locationProvider.baseUrl },
  });
  // A principal's initial read cursor starts at the current event head.
  // Establish it before creating the Briefs whose unread state we protect.
  await json<ReadState>(daemon, "/agents/default/brief-read-state");
  const data = await seed(daemon);
  const readBefore = await json<ReadState>(daemon, "/agents/default/brief-read-state");
  expect(readBefore.unread_count).toBeGreaterThanOrEqual(3);
  const readWrites: string[] = [];
  const directions: string[] = [];
  page.on("request", (request) => {
    if (request.method() === "POST" && request.url().endsWith("/brief-read-cursor")) {
      readWrites.push(request.url());
    }
    if (request.method() === "GET" && CONVERSATION_URL.test(request.url())) {
      const params = new URL(request.url()).searchParams;
      if (params.has("before")) directions.push("before");
      if (params.has("after")) directions.push("after");
    }
  });
  await installToken(page, daemon.token);
  await smallWindows(page);
  const locateResponse = page.waitForResponse(LOCATE_URL);
  await page.goto(deepLink(daemon, `message:${data.messages[1]}`, data.runtimeId));
  const located = await (await locateResponse).json() as { status: string; window: Summary };
  expect(located.status).toBe("located");
  expect(located.window.coverage).toMatchObject({ has_older: true, has_newer: true });
  await expectTarget(page, `input:${data.messages[1]}`);
  await expect.poll(() => turnIds(page)).toEqual([data.turns[1].turn_id]);

  await page.getByRole("button", { name: /^(Load earlier|加载更早的消息)$/ }).click();
  await expect.poll(() => turnIds(page)).toEqual(data.turns.slice(0, 2).map((turn) => turn.turn_id));
  await page.getByRole("button", { name: /^(Load newer conversation|加载更新的会话)$/ }).click();
  await expect.poll(() => turnIds(page)).toEqual(data.turns.map((turn) => turn.turn_id));
  expect(directions).toEqual(["before", "after"]);
  expect(new Set(await turnIds(page)).size).toBe(3);
  await expect(page.locator(TARGET)).toHaveAttribute("data-conversation-anchor", `input:${data.messages[1]}`);
  await expect(page.getByRole("button", { name: /^(Load earlier|加载更早的消息)$/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^(Load newer conversation|加载更新的会话)$/ })).toHaveCount(0);
  const readAfter = await json<ReadState>(daemon, "/agents/default/brief-read-state");
  expect(readAfter.read_through_event_seq).toBe(readBefore.read_through_event_seq);
  expect(readAfter.unread_count).toBe(readBefore.unread_count);
  expect(readWrites).toEqual([]);
});

test("late location is cancelled and live stream cannot replace the winning historical window", async ({
  daemonFactory, locationProvider, page,
}) => {
  test.setTimeout(120_000);
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: { HOLON_OPENAI_BASE_URL: locationProvider.baseUrl },
  });
  const data = await seed(daemon);
  await installToken(page, daemon.token);
  await smallWindows(page);
  await page.goto(deepLink(daemon, `message:${data.messages[1]}`, data.runtimeId));
  await expectTarget(page, `input:${data.messages[1]}`);

  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  let delayed = false;
  let cancelled = false;
  const losingRef = `message:${data.messages[0]}`;
  page.on("requestfailed", (request) => {
    if (LOCATE_URL.test(request.url()) && request.postDataJSON()?.source_ref === losingRef) {
      cancelled = true;
    }
  });
  await page.route(LOCATE_URL, async (route) => {
    if (route.request().postDataJSON().source_ref === losingRef) {
      delayed = true;
      await gate;
    }
    await route.continue({
      postData: JSON.stringify({ ...route.request().postDataJSON(), limit: 1 }),
    });
  });
  try {
    await navigate(page, deepLink(daemon, losingRef, data.runtimeId));
    await expect.poll(() => delayed).toBe(true);
    await navigate(page, deepLink(daemon, `brief:${data.turns[1].brief_ids[0]}`, data.runtimeId));
    await expectTarget(page, `brief:${data.turns[1].brief_ids[0]}`);
    release();
    await expect.poll(() => cancelled).toBe(true);
  } finally {
    release();
  }
  await expect.poll(() => turnIds(page)).toEqual([data.turns[1].turn_id]);

  const live = await enqueueCompleted(daemon, "location concurrent live turn");
  // The live SDK snapshot is persisted only after the real stream applies the
  // new turn; an HTTP settlement alone would not establish this race coverage.
  await expect.poll(() => liveCacheContains(page, live.turn.turn_id)).toBe(true);
  // History membership and the precise target still belong to the winner.
  await expect.poll(() => turnIds(page)).toEqual([data.turns[1].turn_id]);
  await expect(page.locator(TARGET)).toHaveAttribute(
    "data-conversation-anchor", `brief:${data.turns[1].brief_ids[0]}`,
  );
  await page.locator(".conversation-jump").click();
  await expect.poll(() => turnIds(page)).toContain(live.turn.turn_id);
  await expect(page.locator(TARGET)).toHaveCount(0);
});

import {
  expect,
  test,
  type APIRequestContext,
  type BrowserContext,
  type Page,
  type TestInfo,
} from "@playwright/test";

interface FixtureAbortRequest {
  agentId: string;
  body: { run_id?: string; mode?: string };
}

interface FixtureState {
  abortRequests: FixtureAbortRequest[];
}

function sessionFor(testInfo: TestInfo): string {
  const testCase = testInfo.testId.replace(/[^a-zA-Z0-9_-]/g, "-");
  return `${testInfo.workerIndex}-${testInfo.repeatEachIndex}-${testInfo.retry}-${testCase}`;
}

function controlPath(session: string, path: string): string {
  return `${path}?session=${encodeURIComponent(session)}`;
}

async function attachSession(context: BrowserContext, session: string): Promise<void> {
  await context.addCookies([{
    name: "holon_e2e_session",
    value: session,
    domain: "127.0.0.1",
    path: "/",
  }]);
}

async function configure(
  request: APIRequestContext,
  session: string,
  body: Record<string, unknown>,
): Promise<void> {
  const response = await request.post(controlPath(session, "/__e2e__/configure"), { data: body });
  expect(response.ok()).toBe(true);
}

async function fixtureState(
  request: APIRequestContext,
  session: string,
): Promise<FixtureState> {
  const response = await request.get(controlPath(session, "/__e2e__/state"));
  expect(response.ok()).toBe(true);
  return await response.json() as FixtureState;
}

function envelope(
  agentId: string,
  eventSeq: number,
  type: string,
  payload: Record<string, unknown>,
) {
  return {
    id: `${agentId}-event-${eventSeq}`,
    event_seq: eventSeq,
    event_log_epoch: "e2e-epoch",
    contract_version: 2,
    ts: `2026-08-25T00:00:${String(eventSeq).padStart(2, "0")}Z`,
    agent_id: agentId,
    type,
    payload_schema: `holon.runtime_event.${type}`,
    payload_schema_version: 1,
    payload,
  };
}

async function appendEvent(
  request: APIRequestContext,
  session: string,
  event: ReturnType<typeof envelope>,
): Promise<void> {
  const response = await request.post(controlPath(session, "/__e2e__/append-event"), {
    data: { envelope: event, broadcast: true },
  });
  expect(response.ok()).toBe(true);
}

async function openAgent(page: Page, agentId: string): Promise<void> {
  const button = page.getByRole("button", { name: `Open ${agentId}`, exact: true });
  await expect(button).toBeVisible();
  await button.click();
  await expect(page.locator("form.composer")).toBeVisible();
}

test("composer swaps the send button for a turn-scoped stop while a run is active", async ({
  page,
  request,
  context,
}, testInfo) => {
  const session = sessionFor(testInfo);
  await attachSession(context, session);
  const agentId = "bootstrap-agent";

  await page.goto("/");
  await openAgent(page, agentId);

  const composer = page.locator("form.composer");
  const textarea = composer.locator("textarea");
  const sendButton = composer.getByRole("button", { name: "Send", exact: true });
  const stopButton = composer.getByRole("button", { name: "Stop this turn", exact: true });

  // No run: the composer keeps the ordinary send button.
  await expect(sendButton).toBeVisible();
  await expect(stopButton).toHaveCount(0);

  // A turn starts: the empty composer switches to the stop button.
  await appendEvent(request, session, envelope(agentId, 1, "message_processing_started", {
    run_id: "run-e2e-1",
  }));
  await expect(stopButton).toBeVisible();
  await expect(sendButton).toHaveCount(0);

  // Typing restores the send button immediately.
  await textarea.fill("follow-up");
  await expect(sendButton).toBeVisible();
  await expect(stopButton).toHaveCount(0);

  // Clearing the draft while the turn still runs restores the stop button.
  await textarea.fill("");
  await expect(stopButton).toBeVisible();

  // Clicking stop aborts with the run id the UI currently knows.
  await stopButton.click();
  await expect.poll(async () => (await fixtureState(request, session)).abortRequests).toEqual([
    {
      agentId,
      body: {
        run_id: "run-e2e-1",
        mode: "idle_after_abort",
        authority_class: "operator_instruction",
      },
    },
  ]);

  // The turn ends: the composer returns to the send button even though the
  // draft stays empty.
  await appendEvent(request, session, envelope(agentId, 2, "turn_terminal", {}));
  await expect(sendButton).toBeVisible();
  await expect(stopButton).toHaveCount(0);
});

test("stop-turn conflicts converge without surfacing an error", async ({
  page,
  request,
  context,
}, testInfo) => {
  const session = sessionFor(testInfo);
  await attachSession(context, session);
  const agentId = "bootstrap-agent";

  await configure(request, session, {
    abortResponse: {
      status: 409,
      body: { error: "stale run_id run-e2e-2; current run is run-e2e-3", code: "stale_run_id" },
    },
  });

  await page.goto("/");
  await openAgent(page, agentId);

  await appendEvent(request, session, envelope(agentId, 1, "message_processing_started", {
    run_id: "run-e2e-2",
  }));
  const stopButton = page.locator("form.composer").getByRole("button", { name: "Stop this turn", exact: true });
  await expect(stopButton).toBeVisible();

  await stopButton.click();
  await expect.poll(async () => (await fixtureState(request, session)).abortRequests.length)
    .toBe(1);

  // The conflict is treated as convergence: no composer error appears and the
  // button does not stay stuck in a loading state.
  await expect(page.locator("form.composer .composer-status")).toHaveCount(0);
  await expect(stopButton).toBeEnabled();
});

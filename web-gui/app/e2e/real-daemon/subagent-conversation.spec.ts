import http from "node:http";
import type { Page } from "@playwright/test";

import { expect, test } from "./daemon-fixture";

const PARENT_INPUT = "e2e-parent-delegate-private-child";
const CHILD_INPUT = "e2e-private-child-delegated-input";
const CHILD_PROGRESS = "e2e-private-child-visible-progress";
const TOOL_OUTPUT = "e2e-private-child-tool-output";
const CHILD_BRIEF = "e2e-private-child-final-brief";
const OPERATOR_INPUT = "e2e-private-child-direct-operator-input";
const OPERATOR_BRIEF = "e2e-private-child-operator-reply";
const DELETE_INPUT = "e2e-parent-delete-retained-child";
const PARENT_BRIEF = "e2e-parent-child-result-preserved";

interface DaemonApi {
  api(pathname: string, init?: RequestInit): Promise<Response>;
}

interface TaskRecord {
  id: string;
  kind: string;
  status: string;
  target?: {
    kind: "child_agent" | "agent_message_wait";
    agent_id: string;
    incarnation?: number | null;
  };
}

interface TaskStatus {
  task_id: string;
  status: string;
  child_agent_id?: string;
  target?: TaskRecord["target"];
}

interface ChildDetail {
  identity: { incarnation: number; status: string };
}

/**
 * Only the model is scripted. InvokeAgent, ExecCommand, WaitFor, GetAgent and
 * DeleteAgent are executed by the real daemon, not seeded into a browser mock
 * or its database. Hold the child's final model response until the browser has
 * observed an active private conversation, including a cold load and refresh.
 */
async function startDelegationProvider(existingAgentId?: string) {
  let sequence = 0;
  let invoked = false;
  let waiting = false;
  let childExecuted = false;
  let existingAgentReplied = false;
  let inspectedForDeletion = false;
  let deleted = false;
  let childTarget: { agentId: string; incarnation: number } | undefined;
  let release!: () => void;
  const childGate = new Promise<void>((resolve) => { release = resolve; });
  const roles = new Map<string, "parent" | "child" | "delete" | "operator" | "probe">();
  const calls: string[] = [];
  const failures: string[] = [];

  const server = http.createServer(async (request, response) => {
    try {
      let raw = "";
      for await (const chunk of request) raw += chunk.toString();
      const body = raw ? JSON.parse(raw) as {
        input?: unknown;
        previous_response_id?: string;
      } : {};
      const input = JSON.stringify(body.input ?? "");
      const role = input.includes(DELETE_INPUT) ? "delete"
        : input.includes(OPERATOR_INPUT) ? "operator"
        : roles.get(body.previous_response_id ?? "")
          ?? (input.includes(PARENT_INPUT) ? "parent"
            : input.includes(CHILD_INPUT) ? "child" : "probe");
      const id = `resp_subagent_e2e_${++sequence}`;
      roles.set(id, role);
      let output: unknown[];
      const text = (value: string) => [{
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: value }],
      }];
      const tool = (name: string, args: unknown) => {
        calls.push(name);
        return [{
          type: "function_call",
          id: `fc_subagent_${sequence}`,
          call_id: `call_subagent_${sequence}`,
          name,
          arguments: JSON.stringify(args),
        }];
      };

      if (role === "delete" && childTarget) {
        if (!inspectedForDeletion) {
          inspectedForDeletion = true;
          output = tool("GetAgent", { agent_id: childTarget.agentId, detail: "full" });
        } else if (!deleted) {
          deleted = true;
          output = tool("DeleteAgent", {
            agent_id: childTarget.agentId,
            incarnation: childTarget.incarnation,
          });
        } else {
          output = text("e2e-parent-child-deletion-finished");
        }
      } else if (role === "operator") {
        output = text(OPERATOR_BRIEF);
      } else if (role === "parent") {
        if (!invoked) {
          invoked = true;
          output = tool("InvokeAgent", {
            target: existingAgentId
              ? { kind: "existing_agent", agent_id: existingAgentId }
              : { kind: "new_subagent", workspace_mode: "inherit" },
            initial_message: CHILD_INPUT,
          });
        } else if (!waiting) {
          const taskId = input.match(/task_[a-f0-9]+/)?.[0];
          if (!taskId) throw new Error("real InvokeAgent result did not contain a task id");
          waiting = true;
          output = tool("WaitFor", {
            wake: "task_result",
            resource: taskId,
            delivery: "silent",
            reason: "Wait for the private child result in the real-daemon test",
          });
        } else {
          output = text(PARENT_BRIEF);
        }
      } else if (role === "child") {
        if (!childExecuted) {
          childExecuted = true;
          output = [
            ...text(CHILD_PROGRESS),
            ...tool("ExecCommand", { cmd: `printf '${TOOL_OUTPUT}\\n'`, max_output_tokens: 100 }),
          ];
        } else {
          await childGate;
          if (existingAgentId && !existingAgentReplied) {
            existingAgentReplied = true;
            // Existing-agent invocation waits for a durable agent message,
            // not for the target's operator-facing final report.
            output = tool("SendAgentMessage", { agent_id: "default", message: CHILD_BRIEF });
          } else {
            output = text(CHILD_BRIEF);
          }
        }
      } else {
        // Runtime provider capability probes do not run scenario tools.
        output = text("e2e-provider-capability-probe");
      }
      if (response.destroyed) return;
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({
        id,
        status: "completed",
        output,
        usage: { input_tokens: 1, output_tokens: 1 },
      }));
    } catch (error) {
      failures.push(String(error));
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
    failures,
    release,
    setChildTarget(agentId: string, incarnation: number) {
      childTarget = { agentId, incarnation };
    },
    async stop() {
      release();
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

async function json<T>(daemon: DaemonApi, pathname: string): Promise<T> {
  const response = await daemon.api(pathname);
  expect(response.ok, `${pathname}: HTTP ${response.status}${response.ok ? "" : ` ${await response.text()}`}`).toBe(true);
  return await response.json() as T;
}

async function prompt(daemon: DaemonApi, agentId: string, text: string) {
  const response = await daemon.api(`/control/agents/${agentId}/prompt`, {
    method: "POST",
    body: JSON.stringify({ text }),
  });
  expect(response.status, await response.text()).toBe(200);
}

async function authenticate(page: Page, token: string) {
  await page.addInitScript((value) => {
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({
      mode: "local",
      token: value,
    }));
    localStorage.setItem("holon.webGui.languageMode.v1", "en");
  }, token);
}

async function openContextPanel(page: Page) {
  // Route initialization can restore panel state after the initial document load.
  await expect(page.locator(".composer textarea")).toBeVisible();
  if (!await page.locator(".side-panel").isVisible()) {
    await page.getByRole("button", { name: "Context side panel", exact: true }).click();
  }
  await expect(page.locator(".side-panel")).toBeVisible();
  await page.locator(".side-panel .panel-sections")
    .getByRole("button", { name: "Overview", exact: true }).click();
}

test("real parent delegation exposes a private child conversation, operator input and retained task delivery", async ({
  daemonFactory,
  page,
}, testInfo) => {
  test.setTimeout(120_000);
  const provider = await startDelegationProvider();
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    await authenticate(page, daemon.token);
    await page.setViewportSize({ width: 1600, height: 1000 });
    const parentUrl = `${daemon.baseUrl}/agents/${daemon.agentId}/conversation`;
    await page.goto(parentUrl);
    await expect(page.locator(".composer textarea")).toBeVisible();
    await prompt(daemon, daemon.agentId, PARENT_INPUT);

    let task: TaskRecord | undefined;
    await expect.poll(async () => {
      const tasks = await json<TaskRecord[]>(daemon, `/agents/${daemon.agentId}/tasks`);
      task = tasks.find((candidate) => candidate.kind === "actor_invocation");
      return task?.target?.agent_id;
    }, { timeout: 30_000 }).toBeTruthy();
    const statusPath = `/agents/${daemon.agentId}/tasks/${task!.id}`;
    const activeTask = await json<TaskStatus>(daemon, statusPath);
    const childId = activeTask.target?.agent_id;
    expect(childId).toBeTruthy();
    if (!childId) throw new Error("child task is missing its typed target");
    const detail = await json<ChildDetail>(daemon, `/control/agents/${childId}/detail`);
    const incarnation = detail.identity.incarnation;
    expect(incarnation).toBeGreaterThan(0);
    const target = { kind: "child_agent", agent_id: childId, incarnation };
    expect(task!.target).toEqual(target);
    expect(activeTask.target).toEqual(target);
    provider.setChildTarget(childId, incarnation);

    // A real private child must not be promoted into the public sidebar roster.
    const roster = await json<Array<{ agent_id: string }>>(daemon, "/agents/list");
    expect(JSON.stringify(roster)).not.toContain(childId);
    const childPath = `/agents/${encodeURIComponent(childId)}/conversation?incarnation=${incarnation}`;
    await openContextPanel(page);
    const childLink = page.locator(`a[href="${childPath}"]`).first();
    await expect(childLink).toBeVisible();
    await expect(childLink).toHaveAccessibleName("Open agent conversation");
    // The task's detail panel must expose the same identity-pinned link.
    await page.locator(".task-button").first().click();
    await expect(childLink).toBeVisible();
    await childLink.click();
    await expect(page).toHaveURL(`${daemon.baseUrl}${childPath}`);
    await expect(page.locator(".message-list")).toContainText(CHILD_INPUT);

    // Exercise a fresh document with no selected-agent cache, then a reload.
    const cold = await page.context().newPage();
    await authenticate(cold, daemon.token);
    await cold.goto(`${daemon.baseUrl}${childPath}`);
    await expect(cold.locator(".message-list")).toContainText(CHILD_INPUT);
    await cold.reload();
    await expect(cold).toHaveURL(`${daemon.baseUrl}${childPath}`);
    await expect(cold.locator(".message-list")).toContainText(CHILD_INPUT);
    // A historical "default" supervisor is not a verified parent navigation
    // target; do not remap it to whichever agent owns today's default route.
    await expect(cold.getByRole("link", { name: "Back to parent agent", exact: true })).toHaveCount(0);
    await expect(cold.locator('.conversation-sync-status[role="note"]')).toHaveCount(0);

    // A stale identity link must not silently open the current child or home.
    const stale = await page.context().newPage();
    await authenticate(stale, daemon.token);
    const staleUrl = `${daemon.baseUrl}/agents/${encodeURIComponent(childId)}/conversation?incarnation=${incarnation + 1}`;
    await stale.goto(staleUrl);
    await expect(stale.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("This link belongs to an earlier agent identity", { exact: true })).toBeVisible();
    await expect(stale).toHaveURL(staleUrl);
    await expect(stale.locator(".composer textarea")).toHaveCount(0);
    await stale.close();

    const activityToggle = cold.locator(".conversation-detail-toggle").first();
    await activityToggle.click();
  const delegatedInput = cold.locator(".conversation-interjection").filter({ hasText: CHILD_INPUT });
  await expect(delegatedInput.getByText(CHILD_INPUT, { exact: true })).toBeVisible();
    await expect(cold.locator(".conversation-activity").filter({ hasText: "ExecCommand" }).first()).toBeVisible();
    await expect(cold.locator(".message-list")).toContainText(CHILD_PROGRESS);
    expect((await json<TaskStatus>(daemon, statusPath)).status).not.toBe("completed");

    provider.release();
    await expect(cold.locator(".message-list")).toContainText(CHILD_BRIEF, { timeout: 30_000 });
    await expect.poll(async () => (await json<TaskStatus>(daemon, statusPath)).status).toBe("completed");
    const completedTask = await json<TaskStatus>(daemon, statusPath);
    const taskOutputPath = `${statusPath}/output?block=false`;
    const completedOutput = await json<{ task: { output_preview: string } }>(daemon, taskOutputPath);
    expect(completedOutput.task.output_preview).toContain(CHILD_BRIEF);

    // Send from the real child composer, not through TaskInput or parent relay.
    await cold.locator(".composer textarea").fill(OPERATOR_INPUT);
    await cold.locator(".composer .send-button").click();
    const operatorInput = cold.locator(".conversation-input-line").filter({ hasText: OPERATOR_INPUT });
    await expect(operatorInput).toBeVisible();
    // Operator inputs use the normal message line, not an agent-message source
    // disclosure. Check persisted provenance too; actor names are optional.
    const childConversation = await json<{
      turns: Array<{ inputs: Array<{ preview: string; presentation_class: string }> }>;
      active_turns: Array<{ inputs: Array<{ preview: string; presentation_class: string }> }>;
    }>(daemon, `/agents/${childId}/conversation`);
    const operatorRecord = [...childConversation.turns, ...childConversation.active_turns]
      .flatMap((turn) => turn.inputs).find((input) => input.preview.includes(OPERATOR_INPUT));
    expect(operatorRecord?.presentation_class).toBe("operator");
    await expect(cold.locator(".message-list")).toContainText(OPERATOR_BRIEF, { timeout: 30_000 });
    await cold.reload();
    await expect(cold.locator(".message-list")).toContainText(OPERATOR_INPUT);
    expect(await json<TaskStatus>(daemon, statusPath)).toMatchObject({
      task_id: completedTask.task_id,
      status: "completed",
      target,
    });

    // Browser history returns to the parent. The terminal task remains findable
    // even though the default active-task query no longer returns it.
    await page.goBack();
    await expect(page).toHaveURL(parentUrl);
    await expect(page.locator(".message-list")).toContainText(PARENT_BRIEF, { timeout: 30_000 });
    await openContextPanel(page);
    await page.getByText("Recently completed (up to 20)", { exact: true }).click();
    await expect(childLink).toBeVisible();
    await childLink.click();
    await expect(page.locator(".message-list")).toContainText(CHILD_BRIEF);

    // Cleanup uses the actual supervising parent's tools; private deletion is
    // deliberately not made available through the public-only control API.
    await prompt(daemon, daemon.agentId, DELETE_INPUT);
    await expect.poll(async () => {
      const response = await daemon.api(`/control/agents/${childId}/detail`);
      if ([404, 410].includes(response.status)) return true;
      if (!response.ok) return false;
      const value = await response.json() as ChildDetail;
      return value.identity.status === "deleted";
    }, { timeout: 30_000 }).toBe(true);
    await page.reload();
    await expect(page).toHaveURL(`${daemon.baseUrl}${childPath}`);
    await expect(page.getByText(/deleted|no longer available|not found/i).first()).toBeVisible();
    await expect(page.locator(".composer .send-button")).toHaveCount(0);
    await page.goto(parentUrl);
    await expect(page.locator(".message-list")).toContainText(PARENT_BRIEF);
    expect((await json<TaskStatus>(daemon, statusPath)).status).toBe("completed");
    expect((await json<{ task: { output_preview: string } }>(daemon, taskOutputPath)).task.output_preview)
      .toBe(completedOutput.task.output_preview);
    expect(provider.calls).toEqual(expect.arrayContaining([
      "InvokeAgent", "ExecCommand", "WaitFor", "GetAgent", "DeleteAgent",
    ]));
    expect(provider.failures).toEqual([]);
    await cold.close();
  } finally {
    await testInfo.attach("scripted-provider-tools", {
      body: Buffer.from(JSON.stringify({ calls: provider.calls, failures: provider.failures }, null, 2)),
      contentType: "application/json",
    });
    await provider.stop();
  }
});

test("pinned conversation rejects a first summary fetched after same-name recreation without reload", async ({
  daemonFactory,
  page,
}, testInfo) => {
  test.setTimeout(120_000);
  const targetId = "e2e-summary-race-target";
  const provider = await startDelegationProvider();
  let releaseSummary!: () => void;
  const summaryGate = new Promise<void>((resolve) => { releaseSummary = resolve; });
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    await authenticate(page, daemon.token);
    const createTarget = async () => {
      const response = await daemon.api(`/control/agents/${targetId}/create`, {
        method: "POST",
        body: JSON.stringify({ template: null }),
      });
      expect(response.status, await response.text()).toBe(200);
      return await json<ChildDetail>(daemon, `/control/agents/${targetId}/detail`);
    };
    const original = await createTarget();
    let summaryStarted = false;
    let deliveredSummary = "";
    let deliveredSummaryStatus = 0;
    await page.route((url) => url.pathname === `/api/agents/${targetId}/conversation`, async (route) => {
      summaryStarted = true;
      await summaryGate;
      // Fetch only after recreation: the server must reject the old binding
      // even though its access preflight succeeded before replacement.
      const response = await route.fetch();
      deliveredSummary = await response.text();
      deliveredSummaryStatus = response.status();
      await route.fulfill({ response });
    }, { times: 1 });
    const pinnedUrl = `${daemon.baseUrl}/agents/${targetId}/conversation?incarnation=${original.identity.incarnation}`;
    await page.goto(pinnedUrl, { waitUntil: "domcontentloaded" });
    await expect.poll(() => summaryStarted).toBe(true);
    await page.evaluate((markers) => {
      const observations: string[] = [];
      Object.assign(window, { identityRaceLeaks: observations });
      new MutationObserver(() => {
        const text = document.body.textContent ?? "";
        for (const marker of markers) {
          if (text.includes(marker)) observations.push(marker);
        }
      }).observe(document.body, { subtree: true, childList: true, characterData: true });
    }, [OPERATOR_INPUT, OPERATOR_BRIEF]);

    const deleted = await daemon.api(`/control/agents/${targetId}`, {
      method: "DELETE",
      body: JSON.stringify({}),
    });
    expect(deleted.status, await deleted.text()).toBe(200);
    await expect.poll(async () => {
      const response = await daemon.api(`/control/agents/${targetId}/detail`);
      if (response.status === 410) return true;
      if (!response.ok) return false;
      return ((await response.json()) as ChildDetail).identity.status === "deleted";
    }, { timeout: 30_000 }).toBe(true);
    const recreated = await createTarget();
    expect(recreated.identity.incarnation).toBeGreaterThan(original.identity.incarnation);
    await prompt(daemon, targetId, OPERATOR_INPUT);
    await expect.poll(async () => {
      const response = await daemon.api(`/agents/${targetId}/conversation`);
      return await response.text();
    }, { timeout: 30_000 }).toContain(OPERATOR_INPUT);
    releaseSummary();

    await expect.poll(() => deliveredSummaryStatus).toBe(409);
    expect(JSON.parse(deliveredSummary)).toMatchObject({
      ok: false,
      code: "conversation_identity_changed",
      retryable: false,
    });
    expect(deliveredSummary).not.toContain(OPERATOR_INPUT);
    await expect(page.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("This link belongs to an earlier agent identity", { exact: true }))
      .toBeVisible();
    await expect(page).toHaveURL(pinnedUrl);
    await expect(page.locator(".message-list")).toHaveCount(0);
    await expect(page.locator(".composer textarea")).toHaveCount(0);
    expect(await page.evaluate(() =>
      (window as Window & { identityRaceLeaks?: string[] }).identityRaceLeaks)).toEqual([]);
    expect(provider.failures).toEqual([]);
  } finally {
    releaseSummary();
    await testInfo.attach("scripted-provider-tools", {
      body: Buffer.from(JSON.stringify({ calls: provider.calls, failures: provider.failures }, null, 2)),
      contentType: "application/json",
    });
    await provider.stop();
  }
});

test("existing-agent wait links keep their original identity after deletion and same-name recreation", async ({
  daemonFactory,
  page,
}, testInfo) => {
  test.setTimeout(120_000);
  const targetId = "e2e-message-wait-target";
  const provider = await startDelegationProvider(targetId);
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    await authenticate(page, daemon.token);
    await page.setViewportSize({ width: 1600, height: 1000 });
    const missingUrl = `${daemon.baseUrl}/agents/e2e-missing-target/conversation?incarnation=1`;
    await page.goto(missingUrl);
    await expect(page.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("Agent not found", { exact: true })).toBeVisible();
    await expect(page).toHaveURL(missingUrl);
    await expect(page.locator(".composer textarea")).toHaveCount(0);

    const createTarget = async () => {
      const response = await daemon.api(`/control/agents/${targetId}/create`, {
        method: "POST",
        body: JSON.stringify({ template: null }),
      });
      expect(response.status, await response.text()).toBe(200);
      return await json<ChildDetail>(daemon, `/control/agents/${targetId}/detail`);
    };
    const original = await createTarget();
    const target = {
      kind: "agent_message_wait",
      agent_id: targetId,
      incarnation: original.identity.incarnation,
    };
    const parentUrl = `${daemon.baseUrl}/agents/${daemon.agentId}/conversation`;
    await page.goto(parentUrl);
    await prompt(daemon, daemon.agentId, PARENT_INPUT);
    let task: TaskRecord | undefined;
    await expect.poll(async () => {
      const tasks = await json<TaskRecord[]>(daemon, `/agents/${daemon.agentId}/tasks`);
      task = tasks.find((candidate) => candidate.kind === "agent_message_wait");
      return task?.target;
    }, { timeout: 30_000 }).toEqual(target);
    expect(task!.target).toEqual(target);
    const statusPath = `/agents/${daemon.agentId}/tasks/${task!.id}`;
    expect((await json<TaskStatus>(daemon, statusPath)).target).toEqual(target);
    const targetPath = `/agents/${targetId}/conversation?incarnation=${target.incarnation}`;
    await openContextPanel(page);
    const targetLink = page.locator(`a[href="${targetPath}"]`).first();
    await expect(targetLink).toHaveAccessibleName("Open agent conversation");
    await page.locator(".task-button").first().click();
    await expect(targetLink).toBeVisible();
    await targetLink.click();
    await expect(page).toHaveURL(`${daemon.baseUrl}${targetPath}`);
    await expect(page.locator(".message-list")).toContainText(CHILD_INPUT);
    await expect(page.locator(".message-list")).toContainText(CHILD_PROGRESS);
    await page.reload();
    await expect(page.locator(".message-list")).toContainText(CHILD_INPUT);

    provider.release();
    await expect(page.locator(".message-list")).toContainText(CHILD_BRIEF, { timeout: 30_000 });
    await expect.poll(async () => (await json<TaskStatus>(daemon, statusPath)).status).toBe("completed");
    // Check the actual parent continuation, not just durable task completion,
    // before asserting that its retained delivery is available.
    await page.goto(parentUrl);
    await expect(page.locator(".message-list")).toContainText(PARENT_BRIEF, { timeout: 30_000 });
    const outputPath = `${statusPath}/output?block=false`;
    const output = await json<{ task: { output_preview: string } }>(daemon, outputPath);
    expect(output.task.output_preview).toContain(CHILD_BRIEF);
    await openContextPanel(page);
    await page.getByText("Recently completed (up to 20)", { exact: true }).click();
    await expect(targetLink).toBeVisible();
    await targetLink.click();

    // Real lifecycle operations: no mocked detail, task, or conversation API.
    const deleted = await daemon.api(`/control/agents/${targetId}`, {
      method: "DELETE",
      body: JSON.stringify({}),
    });
    expect(deleted.status, await deleted.text()).toBe(200);
    await expect.poll(async () => {
      const response = await daemon.api(`/control/agents/${targetId}/detail`);
      if (response.status === 410) return true;
      if (!response.ok) return false;
      return ((await response.json()) as ChildDetail).identity.status === "deleted";
    }, { timeout: 30_000 }).toBe(true);
    await page.reload();
    await expect(page.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("This agent has been deleted. Its parent task result is still available.", { exact: true }))
      .toBeVisible();
    await expect(page.locator(".composer textarea")).toHaveCount(0);

    const recreated = await createTarget();
    expect(recreated.identity.incarnation).toBeGreaterThan(original.identity.incarnation);
    await prompt(daemon, targetId, OPERATOR_INPUT);
    // Check the uncached API independently of the browser's earlier 410.
    const recreatedState = await daemon.api(`/agents/${targetId}/state`);
    expect(recreatedState.status, await recreatedState.text()).toBe(200);
    await page.reload();
    await expect(page).toHaveURL(`${daemon.baseUrl}${targetPath}`);
    await expect(page.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("This link belongs to an earlier agent identity", { exact: true })).toBeVisible();
    await expect(page.locator(".composer textarea")).toHaveCount(0);
    await expect(page.locator(".message-list")).toHaveCount(0);

    // Explicitly opening the unpinned canonical route may reach the new
    // identity, without changing the stored invocation target or task result.
    // Historical conversation retention is not an incarnation isolation contract.
    await page.goto(`${daemon.baseUrl}/agents/${targetId}/conversation`);
    await expect(page.locator(".message-list")).toContainText(OPERATOR_BRIEF, { timeout: 30_000 });
    expect((await json<TaskStatus>(daemon, statusPath)).target).toEqual(target);
    expect((await json<{ task: { output_preview: string } }>(daemon, outputPath)).task.output_preview)
      .toBe(output.task.output_preview);
    await page.goto(parentUrl);
    await openContextPanel(page);
    const recent = page.getByText("Recently completed (up to 20)", { exact: true });
    if (!await targetLink.isVisible()) await recent.click();
    await expect(targetLink).toBeVisible();
    await targetLink.click();
    await expect(page.getByRole("region", { name: "Agent conversation", exact: true })
      .getByText("This link belongs to an earlier agent identity", { exact: true })).toBeVisible();
    expect(provider.failures).toEqual([]);
  } finally {
    await testInfo.attach("scripted-provider-tools", {
      body: Buffer.from(JSON.stringify({ calls: provider.calls, failures: provider.failures }, null, 2)),
      contentType: "application/json",
    });
    await provider.stop();
  }
});

import http from "node:http";
import type { Server } from "node:http";
import net from "node:net";

import { expect, test } from "./daemon-fixture";

interface HangingProvider {
  server: Server;
  baseUrl: string;
  requests: Array<{ url: string }>;
  stop(): Promise<void>;
}

async function reservePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate a provider port"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

async function startHangingProvider(): Promise<HangingProvider> {
  const port = await reservePort();
  const requests: Array<{ url: string }> = [];
  const server = http.createServer((req, res) => {
    requests.push({ url: `${req.method ?? ""} ${req.url ?? ""}` });
    // Intentionally never respond: provider turns (and the builtin web search
    // probe issued during turn context build) stay in flight until the
    // runtime aborts the run and cancels the request.
    res.writeHead(200, { "Content-Type": "text/event-stream" });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
  return {
    server,
    baseUrl: `http://127.0.0.1:${port}/v1`,
    requests,
    async stop() {
      // The daemon keeps hung provider sockets pooled; close() alone would
      // wait for them forever, so drop the connections explicitly.
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

interface AgentRuntimeState {
  agent: {
    agent: {
      status: string;
      current_run_id: string | null;
      pending: number;
    };
  };
}

interface AgentEventPage {
  events?: Array<{ event_seq?: number; type?: string; payload?: { run_id?: string; reason?: string } }>;
}

interface DaemonApi {
  api(pathname: string, init?: RequestInit): Promise<Response>;
}

async function agentState(daemon: DaemonApi, agentId: string): Promise<AgentRuntimeState> {
  return await daemon.api(`/agents/${agentId}/state`).then((response) => response.json() as Promise<AgentRuntimeState>);
}

test("turn-scoped abort settles the current run and keeps the agent schedulable against a real daemon", async ({
  daemonFactory,
}) => {
  const provider = await startHangingProvider();
  const daemon = await daemonFactory({
    webDist: "dist-e2e",
    env: {
      HOLON_OPENAI_BASE_URL: provider.baseUrl,
      OPENAI_API_KEY: "e2e-provider-key",
    },
  });
  const agentId = daemon.agentId;

  try {
    // A prompt starts a real run whose provider turn (or context-build probe)
    // hangs against the frozen provider.
    const prompt = await daemon.api(`/control/agents/${agentId}/prompt`, {
      method: "POST",
      body: JSON.stringify({ text: "first turn that must be stopped" }),
    });
    expect(prompt.status).toBe(200);

    await expect.poll(async () => {
      const state = await agentState(daemon, agentId);
      return state.agent.agent.current_run_id;
    }, { timeout: 20_000 }).not.toBeNull();
    const runId = (await agentState(daemon, agentId)).agent.agent.current_run_id as string;
    await expect.poll(() => provider.requests.length, { timeout: 20_000 }).toBeGreaterThanOrEqual(1);

    // The turn-scoped abort carries the run id and settles quickly.
    const abort = await daemon.api(`/control/agents/${agentId}/current-run/abort`, {
      method: "POST",
      body: JSON.stringify({
        run_id: runId,
        mode: "idle_after_abort",
        authority_class: "operator_instruction",
      }),
    });
    expect(abort.status).toBe(200);
    await expect.poll(async () => {
      const page = await daemon.api(`/agents/${agentId}/events?limit=100&order=desc`)
        .then((response) => response.json() as Promise<AgentEventPage>);
      return (page.events ?? []).filter((event) =>
        event.type === "message_processing_aborted"
        || event.type === "turn_terminal_aborted"
        || event.type === "turn_terminal").length;
    }, { timeout: 20_000 }).toBeGreaterThanOrEqual(2);
    await expect.poll(async () => (await agentState(daemon, agentId)).agent.agent.current_run_id, { timeout: 20_000 }).toBeNull();
    expect((await agentState(daemon, agentId)).agent.agent.status).not.toBe("stopped");

    // The abort is turn-scoped, not a lifecycle stop: the very next prompt is
    // admitted without a start and begins a fresh provider turn.
    const providerRequestsAfterAbort = provider.requests.length;
    const secondPrompt = await daemon.api(`/control/agents/${agentId}/prompt`, {
      method: "POST",
      body: JSON.stringify({ text: "second turn after the turn-scoped abort" }),
    });
    expect(secondPrompt.status).toBe(200);
    await expect.poll(async () => (await agentState(daemon, agentId)).agent.agent.current_run_id, { timeout: 20_000 }).not.toBeNull();
    await expect.poll(() => provider.requests.length, { timeout: 20_000 }).toBeGreaterThan(providerRequestsAfterAbort);

    // A stale run id conflicts instead of silently aborting the new run.
    const staleAbort = await daemon.api(`/control/agents/${agentId}/current-run/abort`, {
      method: "POST",
      body: JSON.stringify({
        run_id: runId,
        mode: "idle_after_abort",
        authority_class: "operator_instruction",
      }),
    });
    expect(staleAbort.status).toBe(409);

    // Clean up the second run so the daemon can settle before teardown.
    const secondRunId = (await agentState(daemon, agentId)).agent.agent.current_run_id;
    if (secondRunId) {
      const cleanup = await daemon.api(`/control/agents/${agentId}/current-run/abort`, {
        method: "POST",
        body: JSON.stringify({ run_id: secondRunId, mode: "idle_after_abort" }),
      });
      expect(cleanup.status).toBe(200);
    }
  } finally {
    await provider.stop();
  }
});

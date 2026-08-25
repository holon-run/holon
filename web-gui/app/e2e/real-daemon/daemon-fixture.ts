import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";

import { expect, test as base } from "@playwright/test";

const DEFAULT_AGENT_ID = "default";
const START_TIMEOUT_MS = 30_000;
const STOP_TIMEOUT_MS = 10_000;

interface DaemonController {
  readonly agentId: string;
  readonly baseUrl: string;
  readonly token: string;
  api(pathname: string, init?: RequestInit): Promise<Response>;
  enqueue(text: string): Promise<string>;
  restart(): Promise<void>;
  stop(): Promise<void>;
  start(): Promise<void>;
}

interface DaemonOptions {
  webDist: string;
}

type Fixtures = {
  artifacts: void;
  daemonFactory: (options: DaemonOptions) => Promise<DaemonController>;
};

async function reservePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate a daemon port"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

function waitForExit(child: ChildProcessWithoutNullStreams, timeoutMs: number): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(new Error(`daemon did not exit within ${timeoutMs}ms`));
    }, timeoutMs);
    const cleanup = () => {
      clearTimeout(timeout);
      child.off("exit", onExit);
    };
    const onExit = () => {
      cleanup();
      resolve();
    };
    child.once("exit", onExit);
  });
}

async function waitForReady(
  baseUrl: string,
  token: string,
  child: ChildProcessWithoutNullStreams,
): Promise<void> {
  const deadline = Date.now() + START_TIMEOUT_MS;
  let lastError: unknown;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`daemon exited before readiness with code ${child.exitCode}`);
    }
    try {
      const response = await fetch(`${baseUrl}/api/handshake`, {
        headers: { Authorization: `Bearer ${token}` },
        signal: AbortSignal.timeout(1_000),
      });
      if (response.ok) return;
      lastError = new Error(`handshake returned ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`daemon readiness timed out: ${String(lastError)}`);
}

async function createDaemon(
  options: DaemonOptions,
  workerIndex: number,
): Promise<{ controller: DaemonController; cleanup: () => Promise<void> }> {
  const root = await mkdtemp(path.join(os.tmpdir(), "holon-web-e2e-"));
  const home = path.join(root, "home");
  const tokenFile = path.join(root, "control-token");
  const controlToken = randomBytes(24).toString("hex");
  const artifactDir = path.resolve("test-results", "real-daemon", `worker-${workerIndex}`);
  const stdoutPath = path.join(artifactDir, "daemon.stdout.log");
  const stderrPath = path.join(artifactDir, "daemon.stderr.log");
  const port = await reservePort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const binary = path.resolve(
    process.env.HOLON_E2E_DAEMON_BIN ?? "../../target/debug/holon",
  );
  let child: ChildProcessWithoutNullStreams | undefined;
  let agentCreated = false;

  await mkdir(home, { recursive: true });
  await mkdir(artifactDir, { recursive: true });
  await writeFile(tokenFile, `${controlToken}\n`, { mode: 0o600 });

  const start = async () => {
    if (child && child.exitCode === null && child.signalCode === null) {
      throw new Error("daemon is already running");
    }
    child = spawn(binary, [
      "serve",
      "--listen",
      `127.0.0.1:${port}`,
      "--token-file",
      tokenFile,
      "--web-dist",
      path.resolve(options.webDist),
    ], {
      env: {
        ...process.env,
        HOLON_HOME: home,
        HOLON_CONTROL_AUTH_MODE: "required",
        HOLON_CALLBACK_BASE_URL: baseUrl,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    child.stdout.pipe(createWriteStream(stdoutPath, { flags: "a" }));
    child.stderr.pipe(createWriteStream(stderrPath, { flags: "a" }));
    await waitForReady(baseUrl, controlToken, child);
    if (!agentCreated) {
      const response = await fetch(
        `${baseUrl}/api/control/agents/${DEFAULT_AGENT_ID}/create`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${controlToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({
            authority_class: "operator_instruction",
            template: null,
          }),
        },
      );
      if (!response.ok) {
        throw new Error(
          `agent bootstrap failed with ${response.status}: ${await response.text()}`,
        );
      }
      agentCreated = true;
    }
  };

  const stop = async () => {
    if (!child || child.exitCode !== null || child.signalCode !== null) return;
    const running = child;
    running.kill("SIGTERM");
    try {
      await waitForExit(running, STOP_TIMEOUT_MS);
    } catch {
      running.kill("SIGKILL");
      await waitForExit(running, STOP_TIMEOUT_MS);
    }
  };

  const api = async (pathname: string, init: RequestInit = {}) => {
    const headers = new Headers(init.headers);
    headers.set("Authorization", `Bearer ${controlToken}`);
    if (init.body && !headers.has("Content-Type")) {
      headers.set("Content-Type", "application/json");
    }
    return await fetch(`${baseUrl}/api${pathname}`, { ...init, headers });
  };

  const controller: DaemonController = {
    agentId: DEFAULT_AGENT_ID,
    baseUrl,
    token: controlToken,
    api,
    async enqueue(text: string) {
      const response = await api(`/agents/${DEFAULT_AGENT_ID}/enqueue`, {
        method: "POST",
        body: JSON.stringify({
          text,
          origin: { kind: "webhook", source: "web-e2e", event_type: "marker" },
        }),
      });
      if (!response.ok) {
        throw new Error(`enqueue failed with ${response.status}: ${await response.text()}`);
      }
      const body = await response.json() as { message_id: string };
      return body.message_id;
    },
    restart: async () => {
      await stop();
      await start();
    },
    start,
    stop,
  };

  try {
    await start();
  } catch (error) {
    await stop();
    await rm(root, { recursive: true, force: true });
    throw error;
  }
  return {
    controller,
    cleanup: async () => {
      await stop();
      await rm(root, { recursive: true, force: true });
    },
  };
}

export const test = base.extend<Fixtures>({
  artifacts: [async ({ page }, use, testInfo) => {
    const consoleMessages: string[] = [];
    const requestFailures: string[] = [];
    const errorResponses: string[] = [];
    page.on("console", (message) => {
      consoleMessages.push(`[${message.type()}] ${message.text()}`);
    });
    page.on("requestfailed", (request) => {
      requestFailures.push(
        `${request.method()} ${request.url()} ${request.failure()?.errorText ?? "unknown failure"}`,
      );
    });
    page.on("response", (response) => {
      if (response.status() >= 400) {
        errorResponses.push(
          `${response.status()} ${response.request().method()} ${response.url()}`,
        );
      }
    });

    await use();

    if (testInfo.status === testInfo.expectedStatus) return;
    await testInfo.attach("browser-console", {
      body: Buffer.from(consoleMessages.join("\n")),
      contentType: "text/plain",
    });
    await testInfo.attach("request-failures", {
      body: Buffer.from([...requestFailures, ...errorResponses].join("\n")),
      contentType: "text/plain",
    });
    const diagnostics = await page.evaluate(async () => {
      const bridge = window.__HOLON_E2E__;
      if (!bridge) return null;
      const snapshot = bridge.snapshot();
      const ledgers = await Promise.all(snapshot.agentIds.map(async (agentId) => ({
        agentId,
        ledger: await bridge.ledger(agentId),
        partitions: await bridge.ledgerPartitions(agentId),
      })));
      return { snapshot, ledgers };
    }).catch((error: unknown) => ({ error: String(error) }));
    await testInfo.attach("diagnostics", {
      body: Buffer.from(JSON.stringify(diagnostics, null, 2)),
      contentType: "application/json",
    });
  }, { auto: true }],
  daemonFactory: async ({}, use, workerInfo) => {
    const daemons: Array<() => Promise<void>> = [];
    await use(async (options) => {
      const daemon = await createDaemon(options, workerInfo.workerIndex);
      daemons.push(daemon.cleanup);
      return daemon.controller;
    });
    for (const cleanup of daemons.reverse()) await cleanup();
  },
});

export { expect };

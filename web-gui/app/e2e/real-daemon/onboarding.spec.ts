import http from "node:http";

import { expect, test } from "./daemon-fixture";

interface SuccessfulProvider {
  baseUrl: string;
  stop(): Promise<void>;
}

async function startSuccessfulProvider(): Promise<SuccessfulProvider> {
  const server = http.createServer((_request, response) => {
    response.writeHead(200, {
      "Content-Type": "application/json",
    });
    response.end(
      JSON.stringify({
        id: "resp_onboarding_e2e",
        status: "completed",
        output: [
          {
            type: "message",
            role: "assistant",
            content: [
              {
                type: "output_text",
                text: "onboarding first task marker",
              },
            ],
          },
        ],
        usage: { input_tokens: 1, output_tokens: 1 },
      }),
    );
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("failed to allocate the onboarding provider port");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    async stop() {
      server.closeAllConnections?.();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

async function installLocalToken(
  page: import("@playwright/test").Page,
  token: string,
): Promise<void> {
  await page.addInitScript((value) => {
    sessionStorage.setItem(
      "holon.webGui.activeRuntimeConnection.v1",
      JSON.stringify({
        mode: "local",
        token: value,
      }),
    );
  }, token);
}

test("fresh runtime can complete optional web onboarding and run its first task", async ({
  daemonFactory,
  page,
}) => {
  test.setTimeout(120_000);
  const provider = await startSuccessfulProvider();
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      createInitialAgent: false,
      env: {
        HOLON_OPENAI_BASE_URL: provider.baseUrl,
      },
    });
    // The daemon always retains its configured default Agent. Hide only that
    // initial roster projection so the real UI onboarding entry is exercised;
    // all create, model-config, and task requests still use the real daemon.
    let hideInitialRoster = true;
    await page.route("**/api/agents/list*", async (route) => {
      if (!hideInitialRoster) {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });
    await page.route("**/api/control/agents/*/create", async (route) => {
      hideInitialRoster = false;
      const response = await route.fetch();
      if (response.status() >= 400) {
        console.log(
          `agent create failed: ${response.status()} ${await response.text()}`,
        );
      }
      await route.fulfill({ response });
    });
    await installLocalToken(page, daemon.token);
    await page.goto(daemon.baseUrl);

    await expect(
      page
        .locator(".dashboard-empty")
        .filter({ hasText: /No agents|没有可见/ }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: /Create your first agent|创建首个智能体/ })
      .click();

    await expect(
      page.getByRole("dialog", {
        name: /Set up your first Agent|设置首个 Agent/,
      }),
    ).toBeVisible();
    await expect(page.getByText(/Step 1 of 2|第 1 步/)).toBeVisible();
    await page.getByRole("button", { name: /Next|下一步/ }).click();

    await expect(page.getByText(/Step 2 of 2|第 2 步/)).toBeVisible();
    await page.getByLabel(/Agent ID|智能体 ID/).fill("onboarding-agent");
    await page.getByRole("button", { name: /Create Agent|创建 Agent/ }).click();

    await expect(
      page.locator(".agent-row").filter({ hasText: "onboarding-agent" }),
    ).toBeVisible({
      timeout: 30_000,
    });
    await expect(page).toHaveURL(
      /\/agents\/onboarding-agent(?:\/conversation)?$/,
    );

    const enqueue = await daemon.api("/agents/onboarding-agent/enqueue", {
      method: "POST",
      body: JSON.stringify({
        text: "onboarding first task marker",
        origin: {
          kind: "webhook",
          source: "web-e2e",
          event_type: "onboarding",
        },
      }),
    });
    expect(enqueue.ok).toBe(true);
    await expect(
      page
        .locator(".conversation-brief:visible")
        .filter({ hasText: "onboarding first task marker" }),
    ).toBeVisible();
  } finally {
    await provider.stop();
  }
});

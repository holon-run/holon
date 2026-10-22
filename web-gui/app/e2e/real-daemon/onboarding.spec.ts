import { readFile, stat } from "node:fs/promises";
import http from "node:http";
import path from "node:path";

import { expect, test } from "./daemon-fixture";

interface RuntimeConfig {
  config_file_path: string;
  setup: {
    status: "ready" | "blocked";
    default_model: string | null;
    provider: string | null;
    actions: string[];
  };
  reload?: {
    state: string;
    requested_generation: number;
    completed_generation: number;
  };
}

async function startProvider() {
  const requests: string[] = [];
  const server = http.createServer((request, response) => {
    requests.push(`${request.method} ${request.url}`);
    response.writeHead(500, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: { message: "Setup must not invoke a model" } }));
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
    requests,
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
      JSON.stringify({ mode: "local", token: value }),
    );
    localStorage.setItem("holon.webGui.languageMode.v1", "en");
  }, token);
}

test("truly unconfigured runtime persists first-run setup without invoking a model", async ({
  daemonFactory,
  page,
}) => {
  test.setTimeout(120_000);
  const provider = await startProvider();
  try {
    const daemon = await daemonFactory({
      webDist: "dist-e2e",
      emptyConfig: true,
      env: { HOLON_OPENAI_BASE_URL: provider.baseUrl },
    });
    const config = async (): Promise<RuntimeConfig> => {
      const response = await daemon.api("/control/runtime/config");
      expect(response.ok).toBe(true);
      return await response.json() as RuntimeConfig;
    };
    const initial = await config();
    expect(initial.setup).toMatchObject({
      status: "blocked", default_model: "openai@default/unknown", provider: "openai",
    });
    expect(initial.setup.actions.length).toBeGreaterThan(0);
    expect(initial.config_file_path).toBe(path.join(daemon.home, "config.json"));
    await expect(readFile(initial.config_file_path, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    expect((await stat(path.join(daemon.home, "state", "runtime.sqlite"))).isFile()).toBe(true);

    await installLocalToken(page, daemon.token);
    // Leave the daemon's real roster intact. No API routes or roster projections
    // are replaced: setup is entered explicitly from Settings.
    await page.goto(`${daemon.baseUrl}/settings`);
    const setup = page.getByRole("region", { name: "First-run setup", exact: true });
    await expect(setup).toBeInViewport();
    await expect(setup.getByText("Setup is incomplete", { exact: true })).toBeVisible();
    await setup.getByRole("button", {
      name: initial.setup.actions[0] === "configure_model_credential"
        ? "Configure model credentials" : "Choose a default model",
      exact: true,
    }).click();
    const defaultModel = page.locator("#setup-default-model .model-select");
    await expect(defaultModel.getByRole("button", { name: /^Default model/ })).toBeVisible();

    // The setup action opens the matching provider editor directly;
    // all traffic stays loopback and no credentials are inherited from the host.
    const editor = page.locator("form#setup-provider-editor");
    await expect(editor).toBeInViewport();
    const key = editor.getByLabel("API Key", { exact: true });
    await expect(key).toHaveValue("");
    await editor.getByRole("button", { name: "Save openai", exact: true }).click();
    await expect(page.getByText(/Saved, but setup is incomplete/)).toBeVisible({ timeout: 15_000 });
    expect((await config()).setup.status).toBe("blocked");
    await expect(setup.getByText("Configuration ready", { exact: true })).toHaveCount(0);
    expect(provider.requests).toEqual([]);

    await key.fill("e2e-fake-onboarding-key");
    await editor.getByRole("button", { name: "Save openai", exact: true }).click();
    await expect(key).toHaveValue("");
    // The existing loader resolves an implicit default after authentication.
    // Readiness follows that effective route, without a live provider probe.
    await expect.poll(async () => (await config()).setup.status).toBe("ready");
    const authenticatedModel = (await config()).setup.default_model;
    expect(authenticatedModel).toBeTruthy();
    expect(authenticatedModel).not.toBe(initial.setup.default_model);

    // The Settings card opens the Agent wizard only after configuration is ready.
    // Select and persist the first model through the real Settings form first.
    await defaultModel.getByRole("button", { name: /^Default model/ }).click();
    const initialChoice = defaultModel.locator(
      'button[data-model-choice][title^="openai/"], button[data-model-choice][title^="openai@"]',
    ).first();
    await expect(initialChoice).toBeEnabled();
    const initialModel = await initialChoice.getAttribute("title");
    expect(initialModel).toBeTruthy();
    await initialChoice.click();
    expect((await config()).setup.default_model).toBe(authenticatedModel);
    await page.locator("form").filter({ has: defaultModel })
      .getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(async () => (await config()).setup).toMatchObject({
      status: "ready", default_model: initialModel, provider: "openai", actions: [],
    });
    await expect(setup.getByText("Configuration ready", { exact: true })).toBeVisible();
    await setup.getByRole("button", { name: "Set up your first Agent", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Set up your first Agent", exact: true });
    await expect(dialog.getByText("Step 1 of 2", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: /^Default model/ }).click();
    const choice = dialog.locator(
      'button[data-model-choice][title^="openai/"], button[data-model-choice][title^="openai@"]',
    ).last();
    // title carries the canonical model route, avoiding a dependency on catalog
    // display-name churn while still making an explicit UI choice.
    await expect(choice).toBeEnabled();
    const model = await choice.getAttribute("title");
    expect(model).toBeTruthy();
    expect(model).not.toBe(initialModel);
    await choice.click();
    expect((await config()).setup.default_model).toBe(initialModel);
    await dialog.getByRole("button", { name: "Next", exact: true }).click();
    await expect(dialog.getByText("Step 2 of 2", { exact: true })).toBeVisible();
    await expect.poll(async () => (await config()).setup).toMatchObject({
      status: "ready", default_model: model, provider: "openai", actions: [],
    });

    const name = dialog.getByLabel("Agent name", { exact: true });
    await expect(name).toHaveValue("Holon Bot");
    await name.fill("My Holon Bot");
    await expect(name).toHaveValue("My Holon Bot");
    await name.fill("Holon Bot");
    const agentId = dialog.getByLabel("Agent ID", { exact: true });
    await expect(agentId).not.toBeVisible();
    await dialog.locator("summary").filter({ hasText: "Advanced: Agent ID and template" }).click();
    await expect(agentId).toHaveValue("holon-bot");
    await expect(dialog.getByRole("combobox", { name: "Template", exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: "Create Agent", exact: true }).click();
    await expect(page).toHaveURL(/\/agents\/holon-bot(?:\/conversation)?$/);
    await expect(page.locator(".agent-row").filter({ hasText: "Holon Bot" })).toBeVisible();

    const roster = async () => {
      const response = await daemon.api("/agents/list");
      expect(response.ok).toBe(true);
      return await response.json() as Array<{ identity: { agent_id: string; name?: string } }>;
    };
    await expect.poll(roster).toEqual(expect.arrayContaining([
      expect.objectContaining({
        identity: expect.objectContaining({ agent_id: "holon-bot", name: "Holon Bot" }),
      }),
    ]));
    const persisted = JSON.parse(await readFile(initial.config_file_path, "utf8"));
    expect(persisted.model.default).toBe(model);
    expect(persisted.providers.openai.base_url).toBe(provider.baseUrl);

    await page.reload();
    await expect(page).toHaveURL(/\/agents\/holon-bot(?:\/conversation)?$/);
    await expect(page.locator(".agent-row").filter({ hasText: "Holon Bot" })).toBeVisible();
    await page.goto(`${daemon.baseUrl}/settings`);
    await expect(setup.getByText("Configuration ready", { exact: true })).toBeVisible();
    await expect.poll(async () => (await config()).setup.status).toBe("ready");
    // This is configuration validation, not a live model test or first task.
    expect(provider.requests).toEqual([]);
  } finally {
    await provider.stop();
  }
});

import { expect, test, type Page } from "@playwright/test";
import { sessionFor } from "./test-session";

const primary = "openai@default/shared-model";
const alternative = "dashscope@coding/shared-model";
const provider = (id: string, configured: boolean) => ({ id, transport: "openai_responses", base_url: "https://example.test/v1", api_key_supported: true, oauth_supported: false, credential_source: "credential_profile", credential_kind: "api_key", credential_profile: `${id}:default`, credential_configured: configured, configured_in_config: configured });
const providers = [provider("openai", true), provider("dashscope-coding-plan", true), provider("deepseek", false), ...Array.from({ length: 40 }, (_, i) => provider(`custom-${i}`, false))];
const models = [
  { model: "openai/shared-model", provider: "openai", provider_family: "openai", route_provider: "openai", endpoint: "default", display_name: "Shared Model", available: true, policy: { capabilities: { image_input: true, image_generation: true } } },
  { model: "dashscope/shared-model", provider: "dashscope", provider_family: "dashscope", route_provider: "dashscope-coding-plan", endpoint: "coding", display_name: "Shared Model", available: true },
  { model: "deepseek/hidden-model", provider: "deepseek", endpoint: "default", display_name: "Unavailable Model", available: false, unavailable_reason: "Missing credentials" },
];
async function setup(page: Page) {
  let selection = primary;
  let source = "runtime_default";
  const posts: unknown[] = [];
  const updates: { key: string; value?: unknown }[] = [];
  const modelState = () => ({ active_model: primary, effective_model: selection, runtime_default_model: primary, source });
  await page.route("**/api/models", (route) => route.fulfill({ json: { available_models: models.slice(0, 2), model_availability: models } }));
  await page.route("**/api/control/runtime/config", async (route) => {
    if (route.request().method() === "PATCH") updates.push(...route.request().postDataJSON().updates);
    await route.fulfill({ json: { ok: true, changed: true, runtime_surface: { model_default: primary, model_fallbacks: [], providers, web_search_providers: [] } } });
  });
  await page.route("**/api/agents/snapshot", async (route) => {
    const response = await route.fetch(); const roster = await response.json();
    for (const entry of roster.agents) { entry.agent.model = modelState(); entry.agent.current_run_id = "active-run"; }
    await route.fulfill({ json: roster });
  });
  await page.route("**/api/agents/bootstrap-agent/projection-snapshot", async (route) => {
    const response = await route.fetch(); const snapshot = await response.json();
    if (snapshot.projection) { snapshot.projection.agent.model = modelState(); snapshot.projection.agent.current_run_id = "active-run"; }
    await route.fulfill({ json: snapshot, status: response.status() });
  });
  await page.route("**/api/agents/bootstrap-agent/state", async (route) => {
    const response = await route.fetch();
    const state = await response.json();
    state.agent.model = modelState();
    state.session.current_run_id = "active-run";
    await route.fulfill({ json: state });
  });
  await page.route("**/api/control/agents/bootstrap-agent/model", async (route) => {
    const body = route.request().postDataJSON(); posts.push(body); selection = body.model; source = "agent_override";
    await route.fulfill({ json: { model: modelState() } });
  });
  await page.route("**/api/control/agents/bootstrap-agent/model/clear", async (route) => {
    selection = primary; source = "runtime_default";
    await route.fulfill({ json: { model: modelState() } });
  });
  return { posts, updates };
}
test.beforeEach(async ({ context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info, "models"), domain: "127.0.0.1", path: "/" }]);
});

test("provider directory searches aliases, edits one service and preserves drafts", async ({ page }) => {
  await setup(page);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto("/settings");
  const directory = page.locator(".provider-directory");
  await expect(directory.locator("[data-provider]")).toHaveCount(2);
  await page.screenshot({ path: "/tmp/holon-model-settings.png" });
  await expect(page.locator(".model-services-card .settings-provider-editor")).toHaveCount(0);
  await page.getByRole("button", { name: "Connect a service", exact: true }).click();
  await expect(directory.locator("[data-provider]")).toHaveCount(43);
  await page.getByRole("textbox", { name: "Search service name or alias…" }).fill("百炼");
  await expect(directory.locator("[data-provider]")).toHaveCount(1);
  await directory.locator("[data-provider]").click();
  await expect(page.locator(".model-services-card .settings-provider-editor")).toHaveCount(1);
  const key = page.locator(".model-services-card .settings-provider-editor").getByLabel("API Key", { exact: true });
  await key.fill("test-draft-only");
  await page.getByRole("button", { name: "Back to services" }).click();
  await directory.locator("[data-provider]").click();
  await expect(key).toHaveValue("test-draft-only");
  await page.getByRole("button", { name: "Back to services" }).click();
  await expect(directory.locator("[data-provider]")).toBeFocused();
  await page.screenshot({ path: "/tmp/holon-model-services.png", fullPage: true });
});

test("model search keeps routes distinct, persists favorites and next-run selection", async ({ page }) => {
  const { posts } = await setup(page);
  await page.goto("/agents/bootstrap-agent/conversation");
  await page.locator(".model-button").click();
  const dialog = page.getByRole("dialog", { name: "Switch agent model" });
  await expect(dialog.getByRole("textbox", { name: "Search models or services…" })).toBeFocused();
  await expect(dialog.getByText("Unavailable Model", { exact: true })).toHaveCount(0);
  await dialog.getByRole("textbox").fill("shared");
  await expect(dialog.locator("[data-model-choice]")).toHaveCount(2);
  const alternativeRow = dialog.locator(".model-browser-row").filter({ hasText: "Coding Plan" });
  await alternativeRow.getByRole("button", { name: /^Favorite / }).click();
  await alternativeRow.getByRole("button", { name: /^Details for / }).click();
  await expect(alternativeRow.locator("input[readonly]")).toHaveValue(alternative);
  await alternativeRow.locator("[data-model-choice]").click();
  await expect.poll(() => posts.length).toBe(1);
  expect(posts[0]).toMatchObject({ model: alternative });
  await page.reload();
  await page.locator(".model-button").click();
  await expect(dialog.locator(".model-next-selection")).toHaveAttribute("title", alternative);
  await expect(dialog.getByRole("button", { name: /^Unfavorite / })).toHaveCount(1);
  await page.screenshot({ path: "/tmp/holon-model-picker.png" });
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(page.locator(".model-button")).toBeFocused();
  await page.locator(".model-button").click();
  await dialog.getByRole("button", { name: /Runtime default/ }).click();
  await page.locator(".model-button").click();
  await expect(dialog.getByRole("button", { name: /Runtime default/ })).toBeDisabled();
});

test("global defaults and image settings share search with capability filtering", async ({ page }) => {
  const { updates } = await setup(page);
  await page.goto("/settings");
  await page.locator(".model-select-trigger").first().click();
  const panel = page.locator(".model-select-panel");
  await panel.getByRole("textbox", { name: "Search models or services…" }).fill("百炼");
  await panel.locator("[data-model-choice]").click();
  await page.locator(".settings-model-defaults").getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => updates.find((update) => update.key === "model.default")?.value).toBe(alternative);
  await page.getByRole("tab", { name: "Image", exact: true }).click();
  await page.locator(".model-select-trigger").filter({ visible: true }).first().click();
  await expect(page.locator(".model-select-panel").filter({ visible: true }).locator("[data-model-choice]")).toHaveCount(1);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator(".model-select-panel").filter({ visible: true }).getByRole("textbox", { name: "Search models or services…" })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: "/tmp/holon-model-settings-mobile.png", fullPage: true });
});

test("favorites synchronize between tabs and remain shortcuts rather than availability filters", async ({ page, context }) => {
  await setup(page);
  await page.goto("/agents/bootstrap-agent/conversation");
  await page.locator(".model-button").click();
  const second = await context.newPage();
  await setup(second);
  await second.goto("/agents/bootstrap-agent/conversation");
  await second.locator(".model-button").click();
  const firstPicker = page.locator(".model-menu");
  const secondPicker = second.locator(".model-menu");
  await firstPicker.getByRole("textbox", { name: "Search models or services…" }).fill("百炼");
  await firstPicker.getByRole("button", { name: /^Favorite / }).click();
  await expect(secondPicker.getByRole("button", { name: /^Unfavorite / })).toHaveCount(1);
  await secondPicker.getByRole("textbox", { name: "Search models or services…" }).fill("OpenAI");
  await expect(secondPicker.locator("[data-model-choice]")).toHaveCount(1);
  await second.close();
});

test("failed model changes keep selection and do not enter recent history", async ({ page }) => {
  await setup(page);
  await page.route("**/api/control/agents/bootstrap-agent/model", (route) => route.fulfill({ status: 400, json: { error: "Model route rejected" } }));
  await page.goto("/agents/bootstrap-agent/conversation");
  await page.locator(".model-button").click();
  const picker = page.locator(".model-menu");
  await picker.getByRole("textbox", { name: "Search models or services…" }).fill("百炼");
  await picker.locator("[data-model-choice]").click();
  await expect(picker.locator("[data-model-choice]")).toBeEnabled();
  expect(await page.evaluate(() => Object.entries(localStorage).some(([key, value]) => key.startsWith("holon.webGui.modelPreferences.") && JSON.parse(value).recent.length > 0))).toBe(false);
  await picker.getByRole("textbox", { name: "Search models or services…" }).fill("OpenAI");
  await expect(picker.locator("[data-model-choice]")).toHaveAttribute("aria-pressed", "true");
});

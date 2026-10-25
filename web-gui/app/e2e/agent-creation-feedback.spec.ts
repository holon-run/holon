import { expect, test, type Page } from "@playwright/test";
import { sessionFor } from "./test-session";

function bootstrap(status: "pending" | "succeeded" | "failed") {
  const step = { status, attempts: 1, updated_at: "2026-10-25T00:00:00Z", last_error: "SQL /home/private credential=secret" };
  return {
    status: status === "succeeded" ? "ready" : "degraded",
    template: step, runtime: step, workspace: step, model: step, initial_message: step,
  };
}

async function openCreate(page: Page, id: string) {
  await page.getByRole("button", { name: "Create agent from template", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Create agent from template" });
  await dialog.getByRole("textbox", { name: "Agent ID" }).fill(id);
  await dialog.getByRole("combobox", { name: "Template" }).selectOption("test-template");
  return dialog;
}

test.beforeEach(async ({ page, context }, info) => {
  await context.addCookies([{
    name: "holon_e2e_session", value: sessionFor(info, "creation"),
    domain: "127.0.0.1", path: "/",
  }]);
  await page.route("**/api/templates/catalog", (route) => route.fulfill({ json: {
    catalog: [{ catalog_id: "test-template", template: "test-template", name: "Test template" }],
  } }));
});

for (const outcome of ["ready", "degraded", "lost-job"] as const) {
  test(`202 navigates before initialization; ${outcome} remains separate from creation`, async ({ page, request }, info) => {
    const session = sessionFor(info, "creation");
    const id = "new-feedback-agent";
    let posted = 0;
    let finished = false;
    const state = () => bootstrap(!finished ? "pending" : outcome === "degraded" ? "failed" : "succeeded");
    await page.route(`**/api/control/agents/${id}/detail`, async (route) => {
      const response = await route.fetch();
      await route.fulfill({ response, json: { ...await response.json(), bootstrap: state() } });
    });
    await page.route("**/api/jobs/job-create", (route) => outcome === "lost-job" && finished
      ? route.fulfill({ status: 404, json: { error: "lost in-memory job" } })
      : route.fulfill({ json: { job: { id: "job-create", status: !finished ? "running" : outcome === "degraded" ? "failed" : "completed" } } }));
    await page.route(`**/api/control/agents/${id}/create`, async (route) => {
      posted += 1;
      await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
        data: { visibleAgentIds: ["bootstrap-agent", id] },
      });
      await route.fulfill({ status: 202, json: {
        identity: { agent_id: id, incarnation: 1, status: "active" },
        receipt: { receipt_id: "receipt-1", agent_id: id, stage: "provisioning", job_id: "job-create", bootstrap: state() },
      } });
    });
    await page.goto("/");
    const dialog = await openCreate(page, id);
    await dialog.getByRole("button", { name: "Create", exact: true }).click();
    await expect(dialog).not.toBeVisible();
    await expect(page).toHaveURL(new RegExp(`/agents/${id}/conversation\\?incarnation=1`));
    await expect(page.getByText("Created — initializing", { exact: true })).toBeVisible();
    expect(posted).toBe(1);
    finished = true;
    await expect(page.getByText(outcome === "degraded" ? "Created — initialization incomplete" : "Created — initialization complete", { exact: true })).toBeVisible();
    await expect(page.locator(".agent-creation-status")).not.toContainText("SQL");
    await expect(page.locator(".agent-creation-status")).not.toContainText("/home/private");
    if (outcome !== "degraded") {
      await request.post(`/__e2e__/conversation?session=${encodeURIComponent(session)}`, { data: {
        agentId: id,
        turns: [{
          turn_id: "budget-failed", key: { turn_index: 1, turn_id: "budget-failed" }, revision: 1,
          presentation_class: "operator", inputs: [{ message_id: "initial-message", preview: "Initialize" }],
          execution: { kind: "terminal", outcome: "baseline_over_budget" },
          result: { kind: "none", reason: { kind: "reducer_only", reason: "failed" } },
          settled: true, attention: null, detail_coverage: { kind: "complete" }, brief_ids: [],
        }],
      } });
      await expect(page.getByText(/Run failed: the prompt baseline/)).toBeVisible();
      await expect(page.getByText("Created — initialization complete", { exact: true })).toBeVisible();
    }
    expect(posted).toBe(1);
    if (outcome === "ready") {
      await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
        data: { runtimeId: "replacement-runtime", visibilityScopeId: "replacement-scope" },
      });
      await request.post(`/__e2e__/disconnect-streams?session=${encodeURIComponent(session)}`);
      await expect(page.locator(".agent-creation-status")).not.toBeVisible();
    }
  });
}

test("200 bootstrapped navigates immediately with initialization complete", async ({ page, request }, info) => {
  const session = sessionFor(info, "creation");
  const id = "bootstrapped-feedback-agent";
  let posted = 0;
  await page.route(`**/api/control/agents/${id}/create`, async (route) => {
    posted += 1;
    await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
      data: { visibleAgentIds: ["bootstrap-agent", id] },
    });
    await route.fulfill({ status: 200, json: {
      identity: { agent_id: id, incarnation: 1, status: "active" },
      receipt: { receipt_id: "receipt-bootstrapped", agent_id: id, stage: "bootstrapped", bootstrap: bootstrap("succeeded") },
    } });
  });
  await page.goto("/");
  const dialog = await openCreate(page, id);
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/agents/${id}/conversation\\?incarnation=1`));
  await expect(page.getByText("Created — initialization complete", { exact: true })).toBeVisible();
  expect(posted).toBe(1);
});

test("409 is an operation error with an explicit verified existing-agent entry", async ({ page }) => {
  let posted = 0;
  await page.route("**/api/control/agents/bootstrap-agent/create", (route) => {
    posted += 1;
    return route.fulfill({ status: 409, json: { code: "already_exists", error: "SQL /home/private credential=secret" } });
  });
  await page.goto("/");
  const dialog = await openCreate(page, "bootstrap-agent");
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("ID or display name is already in use");
  await expect(dialog.getByRole("alert")).toContainText("HTTP 409");
  await expect(dialog).not.toContainText("/home/private");
  await expect(dialog.getByRole("textbox", { name: "Agent ID" })).toHaveValue("bootstrap-agent");
  await expect(dialog.getByRole("combobox")).toHaveValue("test-template");
  await expect(dialog.getByRole("button", { name: "Open existing agent" })).not.toBeVisible();
  await dialog.getByRole("button", { name: "Check existing agent" }).click();
  await dialog.getByRole("button", { name: "Open existing agent" }).click();
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation\?incarnation=1/);
  expect(posted).toBe(1);
});

test("scope replacement dismisses an old verified existing-identity entry", async ({ page, request }, info) => {
  await page.route("**/api/control/agents/bootstrap-agent/create", (route) => route.fulfill({
    status: 409, json: { code: "already_exists" },
  }));
  await page.goto("/");
  const dialog = await openCreate(page, "bootstrap-agent");
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await dialog.getByRole("button", { name: "Check existing agent" }).click();
  await expect(dialog.getByRole("button", { name: "Open existing agent" })).toBeVisible();
  const session = sessionFor(info, "creation");
  await request.post(`/__e2e__/configure?session=${encodeURIComponent(session)}`, {
    data: { visibilityScopeId: "replacement-scope" },
  });
  await request.post(`/__e2e__/disconnect-streams?session=${encodeURIComponent(session)}`);
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole("button", { name: "Open existing agent" })).not.toBeVisible();
  await openCreate(page, "bootstrap-agent");
  await expect(dialog.getByRole("button", { name: "Open existing agent" })).not.toBeVisible();
});

test("network uncertainty blocks duplicate POST and can open a verified committed identity", async ({ page }) => {
  let posted = 0;
  await page.route("**/api/control/agents/bootstrap-agent/create", (route) => {
    posted += 1;
    return route.abort("failed");
  });
  await page.goto("/");
  const dialog = await openCreate(page, "bootstrap-agent");
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("Creation result is not confirmed");
  await expect(dialog.getByRole("button", { name: "Create", exact: true })).toBeDisabled();
  await expect(dialog.getByRole("textbox", { name: "Agent ID" })).toBeDisabled();
  await dialog.locator("form").evaluate((form: HTMLFormElement) => form.requestSubmit());
  expect(posted).toBe(1);
  await dialog.getByRole("button", { name: "Check existing agent" }).click();
  await dialog.getByRole("button", { name: "Open existing agent" }).click();
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation\?incarnation=1/);
  expect(posted).toBe(1);
});

test("500 retains form inputs and provides safe diagnostics guidance instead of a fixed create failure", async ({ page }) => {
  await page.route("**/api/control/agents/reused-agent/create", (route) => route.fulfill({
    status: 500, json: { error: "private history cleanup proof is missing /home/private SQL", code: "private_internal_code" },
  }));
  await page.goto("/");
  const dialog = await openCreate(page, "reused-agent");
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("deletion status");
  await expect(dialog.getByRole("alert")).toContainText("HTTP 500");
  await expect(dialog).not.toContainText("private_internal_code");
  await expect(dialog).not.toContainText("/home/private");
  await expect(dialog.getByRole("textbox", { name: "Agent ID" })).toHaveValue("reused-agent");
  await expect(dialog.getByRole("combobox")).toHaveValue("test-template");
});

import { expect, test } from "@playwright/test";
import statusFixture from "../../../tests/fixtures/agent-status-display.json" with { type: "json" };
import { sessionFor } from "./test-session";

test.beforeEach(async ({ context }, info) => {
  await context.addCookies([{
    name: "holon_e2e_session", value: sessionFor(info), domain: "127.0.0.1", path: "/",
  }]);
});

const browserCases = new Set([
  "active-turn-with-queued-input",
  "queued-input-is-not-a-request-for-input",
  "operator-wait-overrides-stale-run",
  "blocked-is-not-waiting",
]);

for (const fixture of statusFixture.cases.filter(({ name }) => browserCases.has(name))) {
  test(`${fixture.name}: sidebar, selected header, and Dashboard agree`, async ({ page }) => {
    const summary = { ...statusFixture.defaults, ...fixture.input };
    await page.route(/\/api\/agents\/(list|snapshot|bootstrap-agent\/state)(\?.*)?$/, async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const posture = { posture: summary.posture, reason: "status fixture" };
      if (new URL(route.request().url()).pathname.endsWith("/state")) {
        data.agent.scheduling_posture = posture;
        data.agent.agent.status = "awake_running";
        data.session.current_run_id = "stale-run";
        data.session.pending_count = summary.pending;
      } else {
        const entries = Array.isArray(data) ? data : data.agents.map((entry: { agent: unknown }) => entry.agent);
        for (const entry of entries) {
          entry.scheduling_posture = posture;
          entry.status = "awake_running";
          entry.current_run_id = "stale-run";
          entry.pending = summary.pending;
        }
      }
      await route.fulfill({ response, json: data });
    });

    await page.goto("/agents/bootstrap-agent/conversation");
    const row = page.locator(".agent-row-status-dot");
    const header = page.locator(".header-status");
    for (const indicator of [row, header]) {
      await expect(indicator).toHaveAttribute("data-status-kind", fixture.expected.kind);
      await expect(indicator).toHaveAttribute("data-status-icon", fixture.expected.icon);
      await expect(indicator).toHaveAttribute("data-tone", fixture.expected.tone);
      await expect(indicator).toHaveAccessibleName(/.+/);
      await expect(indicator.locator("svg")).toHaveCount(1);
      await expect(indicator).toHaveAttribute("title", /.+/);
      await expect(indicator).toHaveAttribute("data-tooltip", /.+/);
    }
    await expect(header).toHaveAccessibleName((await row.getAttribute("aria-label"))!);

    if (fixture.expected.kind === "running") {
      const spinner = header.locator(".agent-status-spinner");
      await expect(spinner).not.toHaveCSS("animation-name", "none");
      await page.emulateMedia({ reducedMotion: "reduce" });
      await expect(spinner).toHaveCSS("animation-name", "none");
      await expect(row.locator(".agent-status-spinner")).toHaveCSS("animation-name", "none");
    } else {
      await expect(row.locator(".agent-status-spinner")).toHaveCount(0);
      await expect(header.locator(".agent-status-spinner")).toHaveCount(0);
    }

    await page.getByRole("button", { name: "Dashboard", exact: true }).first().click();
    const dashboardStatus = page.locator(".agent-card-status .agent-status-indicator");
    await expect(dashboardStatus).toHaveCount(1);
    await expect(dashboardStatus).toHaveAttribute("data-status-kind", fixture.expected.kind);
    await expect(dashboardStatus).toHaveAttribute("data-status-icon", fixture.expected.icon);
    await expect(dashboardStatus).toHaveAttribute("data-tone", fixture.expected.tone);
  });
}

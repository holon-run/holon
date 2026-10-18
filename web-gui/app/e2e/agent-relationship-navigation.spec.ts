import { expect, test, type Page } from "@playwright/test";
import { sessionFor } from "./test-session";

const agentId = "bootstrap-agent";
const parentId = "e2e-agent";

async function expectCenteredAccessState(page: Page) {
  const pane = page.locator(".conversation-access-state");
  await expect(pane).toBeVisible();
  const card = pane.locator(".conversation-empty");
  await expect(card).toBeVisible();
  const outer = (await pane.boundingBox())!;
  // Center the complete access-state group, including its retry action.
  const inner = await pane.evaluate((element) => {
    const boxes = Array.from(element.children, (child) => child.getBoundingClientRect());
    const x = Math.min(...boxes.map((box) => box.left));
    const y = Math.min(...boxes.map((box) => box.top));
    return { x, y, width: Math.max(...boxes.map((box) => box.right)) - x, height: Math.max(...boxes.map((box) => box.bottom)) - y };
  });
  expect(outer.height).toBeGreaterThan(300);
  expect(Math.abs(inner.x + inner.width / 2 - outer.x - outer.width / 2)).toBeLessThan(2);
  expect(Math.abs(inner.y + inner.height / 2 - outer.y - outer.height / 2)).toBeLessThan(2);
  await expect(card).toBeInViewport();
}

for (const supervised of [false, true]) {
  for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
    test(`${supervised ? "supervised child" : "independent agent"} direct refresh keeps loading and conversation aligned at ${viewport.width}px`, async ({ page, context, request }, info) => {
      const session = sessionFor(info, "agent-relations");
      await context.addCookies([{ name: "holon_e2e_session", value: session, domain: "127.0.0.1", path: "/" }]);
      await request.post(`/__e2e__/emit-agent?session=${encodeURIComponent(session)}`);
      await page.setViewportSize(viewport);
      let release!: () => void;
      let gate = new Promise<void>((resolve) => { release = resolve; });
      await page.route(`**/api/control/agents/${agentId}/detail`, async (route) => {
        await gate;
        const response = await route.fetch();
        const detail = await response.json();
        detail.identity.lineage_parent_agent_id = "default";
        // Legacy identity fields must not create a navigation relationship.
        detail.identity.parent_agent_id = "default";
        detail.canonical_relations = {
          agent_id: agentId, resolution: "resolved",
          supervision: supervised ? {
            supervision_id: "supervision-test", supervisor_agent_id: parentId,
            child_agent_id: agentId, state: "active", delegated_from_task_id: "task-test",
          } : null,
        };
        await route.fulfill({ json: detail });
      });
      try {
        await page.goto(`/agents/${agentId}/conversation`, { waitUntil: "domcontentloaded" });
        await expect(page.locator(".conversation-sync-status")).toContainText("cached history is read-only");
        await expect(page.locator(".composer textarea")).toBeEnabled();
        await page.locator(".composer textarea").fill("Draft before agent access resolves");
        await expect(page.locator(".composer .send-button")).toBeDisabled();
        release();
        await expect(page.locator(".composer .send-button")).toBeEnabled();

        gate = new Promise<void>((resolve) => { release = resolve; });
        await page.reload({ waitUntil: "domcontentloaded" });
        await expect(page.locator(".conversation-sync-status")).toContainText("cached history is read-only");
        await expect(page.locator(".composer textarea")).toBeEnabled();
        await expect(page.locator(".composer textarea")).toHaveValue("Draft before agent access resolves");
        await expect(page.locator(".composer .send-button")).toBeDisabled();
        await expect(page.locator(".message-list")).toBeVisible();
        await page.screenshot({ path: info.outputPath("refresh-read-only.png") });
        release();
        await expect(page.locator(".composer .send-button")).toBeEnabled();
        await expect(page.getByText(/Messages go directly to this subagent/)).toHaveCount(0);
        const back = page.getByRole("link", { name: "Back to parent agent", exact: true });
        await expect(back).toHaveCount(supervised ? 1 : 0);
        const list = (await page.locator(".message-list").boundingBox())!;
        const composer = (await page.locator(".composer").boundingBox())!;
        const header = (await page.locator(".conversation-header").boundingBox())!;
        expect(list.height).toBeGreaterThan(150);
        expect(list.y).toBeGreaterThanOrEqual(header.y + header.height - 1);
        expect(list.y + list.height).toBeLessThanOrEqual(composer.y + 1);
        await expect(page.locator(".composer textarea")).toBeInViewport();
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
        const requests = await request.get(`/__e2e__/requests?session=${encodeURIComponent(session)}`).then((response) => response.json());
        expect(requests.requests.some((path: string) => path.includes("/control/agents/default/"))).toBe(false);
        if (supervised) {
          await expect(back).toHaveAttribute("href", `/agents/${parentId}/conversation`);
          await back.click();
          await expect(page).toHaveURL(new RegExp(`/agents/${parentId}/conversation$`));
          await expect(page.locator(".composer textarea")).toBeEnabled();
        }
      } finally {
        release();
      }
    });
  }
}

test("missing agent state stays centered with a retry action", async ({ page, context }, info) => {
  await context.addCookies([{ name: "holon_e2e_session", value: sessionFor(info), domain: "127.0.0.1", path: "/" }]);
  await page.goto("/agents/removed-agent/conversation");
  await expect(page.locator(".conversation-access-state").getByText("Agent not found", { exact: true })).toBeVisible();
  await expect(page.locator(".side-panel").getByText("Agent not found", { exact: true })).toHaveCount(1);
  await expect(page.locator(".conversation-access-state")).toHaveCSS("display", "flex");
  await expectCenteredAccessState(page);
  await expect(page.locator(".conversation-access-state").getByRole("button", { name: "Refresh", exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "Back to parent agent", exact: true })).toHaveCount(0);
});

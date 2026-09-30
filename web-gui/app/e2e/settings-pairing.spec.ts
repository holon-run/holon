import { expect, test, type Page } from "@playwright/test";

async function setupPairing(page: Page, serving: boolean) {
  let issued = 0;
  await page.route("**/api/control/network/tailscale/serve", (route) => route.fulfill({
    json: {
      available: true, connected: true, status_known: true, serving,
      desired_enabled: serving, conflict: false, message: "",
      serve_url: "https://holon.example.ts.net",
    },
  }));
  await page.route("**/api/auth/pairing/issue", (route) => {
    issued++;
    return route.fulfill({
      json: { ticket: "one-time/+ticket", expires_at: new Date(Date.now() + 60_000).toISOString() },
    });
  });
  await page.goto("/settings");
  return () => issued;
}

test("localhost pairing uses active Tailscale Serve without changing the issue endpoint", async ({ page }) => {
  const issued = await setupPairing(page, true);
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  const link = page.getByRole("textbox", { name: "Pairing link", exact: true });
  await expect(link).toBeVisible();
  const url = new URL(await link.inputValue());
  expect(url.origin).toBe("https://holon.example.ts.net");
  expect(new URLSearchParams(url.hash.slice(1)).get("pair")).toBe("one-time/+ticket");
  expect(url.search).toBe("");
  expect(issued()).toBe(1);
  await expect(page.getByAltText("QR code for one-time pairing link")).toBeVisible();
});

test("requires a reachable origin when Serve is off, and supports LAN without leaking a ticket to localhost", async ({ page }) => {
  const issued = await setupPairing(page, false);
  const button = page.getByRole("button", { name: "Create one-time pairing link", exact: true });
  const address = page.getByRole("textbox", { name: "Device-accessible server address", exact: true });
  await button.click();
  await expect(page.getByRole("alert")).toContainText("Localhost cannot");
  expect(issued()).toBe(0);
  await address.fill("http://localhost:7878");
  await button.click();
  await expect(page.getByRole("alert")).toContainText("Localhost cannot");
  expect(issued()).toBe(0);
  await address.fill("http://192.168.1.10:7878");
  await button.click();
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true }))
    .toHaveValue(/^http:\/\/192\.168\.1\.10:7878\/login#pair=/);
  expect(issued()).toBe(1);
  await address.fill("http://192.168.1.11:7878");
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true })).toHaveCount(0);
});

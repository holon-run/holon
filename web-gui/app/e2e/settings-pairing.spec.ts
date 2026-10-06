import { expect, test, type Page } from "@playwright/test";

async function setupPairing(page: Page, serving: boolean, settingsUrl = "/settings") {
  let issued = 0;
  await page.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "local" } }));
  await page.route("**/api/control/network/tailscale/serve", (route) => route.fulfill({
    json: {
      available: true, connected: true, status_known: true, serving,
      desired_enabled: serving, conflict: false, message: "",
      hostname: "holon.example.ts.net",
      serve_url: "https://holon.example.ts.net",
    },
  }));
  await page.route("**/api/auth/pairing/issue", (route) => {
    issued++;
    return route.fulfill({
      json: { ticket: "one-time/+ticket", expires_at: new Date(Date.now() + 60_000).toISOString() },
    });
  });
  await page.goto(settingsUrl);
  return () => issued;
}

test("automatic Serve address is a full-width editable value on desktop and mobile", async ({ page }) => {
  await setupPairing(page, true);
  const address = page.getByRole("textbox", { name: "Device-accessible server address", exact: true });
  await expect(address).toHaveValue("https://holon.example.ts.net");
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const bounds = await address.boundingBox();
    expect(bounds!.width).toBeGreaterThan(width === 1280 ? 600 : 200);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
    expect(await address.evaluate((input) => parseFloat(getComputedStyle(input).borderTopWidth))).toBeGreaterThan(0);
  }
  await address.click();
  await address.press("ControlOrMeta+A");
  await address.pressSequentially("http://192.168.1.10:7878");
  await expect(address).toHaveValue("http://192.168.1.10:7878");
  await page.getByRole("button", { name: "Refresh status", exact: true }).click();
  await expect(page.getByRole("button", { name: "Disable", exact: true })).toBeVisible();
  await expect(address).toHaveValue("http://192.168.1.10:7878");
});

for (const host of ["192.168.1.10", "holon-http.test"]) {
  test(`defaults to the non-local browser origin ${host} without Serve`, async ({ page, baseURL }) => {
    const origin = new URL(baseURL!);
    origin.hostname = host;
    await page.route(`${origin.origin}/**`, (route) => {
      if (route.request().headers().accept === "text/event-stream") {
        return route.fulfill({ contentType: "text/event-stream", body: ": keepalive\n\n" });
      }
      const localUrl = new URL(route.request().url());
      localUrl.hostname = "127.0.0.1";
      return route.fetch({ url: localUrl.toString() }).then((response) => route.fulfill({ response }));
    });
    await setupPairing(page, false, `${origin.origin}/settings`);
    await expect(page.getByRole("textbox", { name: "Device-accessible server address", exact: true }))
      .toHaveValue(origin.origin);
    await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
    const link = page.getByRole("textbox", { name: "Pairing link", exact: true });
    await expect(link).toBeVisible();
    expect(new URL(await link.inputValue()).origin).toBe(origin.origin);
  });
}

test("clearing the default survives status refresh and never silently issues to Serve", async ({ page }) => {
  const issued = await setupPairing(page, true);
  const address = page.getByRole("textbox", { name: "Device-accessible server address", exact: true });
  await expect(address).toHaveValue("https://holon.example.ts.net");
  await address.fill("");
  const mutations: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/control/network/tailscale/serve") && request.method() !== "GET") {
      mutations.push(request.method());
    }
  });
  await page.getByRole("button", { name: "Refresh status", exact: true }).click();
  await expect(page.getByRole("button", { name: "Disable", exact: true })).toBeVisible();
  await expect(address).toHaveValue("");
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Localhost cannot");
  await expect(address).toHaveValue("");
  expect(issued()).toBe(0);
  expect(mutations).toEqual([]);
});

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

for (const serving of [true, false]) {
  test(`bootstrap refresh preserves pairing QR, link and address (Serve ${serving})`, async ({ page }) => {
    const issued = await setupPairing(page, serving);
    const address = page.getByRole("textbox", { name: "Device-accessible server address", exact: true });
    if (!serving) await address.fill("http://192.168.1.10:7878");
    else await expect(address).toHaveValue("https://holon.example.ts.net");
    const previousAddress = await address.inputValue();
    await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
    const link = page.getByRole("textbox", { name: "Pairing link", exact: true });
    await expect(link).toBeVisible();
    const previousLink = await link.inputValue();
    const qr = page.getByAltText("QR code for one-time pairing link");
    const previousQr = await qr.getAttribute("src");
    const refreshed = page.waitForResponse("**/api/agents/snapshot");
    const applied = page.waitForResponse("**/api/auth/session/me");
    await page.evaluate(() => window.dispatchEvent(new Event("online")));
    await refreshed;
    await applied;
    // User lookup starts after bootstrap resolves; let React commit its effects.
    await page.evaluate(() => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    }));
    await expect(address).toHaveValue(previousAddress);
    await expect(link).toHaveValue(previousLink);
    await expect(qr).toHaveAttribute("src", previousQr!);
    expect(issued()).toBe(1);
  });
}

test("Serve change hides the old ticket during the request and does not reissue", async ({ page }) => {
  const issued = await setupPairing(page, true);
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  const link = page.getByRole("textbox", { name: "Pairing link", exact: true });
  await expect(link).toBeVisible();
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/control/network/tailscale/serve/disable", async (route) => {
    await pending;
    await route.fulfill({ json: {
      available: true, connected: true, status_known: true, serving: false,
      desired_enabled: false, conflict: false, hostname: "holon.example.ts.net", message: "",
    } });
  });
  await page.getByRole("button", { name: "Disable", exact: true }).click();
  await expect(link).toHaveCount(0);
  await expect(page.getByAltText("QR code for one-time pairing link")).toHaveCount(0);
  release();
  await expect(page.getByRole("button", { name: "Enable", exact: true })).toBeVisible();
  expect(issued()).toBe(1);
});

test("OIDC explains normal sign-in and does not issue a pairing ticket", async ({ page }) => {
  let issued = 0;
  await page.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "oidc" } }));
  await page.route("**/api/auth/pairing/issue", (route) => { issued++; return route.fulfill({ json: {} }); });
  await page.goto("/settings");
  await expect(page.getByText("This runtime uses OIDC.", { exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Create one-time pairing link", exact: true })).toBeDisabled();
  expect(issued).toBe(0);
});

for (const action of ["enable", "refresh"] as const) {
  test(`Serve ${action} immediately hides a custom-origin ticket without reissuing`, async ({ page }) => {
    const issued = await setupPairing(page, false);
    await page.getByRole("textbox", { name: "Device-accessible server address", exact: true })
      .fill("http://192.168.1.10:7878");
    await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
    const link = page.getByRole("textbox", { name: "Pairing link", exact: true });
    await expect(link).toBeVisible();
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    const endpoint = action === "enable" ? "**/api/control/network/tailscale/serve/enable"
      : "**/api/control/network/tailscale/serve";
    await page.route(endpoint, async (route) => {
      await pending;
      await route.fulfill({ json: {
        available: true, connected: true, status_known: true, serving: true,
        desired_enabled: true, conflict: false, hostname: "holon.example.ts.net",
        serve_url: "https://holon.example.ts.net", message: "",
      } });
    });
    if (action === "enable") page.once("dialog", (dialog) => void dialog.accept());
    await page.getByRole("button", { name: action === "enable" ? "Enable" : "Refresh status", exact: true }).click();
    await expect(link).toHaveCount(0);
    await expect(page.getByAltText("QR code for one-time pairing link")).toHaveCount(0);
    release();
    await expect(page.getByRole("button", { name: "Disable", exact: true })).toBeVisible();
    expect(issued()).toBe(1);
  });
}

test("explicit custom origin overrides valid Serve", async ({ page }) => {
  await setupPairing(page, true);
  await page.getByRole("textbox", { name: "Device-accessible server address", exact: true }).fill("https://proxy.example");
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true }))
    .toHaveValue(/^https:\/\/proxy\.example\/login#pair=/);
});

test("initial authentication failure can be retried without remounting or issuing automatically", async ({ page }) => {
  let failing = true;
  await page.route("**/api/auth/method", (route) => route.fulfill(
    failing ? { status: 503, json: {} } : { json: { mode: "local" } },
  ));
  let issued = 0;
  await page.route("**/api/control/network/tailscale/serve", (route) => route.fulfill({
    json: { available: true, connected: true, status_known: true, serving: true,
      desired_enabled: true, conflict: false, message: "", hostname: "holon.example.ts.net",
      serve_url: "https://holon.example.ts.net" },
  }));
  await page.route("**/api/auth/pairing/issue", (route) => {
    issued++;
    return route.fulfill({ json: { ticket: "retried", expires_at: new Date(Date.now() + 60_000).toISOString() } });
  });
  await page.goto("/settings");
  const button = page.getByRole("button", { name: "Create one-time pairing link", exact: true });
  const alert = page.getByRole("alert").filter({ hasText: "Cannot reach the sign-in service" });
  await expect(alert).toBeVisible();
  await expect(button).toBeDisabled();
  expect(issued).toBe(0);
  failing = false;
  await alert.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(alert).toHaveCount(0);
  await expect(button).toBeEnabled();
  expect(issued).toBe(0);
  await button.click();
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true })).toBeVisible();
  expect(issued).toBe(1);
});

for (const auth of ["failure", "oidc"]) {
  test(`authentication recheck ${auth} prevents ticket issuance`, async ({ page }) => {
    const issued = await setupPairing(page, true);
    const button = page.getByRole("button", { name: "Create one-time pairing link", exact: true });
    await expect(button).toBeEnabled();
    await page.route("**/api/auth/method", (route) => route.fulfill(
      auth === "failure" ? { status: 503, json: {} } : { json: { mode: "oidc" } },
    ));
    await button.click();
    if (auth === "failure") await expect(page.getByRole("alert")).toBeVisible();
    else await expect(button).toBeDisabled();
    expect(issued()).toBe(0);
    await expect(page.getByRole("textbox", { name: "Pairing link", exact: true })).toHaveCount(0);
  });
}

test("Serve disable during auth recheck prevents a later issue request", async ({ page }) => {
  const issued = await setupPairing(page, true);
  await expect(page.getByRole("button", { name: "Disable", exact: true })).toBeVisible();
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/auth/method", async (route) => {
    await pending;
    await route.fulfill({ json: { mode: "local" } });
  });
  await page.route("**/api/control/network/tailscale/serve/disable", (route) => route.fulfill({
    json: { available: true, connected: true, status_known: true, serving: false,
      desired_enabled: false, conflict: false, message: "" },
  }));
  const checking = page.waitForRequest("**/api/auth/method");
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  await checking;
  await page.getByRole("button", { name: "Disable", exact: true }).click();
  await expect(page.getByRole("button", { name: "Enable", exact: true })).toBeVisible();
  release();
  await expect(page.getByRole("button", { name: "Create one-time pairing link", exact: true })).toBeEnabled();
  expect(issued()).toBe(0);
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true })).toHaveCount(0);
});

test("late issue response cannot restore a ticket after Serve changes", async ({ page }) => {
  await setupPairing(page, true);
  let release!: () => void;
  const pending = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/auth/pairing/issue", async (route) => {
    await pending;
    await route.fulfill({ json: { ticket: "stale", expires_at: new Date(Date.now() + 60_000).toISOString() } });
  });
  await page.route("**/api/control/network/tailscale/serve/disable", (route) => route.fulfill({ json: {
    available: true, connected: true, status_known: true, serving: false,
    desired_enabled: false, conflict: false, message: "",
  } }));
  const issuing = page.waitForRequest("**/api/auth/pairing/issue");
  await page.getByRole("button", { name: "Create one-time pairing link", exact: true }).click();
  await issuing;
  await page.getByRole("button", { name: "Disable", exact: true }).click();
  const response = page.waitForResponse("**/api/auth/pairing/issue");
  release();
  await response;
  await expect(page.getByRole("button", { name: "Create one-time pairing link", exact: true })).toBeEnabled();
  await expect(page.getByRole("textbox", { name: "Pairing link", exact: true })).toHaveCount(0);
});

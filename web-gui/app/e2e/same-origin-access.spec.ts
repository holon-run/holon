import { expect, test } from "@playwright/test";

for (const scenario of [
  { name: "anonymous local access", authMethod: "local_control", tokenRequired: false, signOut: false },
  { name: "local token session", authMethod: "local_control", tokenRequired: true, signOut: true },
  { name: "organization session without a control token", authMethod: "oidc", tokenRequired: false, signOut: true },
]) {
  test(`connection menu handles ${scenario.name}`, async ({ page }) => {
    await page.route("**/api/handshake", async (route) => {
      const response = await route.fetch();
      const handshake = await response.json();
      handshake.auth = { mode: scenario.tokenRequired ? "bearer" : "local", required: scenario.tokenRequired };
      await route.fulfill({ json: handshake });
    });
    await page.route("**/api/auth/session/me", (route) => route.fulfill({
      json: { user_id: "control", auth_method: scenario.authMethod },
    }));
    await page.goto("/");
    await page.locator(".connection-status").click();
    const panel = page.getByRole("dialog", { name: "Connection", exact: true });
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Sign out" })).toHaveCount(scenario.signOut ? 1 : 0);
    await expect(panel.getByRole("button", { name: "Retry connection" })).toBeVisible();
  });
}

test("startup ignores saved remotes and exposes only the current server", async ({ page, baseURL }) => {
  await page.addInitScript(() => {
    const remote = { mode: "remote", baseUrl: "https://retired.example", token: "old-remote-secret" };
    sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify(remote));
    localStorage.setItem("holon.webGui.runtimeConnection.v1", JSON.stringify(remote));
    localStorage.setItem("holon.webGui.runtimeConnectionProfiles.v1", JSON.stringify({ "https://retired.example": remote }));
  });
  const requests: string[] = [];
  page.on("request", (request) => {
    requests.push(request.url());
    expect(request.headers().authorization ?? "").not.toContain("old-remote-secret");
  });
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/api/handshake", async (route) => { await gate; await route.continue(); });
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Opening your workspace…" })).toBeVisible();
  await expect(page.locator(".boot-page input, .boot-page button")).toHaveCount(0);
  release();
  await expect(page.locator(".sidebar")).toBeVisible();
  await page.locator(".connection-status").click();
  const panel = page.getByRole("dialog", { name: "Connection", exact: true });
  await expect(panel.getByText(baseURL!, { exact: true })).toBeVisible();
  await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
  await expect(panel.locator("input")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  await expect(page.locator(".connection-status")).toBeFocused();
  await page.reload();
  await expect(page.locator(".sidebar")).toBeVisible();
  expect(requests.every((url) => url.startsWith(baseURL!))).toBe(true);
});

test("token sign-in uses this server, reports errors, and replaces stale bearer credentials", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  let authenticated = false;
  await page.route("**/api/handshake", (route) => authenticated
    ? route.continue()
    : route.fulfill({ status: 401, json: { code: "auth_required", error: "Unauthorized" } }));
  await page.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "static" } }));
  await page.goto("/agents/bootstrap-agent/conversation");
  await expect(page.getByRole("heading", { name: "Sign in to Holon" })).toBeVisible();
  const token = page.getByLabel("Access token", { exact: true });
  await token.fill("wrong-token");
  await page.route("**/api/auth/session/exchange", async (route) => {
    expect(route.request().postDataJSON()).toEqual({ credential: authenticated ? "valid-token" : "wrong-token" });
    await route.fulfill({ status: authenticated ? 200 : 401, json: {} });
  });
  await page.getByRole("button", { name: "Sign in with token" }).click();
  await expect(page.getByRole("alert")).toContainText("Check your access token");
  await page.evaluate(() => sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token: "stale-token" })));
  authenticated = true;
  await token.fill("valid-token");
  await page.getByRole("button", { name: "Sign in with token" }).click();
  await expect(page.locator(".sidebar")).toBeVisible();
  await expect(page).toHaveURL(/\/agents\/bootstrap-agent\/conversation$/);
  expect(await page.evaluate(() => sessionStorage.getItem("holon.webGui.activeRuntimeConnection.v1"))).not.toContain("stale-token");
});

test("unavailable sign-in discovery stays on the page and can be retried", async ({ page }) => {
  await page.route("**/api/handshake", (route) => route.fulfill({ status: 401, json: { code: "auth_required" } }));
  let available = false;
  await page.route("**/api/auth/method", (route) => route.fulfill({ status: available ? 200 : 503, json: { mode: "static" } }));
  await page.goto("/login");
  await expect(page.getByRole("status")).toContainText("Cannot reach the sign-in service");
  await expect(page).toHaveURL(/\/login$/);
  available = true;
  await page.getByRole("button", { name: "Retry connection" }).click();
  await expect(page.getByLabel("Access token", { exact: true })).toBeVisible();
});

test("connection recovery and sign-out remain available in the compact sidebar", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() => {
    if (location.pathname === "/") sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token: "legacy-local-token" }));
  });
  let disconnected = true;
  await page.route("**/api/handshake", (route) => disconnected
    ? route.fulfill({ status: 503, json: {} }) : route.continue());
  await page.goto("/");
  await page.locator(".connection-status").click();
  const panel = page.getByRole("dialog", { name: "Connection", exact: true });
  await expect(panel.getByText("Disconnected", { exact: true })).toBeVisible();
  disconnected = false;
  await panel.getByRole("button", { name: "Retry connection" }).click();
  // Initial bootstrap may briefly replace the workspace while retrying.
  await expect(page.locator(".sidebar")).toBeVisible();
  if (!await panel.isVisible()) await page.locator(".connection-status").click();
  await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
  const bounds = await panel.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
  await page.route("**/api/auth/session/logout", (route) => route.fulfill({ json: {} }));
  await page.route("**/api/handshake", (route) => route.fulfill({ status: 401, json: { code: "auth_required" } }));
  await page.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "static" } }));
  await panel.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Sign in to Holon" })).toBeVisible();
  await expect(page).toHaveURL(/\/login\?return_to=/);
  expect(await page.evaluate(() => sessionStorage.getItem("holon.webGui.activeRuntimeConnection.v1"))).not.toContain("legacy-local-token");
});

test("organization sign-in keeps a same-origin callback and clears a stale token", async ({ page }) => {
  await page.addInitScript(() => {
    if (location.pathname === "/login") sessionStorage.setItem("holon.webGui.activeRuntimeConnection.v1", JSON.stringify({ mode: "local", token: "stale-token" }));
  });
  await page.route("**/api/handshake", (route) => route.fulfill({ status: 401, json: { code: "auth_required" } }));
  await page.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "oidc" } }));
  await page.route("**/api/auth/oidc/start?**", (route) => route.fulfill({ contentType: "text/html", body: "<h1>Organization login</h1>" }));
  await page.goto("/login?return_to=%2Fagents%2Fbootstrap-agent%2Fconversation");
  await expect(page).toHaveURL(/\/api\/auth\/oidc\/start\?return_to=%2Fagents%2Fbootstrap-agent%2Fconversation$/);
  expect(await page.evaluate(() => sessionStorage.getItem("holon.webGui.activeRuntimeConnection.v1"))).not.toContain("stale-token");
});

test("sign-out clears conversation caches and rechecks sibling tabs", async ({ page, context }) => {
  let authenticated = true;
  await context.route("**/api/auth/session/me", (route) => route.fulfill({ json: { user_id: "control", auth_method: "oidc" } }));
  await context.route("**/api/handshake", (route) => authenticated ? route.continue()
    : route.fulfill({ status: 401, json: { code: "auth_required" } }));
  await context.route("**/api/auth/method", (route) => route.fulfill({ json: { mode: "static" } }));
  await context.route("**/api/auth/session/logout", (route) => {
    authenticated = false;
    return route.fulfill({ json: {} });
  });
  await page.goto("/");
  await expect(page.locator(".sidebar")).toBeVisible();
  const sibling = await context.newPage();
  await sibling.goto("/");
  await expect(sibling.locator(".sidebar")).toBeVisible();
  await page.evaluate(async () => {
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.open("holon-webgui-cache");
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction(["briefs", "snapshots"], "readwrite");
        tx.objectStore("briefs").put({ remoteKey: "local#user", agentId: "agent", briefId: "brief", text: "private cached brief" });
        tx.objectStore("snapshots").put({ remoteKey: "local#user", agentId: "agent", text: "private cached snapshot" });
        tx.oncomplete = () => { db.close(); resolve(); };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
      request.onerror = () => reject(request.error);
    });
  });
  await page.locator(".connection-status").click();
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Sign in to Holon" })).toBeVisible();
  await expect(sibling.getByRole("heading", { name: "Sign in to Holon" })).toBeVisible();
  const count = await page.evaluate(async () => await new Promise<number>((resolve, reject) => {
    const request = indexedDB.open("holon-webgui-cache");
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction(["briefs", "snapshots"], "readonly");
      const briefs = tx.objectStore("briefs").count();
      const snapshots = tx.objectStore("snapshots").count();
      tx.oncomplete = () => { db.close(); resolve(briefs.result + snapshots.result); };
      tx.onabort = () => { db.close(); reject(tx.error); };
    };
    request.onerror = () => reject(request.error);
  }));
  expect(count).toBe(0);
  await sibling.close();
});

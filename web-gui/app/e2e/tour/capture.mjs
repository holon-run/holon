import { spawn } from "node:child_process";
import { once } from "node:events";
import { writeFile, mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { tourAgents } from "./scenario.mjs";

const cwd = fileURLToPath(new URL("../../", import.meta.url));
const output = new URL("../../../../docs/website/assets/", import.meta.url);
const port = Number(process.env.HOLON_TOUR_PORT ?? 43127);
const origin = `http://127.0.0.1:${port}`;
let server;
let browser;
async function stop() {
  if (!server || server.exitCode !== null) return;
  const exited = once(server, "exit");
  server.kill("SIGTERM");
  await exited;
}
async function start() {
  server = spawn(process.execPath, ["e2e/fixture-server.mjs", "--port", String(port), "--tour"], { cwd, stdio: "inherit" });
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error("fixture failed to start");
    try {
      if ((await fetch(`${origin}/__e2e__/health`)).ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("fixture startup timed out");
}
async function snapshot() {
  return Promise.all(tourAgents.map(async ({ id }) =>
    (await fetch(`${origin}/api/agents/${id}/state`)).json()));
}
async function capture(page, name) {
  const png = (await page.screenshot({ animations: "disabled" })).toString("base64");
  const webp = await page.evaluate(async (data) => {
    const image = new Image();
    image.src = `data:image/png;base64,${data}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    canvas.getContext("2d").drawImage(image, 0, 0);
    return canvas.toDataURL("image/webp", 0.94).split(",")[1];
  }, png);
  await writeFile(new URL(name, output), Buffer.from(webp, "base64"));
}
try {
  await start();
  const before = await snapshot();
  await stop();
  await start();
  assert.deepEqual(await snapshot(), before, "restart must reconstruct identical demo states");
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1, locale: "en-US", timezoneId: "UTC", colorScheme: "light" });
  const unexpected = [];
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) {
      unexpected.push(url.origin);
      await route.abort();
    } else await route.continue();
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => unexpected.push(error.message));
  await page.goto(`${origin}/`);
  await page.waitForFunction(() => window.__HOLON_E2E__?.snapshot().agentIds.length === 6);
  await page.goto(`${origin}/agents/reviewer`);
  await page.getByText("Review event-ingress trust boundaries", { exact: false }).first().waitFor();
  await page.getByRole("heading", { name: "Review scope ready for confirmation" }).waitFor();
  await page.evaluate(() => document.fonts.ready);
  // Allow the local fixture's optional detail requests to settle.
  await page.waitForTimeout(800);
  await page.locator("summary").filter({ hasText: /Skills \(2\)/i }).click();
  await page.getByText("github-review", { exact: true }).first().waitFor();
  await mkdir(output, { recursive: true });
  await capture(page, "holon-tour-agents.webp");
  await page.getByRole("button", { name: /Review event-ingress trust boundaries.*Current/ }).click();
  await page.getByText("Confirm review scope with the operator", { exact: false }).first().waitFor();
  await capture(page, "holon-tour-review-work.webp");
  console.log((await page.locator("body").innerText()).slice(0, 10000));
  assert.deepEqual(unexpected, [], "capture must not contact external origins or throw page errors");
  console.log("Verified deterministic restart and isolated GUI capture: 1600 × 1000.");
} finally {
  await browser?.close();
  await stop();
}

#!/usr/bin/env node
// Capture the real Holon Web GUI for the homepage coordinator showcase (zh-CN & en).
// Serves web-gui/app/dist against web-gui/app/e2e/fixture-server.mjs (HOLON_E2E_PRODUCTION=1),
// takes unedited 3000x1880 (2x) browser captures, and exports 1600x1003 lossless WebP assets.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../../..");
const guiRoot = join(repoRoot, "web-gui/app");
const assetsOut = resolve(here, "../../assets");

function loadPlaywright() {
  const candidates = [
    join(guiRoot, "package.json"),
    join(homedir(), ".npm/_npx/9833c18b2d85bc59/node_modules/playwright/package.json"),
  ];
  for (const pkg of candidates) {
    try {
      const req = createRequire(pkg);
      return req("playwright");
    } catch {}
  }
  throw new Error("Playwright not found. Install playwright or @playwright/test.");
}

const { chromium } = loadPlaywright();

async function captureLocale(lang, port) {
  const scenarioUrl = new URL(`./home-scenario-${lang}.mjs`, import.meta.url);
  const { focusAgentId, labels, objective, timestamp } = await import(scenarioUrl.href);
  const origin = `http://127.0.0.1:${port}`;
  const staging = await mkdtemp(join(tmpdir(), `holon-home-gui-${lang}-`));

  const originalFixture = await readFile(join(guiRoot, "e2e/fixture-server.mjs"), "utf8");
  const patchedFixture = originalFixture
    .replace('import { createServer as createViteServer } from "vite";', "const createViteServer = null;")
    .replace("./tour/scenario.mjs", scenarioUrl.href)
    .replace("const productionRoot = new URL(\"../dist/\", import.meta.url);",
      `const productionRoot = new URL(${JSON.stringify(pathToFileURL(join(guiRoot, "dist/")).href)});`);

  const serverPath = join(staging, "fixture-server.mjs");
  await writeFile(serverPath, patchedFixture, "utf8");

  let server;
  let browser;
  const failures = [];
  try {
    server = spawn(process.execPath, [serverPath, "--port", String(port), "--tour"], {
      cwd: guiRoot,
      env: { ...process.env, HOLON_E2E_PRODUCTION: "1" },
      stdio: "inherit",
    });

    let ready = false;
    for (let i = 0; i < 100; i++) {
      if (server.exitCode !== null) throw new Error("Fixture server exited early");
      try {
        if ((await fetch(`${origin}/__e2e__/health`)).ok) {
          ready = true;
          break;
        }
      } catch {}
      await new Promise((r) => setTimeout(r, 100));
    }
    assert.ok(ready, "Fixture server startup timed out");

    try {
      browser = await chromium.launch({ headless: true });
    } catch {
      browser = await chromium.launch({ headless: true, channel: "chrome" });
    }
    const context = await browser.newContext({
      viewport: { width: 1500, height: 940 },
      deviceScaleFactor: 2,
      locale: lang,
      timezoneId: "Asia/Shanghai",
      colorScheme: "light",
    });
    await context.addInitScript(
      (language) => localStorage.setItem("holon.webGui.languageMode.v1", language),
      lang,
    );
    await context.route("**/*", async (route) => {
      if (new URL(route.request().url()).origin !== origin) {
        failures.push(`external request: ${route.request().url()}`);
        await route.abort();
      } else {
        await route.continue();
      }
    });

    const page = await context.newPage();
    page.on("pageerror", (err) => failures.push(err.message));
    await page.clock.install({ time: new Date(timestamp) });
    await page.goto(`${origin}/agents/${focusAgentId}/conversation`);
    await page.getByRole("heading", { name: labels.heading }).waitFor();
    await page.locator(".current-work-title").filter({ hasText: objective }).click();
    await page.getByText(labels.ciTodo, { exact: true }).waitFor();
    await page.locator(".conversation-detail-toggle").first().click();
    await page.getByText(labels.source, { exact: false }).first().waitFor();
    await page.evaluate(() => document.fonts.ready);
    await page.waitForTimeout(450);

    assert.equal(await page.locator(".current-work-bar").getAttribute("data-state"), "waitingExternal");
    for (const text of [labels.ciTodo, labels.finalTodo]) {
      const bounds = await page.getByText(text, { exact: true }).boundingBox();
      assert.ok(bounds && bounds.y >= 0 && bounds.y + bounds.height <= 940, `Clipped content (${lang}): ${text}`);
    }
    assert.deepEqual(failures, []);

    const rawPng = join(staging, `raw-${lang}.png`);
    const resizedPng = join(staging, `resized-${lang}.png`);
    const outWebp = join(assetsOut, `home-workbench-${lang}.webp`);

    await page.screenshot({ path: rawPng, animations: "disabled" });
    await mkdir(assetsOut, { recursive: true });
    // Match docs/website/assets/README.md: downscale 3000x1880 -> 1600x1003 and encode lossless WebP.
    execFileSync("magick", [rawPng, "-resize", "1600x1003!", resizedPng], { stdio: "inherit" });
    execFileSync("cwebp", ["-quiet", "-lossless", resizedPng, "-o", outWebp], { stdio: "inherit" });
    console.log(`Exported ${outWebp} (1600x1003 lossless WebP)`);
  } finally {
    await browser?.close();
    if (server && server.exitCode === null) {
      const stopped = once(server, "exit");
      server.kill("SIGTERM");
      await stopped;
    }
    await rm(staging, { recursive: true, force: true });
  }
}

await captureLocale("zh-CN", 43141);
await captureLocale("en", 43142);

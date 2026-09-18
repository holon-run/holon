// Capture the CURRENT real GUI using a versioned synthetic scenario.
// No changes to GUI components, no daemon, credentials or external services.
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile, mkdir, mkdtemp, symlink, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import assert from 'node:assert/strict';


const args = process.argv.slice(2);
const option = name => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
if (!option('--gui-root') || !option('--output-dir')) throw new Error('Usage: node capture.mjs --gui-root <repo>/web-gui/app --output-dir <review-output>');
const lang = option('--lang') ?? 'zh-CN';
assert(['zh-CN', 'en'].includes(lang), 'unsupported language');
const scenarioUrl = new URL(lang === 'en' ? './scenario-en.mjs' : './scenario.mjs', import.meta.url);
const { objective, timestamp } = await import(scenarioUrl.href);
const labels = lang === 'en' ? { heading: 'Changes reviewed; waiting for CI', ci: 'Check CI for the current commit', source: 'GitHub webhook · New commit on PR #42', report: 'Summarize conclusions and evidence' } : { heading: '已复核新提交，等待 CI', ci: '核对当前提交的 CI 结果', source: 'GitHub webhook · PR #42 有新提交', report: '汇总审阅结论与验证依据' };
const gui = resolve(option('--gui-root'));
const output = resolve(option('--output-dir'));
const require = createRequire(join(gui, 'package.json'));
const { chromium } = require('@playwright/test');
const port = Number(option('--port') ?? 43137);
const origin = `http://127.0.0.1:${port}`;
const staging = await mkdtemp(join(tmpdir(), 'holon-paper-gui-'));
let server, browser;
const failures = [];
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
try {
  // Reuse the current fixture transport and real Vite frontend. Only the data
  // module changes; temporary files and dependency link stay outside the repo.
  const originalFixture = await readFile(join(gui, 'e2e/fixture-server.mjs'), 'utf8');
  const importPath = './tour/scenario.mjs';
  assert(originalFixture.includes(importPath), 'fixture scenario import changed');
  const fixture = originalFixture.replace(importPath, scenarioUrl.href)
    .replace('server: { hmr: false,', 'server: { host: "127.0.0.1", hmr: false,');
  await writeFile(join(staging, 'fixture-server.mjs'), fixture);
  await symlink(join(gui, 'node_modules'), join(staging, 'node_modules'));
  server = spawn(process.execPath, [join(staging, 'fixture-server.mjs'), '--port', String(port), '--tour'], { cwd: gui, stdio: 'inherit' });
  let ready = false;
  for (let i = 0; i < 150; i++) {
    if (server.exitCode !== null) throw new Error('Fixture exited during startup');
    try { if ((await fetch(`${origin}/__e2e__/health`)).ok) { ready = true; break; } } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  assert(ready, 'fixture startup timed out');
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1500, height: 940 }, deviceScaleFactor: 2, locale: lang, timezoneId: 'Asia/Shanghai', colorScheme: 'light' });
  await context.addInitScript(language => localStorage.setItem('holon.webGui.languageMode.v1', language), lang);
  await context.route('**/*', async route => {
    if (new URL(route.request().url()).origin !== origin) { failures.push(`external request: ${route.request().url()}`); await route.abort(); }
    else await route.continue();
  });
  const page = await context.newPage();
  page.on('pageerror', error => failures.push(error.message));
  await page.clock.install({ time: new Date(timestamp) });
  await page.goto(`${origin}/agents/reviewer/conversation`);
  await page.getByRole('heading', { name: labels.heading }).waitFor();
  await page.locator('.current-work-title').filter({ hasText: objective }).click();
  await page.getByText(labels.ci, { exact: true }).waitFor();
  await page.locator('.conversation-source > summary').click();
  await page.getByText(labels.source, { exact: true }).waitFor();
  if (await page.locator('.conversation-jump').isVisible()) await page.locator('.conversation-jump').click();
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(500);
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'page-text.txt'), await page.locator('body').innerText());
  assert.equal(await page.locator('.current-work-bar').getAttribute('data-state'), 'waitingExternal');
  for (const text of [labels.source, labels.report]) {
    const bounds = await page.getByText(text, { exact: true }).boundingBox();
    assert(bounds && bounds.y >= 0 && bounds.y + bounds.height <= 940, `clipped screenshot content: ${text}`);
  }
  assert.deepEqual(failures, []);
  const png = await page.screenshot({ animations: 'disabled' });
  await writeFile(join(output, `web-gui-review-${lang}.png`), png);
  const metadata = {
    gui_commit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: gui, encoding: 'utf8' }).trim(),
    gui_dirty: execFileSync('git', ['status', '--porcelain', '--', 'web-gui'], { cwd: resolve(gui, '../..'), encoding: 'utf8' }).trim(),
    locale: lang, timezone: 'Asia/Shanghai', fixed_time: timestamp,
    viewport: { width: 1500, height: 940 }, device_scale_factor: 2,
    synthetic_data: true, live_runtime: false,
    scenario_sha256: sha(await readFile(scenarioUrl)),
    capture_script_sha256: sha(await readFile(fileURLToPath(import.meta.url))),
    fixture_transport_sha256: sha(originalFixture), screenshot_sha256: sha(png),
    checks: [`${lang} heading visible`, 'webhook source expanded and within viewport', 'full work item checklist within viewport', 'waitingExternal state', 'no external requests or page errors'],
  };
  await writeFile(join(output, 'capture-info.json'), JSON.stringify(metadata, null, 2) + '\n');
  console.log(`Captured ${join(output, `web-gui-review-${lang}.png`)}`);
} finally {
  await browser?.close();
  if (server && server.exitCode === null) { const stopped = once(server, 'exit'); server.kill('SIGTERM'); await stopped; }
  await rm(staging, { recursive: true, force: true });
}

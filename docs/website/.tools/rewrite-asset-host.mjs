// Point site-internal asset references at the dedicated asset host.
//
// Pages are served by the `holon.run` Worker while binaries are served by the
// `assets.holon.run` Worker (see ../asset-worker.mjs). mdorigin has no setting
// for an asset base URL, so the generated content tree is rewritten right
// before the Cloudflare bundle is built: `/assets/logo.png` becomes
// `https://assets.holon.run/assets/logo.png`.
//
// Only a bare `/assets/` prefix is rewritten. Repository paths mentioned in
// prose (`docs/website/assets/...`) and already-absolute URLs are left alone,
// which also makes the rewrite idempotent.
//
// Usage:
//   node rewrite-asset-host.mjs            # rewrite dist/content in place
//   node rewrite-asset-host.mjs --check    # fail if a built bundle still has
//                                          # bare `/assets/` references
//
// `ASSET_HOST=none` (or an empty value) disables the rewrite, which is the
// rollback switch: rebuild and redeploy the page Worker and pages go back to
// same-origin assets.
import { readFile, readdir, writeFile } from "node:fs/promises";

const DEFAULT_ASSET_HOST = "https://assets.holon.run";
const rawAssetHost = process.env.ASSET_HOST ?? DEFAULT_ASSET_HOST;
const assetHost = rawAssetHost === "none" ? "" : rawAssetHost.replace(/\/+$/, "");

const contentRoot = new URL("./dist/content/", import.meta.url);
const builtRoot = new URL("./dist/cloudflare/", import.meta.url);
const TEXT_EXTENSIONS = new Set([".md", ".html", ".mjs", ".js", ".css", ".json", ".xml", ".svg"]);
// A bare `/assets/` is a site-internal path. Skipping any match whose previous
// character is part of a path or host keeps `docs/website/assets/...` prose and
// already-rewritten URLs untouched.
const BARE_ASSET_REFERENCE = /(?<![A-Za-z0-9._/-])\/assets\//g;

async function listTextFiles(directory) {
  const files = [];
  const entries = await readdir(directory, { withFileTypes: true });
  for (const entry of entries) {
    const url = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) {
      files.push(...(await listTextFiles(url)));
      continue;
    }
    const dot = entry.name.lastIndexOf(".");
    if (dot >= 0 && TEXT_EXTENSIONS.has(entry.name.slice(dot))) files.push(url);
  }
  return files;
}

async function rewriteContent() {
  if (!assetHost) {
    console.log("asset host disabled (ASSET_HOST=none); leaving /assets/ paths relative");
    return;
  }
  let changedFiles = 0;
  let replacements = 0;
  for (const url of await listTextFiles(contentRoot)) {
    const original = await readFile(url, "utf8");
    const updated = original.replace(BARE_ASSET_REFERENCE, `${assetHost}/assets/`);
    if (updated === original) continue;
    replacements += (original.match(BARE_ASSET_REFERENCE) ?? []).length;
    await writeFile(url, updated, "utf8");
    changedFiles += 1;
  }
  console.log(`rewrote ${replacements} asset references in ${changedFiles} content files to ${assetHost}`);
}

async function verifyBuild() {
  if (!assetHost) {
    console.log("asset host disabled; skipping built-bundle check");
    return;
  }
  const workerUrl = new URL("worker.mjs", builtRoot);
  const source = await readFile(workerUrl, "utf8");
  const leftovers = source.match(BARE_ASSET_REFERENCE) ?? [];
  if (leftovers.length > 0) {
    console.error(
      `built bundle still has ${leftovers.length} bare /assets/ references; ` +
        "content must be rewritten before mdorigin build cloudflare",
    );
    process.exitCode = 1;
    return;
  }
  console.log("built bundle has no same-origin asset references");
}

if (process.argv.includes("--check")) {
  await verifyBuild();
} else {
  await rewriteContent();
}

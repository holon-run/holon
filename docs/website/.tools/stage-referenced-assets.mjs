// Copy every rewritten media reference into the asset host's static directory.
//
// `rewrite-asset-host.mjs` sends `/assets/...` references to
// `https://assets.holon.run/assets/...`, and mdorigin stages that origin's
// files in `dist/cloudflare/assets`. mdorigin only stages non-text files
// though: `image/svg+xml` counts as text, so diagrams stay inside the page
// Worker bundle and never reach the asset directory. Their rewritten URLs then
// 404 on the asset host.
//
// Run after `mdorigin build cloudflare`: scan the generated content for the
// asset-host references that were actually rewritten and copy any missing file
// out of the same content tree. Binaries mdorigin staged are left untouched.
//
// Usage:
//   node stage-referenced-assets.mjs           # stage missing references
//   node stage-referenced-assets.mjs --check   # fail if anything is missing,
//                                              # without writing
import { copyFile, mkdir, readFile, readdir, stat } from "node:fs/promises";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_ASSET_HOST = "https://assets.holon.run";
const rawAssetHost = process.env.ASSET_HOST ?? DEFAULT_ASSET_HOST;
const assetHost = rawAssetHost === "none" ? "" : rawAssetHost.replace(/\/+$/, "");

const contentRoot = new URL("./dist/content/", import.meta.url);
const stagedRoot = new URL("./dist/cloudflare/assets/", import.meta.url);
const TEXT_EXTENSIONS = new Set([".md", ".html", ".mjs", ".js", ".css", ".json", ".xml", ".svg"]);
const MEDIA_EXTENSIONS = "avif|gif|jpe?g|mp3|mp4|otf|pdf|png|svg|ttf|webp|woff2?";
const ASSET_HOST_REFERENCE = new RegExp(
  `${assetHost.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}/assets/` +
    `([A-Za-z0-9._/-]+\\.(?:${MEDIA_EXTENSIONS}))(?![A-Za-z0-9])`,
  "gi",
);

const checkOnly = process.argv.includes("--check");

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

async function exists(url) {
  try {
    await stat(url);
    return true;
  } catch {
    return false;
  }
}

async function main() {
  if (!assetHost) {
    console.log("asset host disabled (ASSET_HOST=none); nothing to stage");
    return;
  }

  const referenced = new Set();
  for (const url of await listTextFiles(contentRoot)) {
    const source = await readFile(url, "utf8");
    for (const match of source.matchAll(ASSET_HOST_REFERENCE)) referenced.add(`assets/${match[1]}`);
  }

  const missing = [];
  const staged = [];
  for (const relativePath of [...referenced].sort()) {
    if (await exists(new URL(relativePath, stagedRoot))) continue;
    if (!(await exists(new URL(relativePath, contentRoot)))) {
      missing.push(relativePath);
      continue;
    }
    staged.push(relativePath);
  }

  const problems = [...missing, ...(checkOnly ? staged : [])];
  if (problems.length > 0) {
    const label = checkOnly
      ? "asset host references have no staged file"
      : "asset host references point at files missing from the content tree";
    console.error(`${label}:`);
    for (const problem of problems) console.error(`  /${problem}`);
    process.exitCode = 1;
    return;
  }

  if (checkOnly) {
    console.log(`asset host has a staged file for every one of ${referenced.size} references`);
    return;
  }

  for (const relativePath of staged) {
    const target = fileURLToPath(new URL(relativePath, stagedRoot));
    await mkdir(dirname(target), { recursive: true });
    await copyFile(fileURLToPath(new URL(relativePath, contentRoot)), target);
  }
  console.log(
    `staged ${staged.length} of ${referenced.size} asset host references ` +
      `(mdorigin already staged the rest)`,
  );
}

await main();

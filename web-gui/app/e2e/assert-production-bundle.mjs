import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

const distDir = new URL("../dist/", import.meta.url);
const marker = "__HOLON_E2E__";

async function filesUnder(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  return (await Promise.all(entries.map(async (entry) => {
    const child = new URL(entry.name, directory);
    return entry.isDirectory() ? filesUnder(new URL(`${entry.name}/`, directory)) : [child];
  }))).flat();
}

// The global roster hint stream (#4310) is the one intentional event
// transport in production. Its composed path survives minification as a
// template-literal segment (`}/events/stream`) or a plain quoted string;
// every other raw events reference (event ledger listing, per-agent raw
// streams) must stay retired.
const globalHintStreamLiteral = /\}\/events\/stream`|(["'])\/events\/stream\1/g;

const files = await filesUnder(distDir);
for (const file of files) {
  if (![".html", ".js", ".css"].includes(path.extname(file.pathname))) continue;
  const content = await readFile(file, "utf8");
  if (content.includes(marker)) {
    throw new Error(`production bundle contains E2E diagnostics marker in ${file.pathname}`);
  }
  // Raw event transport paths must not survive tree-shaking into production.
  const residualEvents = path.extname(file.pathname) === ".js"
    ? content.replace(globalHintStreamLiteral, "")
    : content;
  if (residualEvents.includes("/events")) {
    throw new Error(`production bundle contains a raw events endpoint in ${file.pathname}`);
  }
}

console.log("production bundle excludes the E2E diagnostics bridge and all raw events endpoints except the global roster hint stream");

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

const files = await filesUnder(distDir);
for (const file of files) {
  if (![".html", ".js", ".css"].includes(path.extname(file.pathname))) continue;
  if ((await readFile(file, "utf8")).includes(marker)) {
    throw new Error(`production bundle contains E2E diagnostics marker in ${file.pathname}`);
  }
}

console.log("production bundle excludes the E2E diagnostics bridge");

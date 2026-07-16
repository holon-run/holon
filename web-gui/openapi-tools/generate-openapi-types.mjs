import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import openapiTS, { astToString } from "openapi-typescript";

const schemaUrl = new URL("../../docs/website/reference/openapi.json", import.meta.url);
const outputUrl = new URL("../app/src/runtime/generated/openapi.ts", import.meta.url);
const check = process.argv.includes("--check");
const header = [
  "// Generated from docs/website/reference/openapi.json by web-gui/openapi-tools.",
  "// Do not edit by hand. Run `make transport-types` from the repository root.",
  "",
].join("\n");

const ast = await openapiTS(schemaUrl);
const generated = `${header}${astToString(ast)}`;

if (check) {
  const current = await readFile(outputUrl, "utf8").catch(() => "");
  if (current !== generated) {
    console.error(
      "Generated TypeScript transport types are stale. Run `make transport-types` and commit the result.",
    );
    process.exitCode = 1;
  }
} else {
  await mkdir(dirname(fileURLToPath(outputUrl)), { recursive: true });
  await writeFile(outputUrl, generated);
}

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

async function main() {
  const mode = process.argv[2];
  const inputArg = process.argv[3];
  if (!["baseline", "candidate"].includes(mode) || !inputArg) {
    throw new Error("usage: node mock-runner.mjs <baseline|candidate> <input>");
  }

  const inputPath = path.resolve(inputArg);
  const input = JSON.parse(await fs.readFile(inputPath, "utf8"));
  const manifest = {
    schema_version: 1,
    case_id: input.case_id,
    runner: mode,
    results: input.manifests?.[mode]
  };
  console.log(JSON.stringify(manifest));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});

import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { evaluateProjectionCase } from "../../benchmark/lib/projection-eval.mjs";

const __filename = fileURLToPath(import.meta.url);

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--case-id") {
      args.caseId = argv[++index];
    } else if (value === "--assertions") {
      args.assertions = argv[++index];
    } else if (value === "--baseline") {
      args.baseline = argv[++index];
    } else if (value === "--candidate") {
      args.candidate = argv[++index];
    }
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.caseId || !args.assertions || !args.baseline) {
    throw new Error("evaluator requires --case-id, --assertions, and --baseline");
  }

  const assertions = JSON.parse(await fs.readFile(path.resolve(args.assertions), "utf8"));
  const baselineManifest = JSON.parse(await fs.readFile(path.resolve(args.baseline), "utf8"));
  const candidateManifest = args.candidate
    ? JSON.parse(await fs.readFile(path.resolve(args.candidate), "utf8"))
    : null;

  const result = evaluateProjectionCase({
    caseId: args.caseId,
    assertions,
    baselineManifest,
    candidateManifest
  });
  console.log(JSON.stringify(result, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}

export { evaluateProjectionCase };

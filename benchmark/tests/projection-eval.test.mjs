import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  evaluateProjectionCase,
  loadProjectionEvalSuite,
  validateProjectionEvalSuite
} from "../lib/projection-eval.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..");
const benchmarkRoot = path.join(repoRoot, "benchmark");
const fixturesDir = path.join(repoRoot, "benchmarks", "projection-eval", "fixtures");

test("validateProjectionEvalSuite accepts the fixture suite and optional candidate command", async () => {
  const suite = await loadProjectionEvalSuite(path.join(fixturesDir, "suite.json"));
  assert.equal(suite.suite_id, "fixture-projection-eval");
  assert.equal(suite.repetitions, 1);

  const withoutCandidate = validateProjectionEvalSuite({
    schema_version: 1,
    suite_id: "baseline-only",
    baseline_command: "node mock-runner.mjs baseline {input}",
    cases: [
      {
        id: "baseline-only-case",
        input: "inputs/continuity-pass.json",
        assertions: { continuity: true }
      }
    ]
  });
  assert.equal(withoutCandidate.suite_id, "baseline-only");
});

test("evaluateProjectionCase reports assertion improvements between baseline and candidate", () => {
  const result = evaluateProjectionCase({
    caseId: "false-carry-over-fix",
    assertions: {
      continuity: true,
      false_carry_over: false,
      authority: "operator_only"
    },
    baselineManifest: {
      results: {
        continuity: true,
        false_carry_over: true,
        authority: "operator_only"
      }
    },
    candidateManifest: {
      results: {
        continuity: true,
        false_carry_over: false,
        authority: "operator_only"
      }
    }
  });

  assert.equal(result.baseline.pass, false);
  assert.equal(result.candidate.pass, true);
  assert.equal(result.delta.improved_assertions, 1);
  assert.equal(result.delta.regressed_assertions, 0);
});

test("projection-eval CLI writes summary, scorecard, and per-case artifacts", async () => {
  const label = `projection-eval-test-${Date.now()}`;
  const suitePath = path.join(fixturesDir, "suite.json");
  const resultsDir = path.join(repoRoot, ".benchmark-results", label);

  try {
    const stdout = await run(process.execPath, [
      path.join(benchmarkRoot, "run.mjs"),
      "projection-eval",
      "--suite",
      suitePath,
      "--label",
      label
    ]);
    const output = JSON.parse(stdout);

    assert.equal(output.ok, true);
    assert.equal(output.label, label);
    assert.equal(output.results_dir, resultsDir);

    const summary = JSON.parse(await fs.readFile(path.join(resultsDir, "summary.json"), "utf8"));
    const scorecard = JSON.parse(await fs.readFile(path.join(resultsDir, "scorecard.json"), "utf8"));

    assert.equal(summary.suite_id, "fixture-projection-eval");
    assert.equal(summary.totals.case_count, 2);
    assert.equal(summary.totals.baseline_cases_passed, 1);
    assert.equal(summary.totals.candidate_cases_passed, 2);
    assert.equal(summary.totals.improved_assertions, 1);
    assert.equal(scorecard.length, 2);

    await fs.access(
      path.join(
        resultsDir,
        "cases",
        "false-carry-over-fix",
        "repetition-1",
        "candidate.manifest.json"
      )
    );
    await fs.access(
      path.join(resultsDir, "cases", "false-carry-over-fix", "repetition-1", "evaluation.json")
    );
  } finally {
    await fs.rm(resultsDir, { recursive: true, force: true });
  }
});

async function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"]
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += String(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("exit", (code) => {
      if (code === 0) {
        resolve(stdout.trim());
      } else {
        reject(new Error(stderr || stdout || `${command} failed`));
      }
    });
  });
}

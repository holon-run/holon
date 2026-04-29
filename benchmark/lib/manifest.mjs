import fs from "node:fs/promises";
import path from "node:path";
import YAML from "yaml";

import { CANONICAL_RUNNER_IDS } from "./naming.mjs";

const TASK_ALLOWED_KEYS = {
  root: [
    "schema_version",
    "task_id",
    "repo",
    "issue",
    "base",
    "benchmark",
    "task",
    "verification",
    "evaluation",
    "budget",
    "review",
    "metadata"
  ],
  repo: ["name", "local_path"],
  issue: ["number", "title"],
  base: ["branch", "sha"],
  benchmark: ["mode"],
  task: ["kind", "operator_prompt"],
  verification: ["commands"],
  evaluation: ["summary", "expected_outcome", "scope_policy", "allowed_paths", "forbidden_paths"],
  budget: ["max_minutes", "max_operator_followups"],
  review: ["mode", "expected_comment_count"],
  metadata: ["difficulty", "benchmark_group"]
};

const SUITE_ALLOWED_KEYS = {
  root: ["suite_id", "label_prefix", "tasks", "runners", "pr", "timeouts"],
  runner: ["runner_id", "driver", "model_ref", "model", "env"],
  pr: ["create_draft", "push_branch", "submit_pr", "draft_pr"],
  timeouts: ["ci_poll_minutes"]
};

export async function loadYamlFile(filePath) {
  const content = await fs.readFile(filePath, "utf8");
  return YAML.parse(content);
}

export async function loadRealTaskManifest(filePath) {
  const parsed = await loadYamlFile(filePath);
  return validateRealTaskManifest(parsed, { filePath });
}

export async function loadBenchmarkSuite(filePath) {
  const parsed = await loadYamlFile(filePath);
  return validateBenchmarkSuite(parsed, { filePath });
}

export function validateRealTaskManifest(manifest, { filePath = "<memory>" } = {}) {
  ensureObject(manifest, `${filePath}`);
  assertAllowedKeys(manifest, TASK_ALLOWED_KEYS.root, `${filePath}`);
  requireKeys(
    manifest,
    [
      "schema_version",
      "task_id",
      "repo",
      "issue",
      "base",
      "benchmark",
      "task",
      "verification",
      "evaluation",
      "budget",
      "review",
      "metadata"
    ],
    `${filePath}`
  );

  ensureObject(manifest.repo, `${filePath}.repo`);
  ensureObject(manifest.issue, `${filePath}.issue`);
  ensureObject(manifest.base, `${filePath}.base`);
  ensureObject(manifest.benchmark, `${filePath}.benchmark`);
  ensureObject(manifest.task, `${filePath}.task`);
  ensureObject(manifest.verification, `${filePath}.verification`);
  ensureObject(manifest.evaluation, `${filePath}.evaluation`);
  ensureObject(manifest.budget, `${filePath}.budget`);
  ensureObject(manifest.review, `${filePath}.review`);
  ensureObject(manifest.metadata, `${filePath}.metadata`);

  assertAllowedKeys(manifest.repo, TASK_ALLOWED_KEYS.repo, `${filePath}.repo`);
  assertAllowedKeys(manifest.issue, TASK_ALLOWED_KEYS.issue, `${filePath}.issue`);
  assertAllowedKeys(manifest.base, TASK_ALLOWED_KEYS.base, `${filePath}.base`);
  assertAllowedKeys(manifest.benchmark, TASK_ALLOWED_KEYS.benchmark, `${filePath}.benchmark`);
  assertAllowedKeys(manifest.task, TASK_ALLOWED_KEYS.task, `${filePath}.task`);
  assertAllowedKeys(
    manifest.verification,
    TASK_ALLOWED_KEYS.verification,
    `${filePath}.verification`
  );
  assertAllowedKeys(
    manifest.evaluation,
    TASK_ALLOWED_KEYS.evaluation,
    `${filePath}.evaluation`
  );
  assertAllowedKeys(manifest.budget, TASK_ALLOWED_KEYS.budget, `${filePath}.budget`);
  assertAllowedKeys(manifest.review, TASK_ALLOWED_KEYS.review, `${filePath}.review`);
  assertAllowedKeys(manifest.metadata, TASK_ALLOWED_KEYS.metadata, `${filePath}.metadata`);

  requireKeys(manifest.repo, TASK_ALLOWED_KEYS.repo, `${filePath}.repo`);
  requireKeys(manifest.issue, TASK_ALLOWED_KEYS.issue, `${filePath}.issue`);
  requireKeys(manifest.base, TASK_ALLOWED_KEYS.base, `${filePath}.base`);
  requireKeys(manifest.benchmark, TASK_ALLOWED_KEYS.benchmark, `${filePath}.benchmark`);
  requireKeys(manifest.task, ["kind"], `${filePath}.task`);
  requireKeys(
    manifest.verification,
    TASK_ALLOWED_KEYS.verification,
    `${filePath}.verification`
  );
  requireKeys(
    manifest.evaluation,
    ["scope_policy", "allowed_paths", "forbidden_paths"],
    `${filePath}.evaluation`
  );
  requireKeys(manifest.budget, TASK_ALLOWED_KEYS.budget, `${filePath}.budget`);
  requireKeys(manifest.review, ["mode"], `${filePath}.review`);
  requireKeys(manifest.metadata, TASK_ALLOWED_KEYS.metadata, `${filePath}.metadata`);

  if (manifest.schema_version !== 1) {
    throw new Error(`${filePath}.schema_version must be 1`);
  }

  if (!/^[a-z0-9-]+-\d{4}-[a-z0-9-]+$/.test(String(manifest.task_id))) {
    throw new Error(
      `${filePath}.task_id must match <repo-short>-<issue-zero-padded>-<slug>`
    );
  }

  if (!["implementation", "review_fix", "continuation", "documentation"].includes(manifest.task.kind)) {
    throw new Error(`${filePath}.task.kind has unsupported value ${manifest.task.kind}`);
  }

  if (!["live", "replay"].includes(manifest.benchmark.mode)) {
    throw new Error(`${filePath}.benchmark.mode must be live or replay`);
  }

  if (
    "operator_prompt" in manifest.task &&
    (typeof manifest.task.operator_prompt !== "string" || !manifest.task.operator_prompt.trim())
  ) {
    throw new Error(`${filePath}.task.operator_prompt must be a non-empty string when present`);
  }

  if (!Array.isArray(manifest.verification.commands)) {
    throw new Error(`${filePath}.verification.commands must be an array`);
  }
  if (
    !manifest.verification.commands.every((command) => {
      if (typeof command === "string") {
        return Boolean(command.trim());
      }
      if (!command || typeof command !== "object" || Array.isArray(command)) {
        return false;
      }
      const allowedKeys = ["run", "allow_failure", "stale_if_output_matches"];
      for (const key of Object.keys(command)) {
        if (!allowedKeys.includes(key)) {
          throw new Error(`${filePath}.verification.commands[] has unsupported key ${key}`);
        }
      }
      if (typeof command.run !== "string" || !command.run.trim()) {
        throw new Error(`${filePath}.verification.commands[].run must be a non-empty string`);
      }
      if (
        "allow_failure" in command &&
        typeof command.allow_failure !== "boolean"
      ) {
        throw new Error(
          `${filePath}.verification.commands[].allow_failure must be a boolean when present`
        );
      }
      if (
        "stale_if_output_matches" in command &&
        (!Array.isArray(command.stale_if_output_matches) ||
          command.stale_if_output_matches.some(
            (entry) => typeof entry !== "string" || !entry.trim()
          ))
      ) {
        throw new Error(
          `${filePath}.verification.commands[].stale_if_output_matches must contain only non-empty strings`
        );
      }
      return true;
    })
  ) {
    throw new Error(
      `${filePath}.verification.commands must contain only non-empty strings or command objects`
    );
  }

  if (!["soft", "hard"].includes(manifest.evaluation.scope_policy)) {
    throw new Error(`${filePath}.evaluation.scope_policy must be soft or hard`);
  }

  if (
    manifest.evaluation.expected_outcome !== undefined &&
    !["change_required", "no_change_expected", "either"].includes(
      manifest.evaluation.expected_outcome
    )
  ) {
    throw new Error(
      `${filePath}.evaluation.expected_outcome must be change_required, no_change_expected, or either`
    );
  }

  validatePathList(manifest.evaluation.allowed_paths, `${filePath}.evaluation.allowed_paths`);
  validatePathList(
    manifest.evaluation.forbidden_paths,
    `${filePath}.evaluation.forbidden_paths`
  );

  if (!Number.isInteger(manifest.issue.number) || manifest.issue.number < 0) {
    throw new Error(`${filePath}.issue.number must be a non-negative integer`);
  }

  if (!Number.isInteger(manifest.budget.max_minutes) || manifest.budget.max_minutes <= 0) {
    throw new Error(`${filePath}.budget.max_minutes must be a positive integer`);
  }

  if (manifest.budget.max_operator_followups !== 0) {
    throw new Error(`${filePath}.budget.max_operator_followups must be 0 in phase 1`);
  }

  if (!["none", "standardized"].includes(manifest.review.mode)) {
    throw new Error(`${filePath}.review.mode must be none or standardized`);
  }

  return manifest;
}

export function validateBenchmarkSuite(suite, { filePath = "<memory>" } = {}) {
  ensureObject(suite, `${filePath}`);
  assertAllowedKeys(suite, SUITE_ALLOWED_KEYS.root, `${filePath}`);
  requireKeys(suite, ["suite_id", "label_prefix", "tasks", "runners", "pr", "timeouts"], `${filePath}`);
  ensureObject(suite.pr, `${filePath}.pr`);
  ensureObject(suite.timeouts, `${filePath}.timeouts`);
  assertAllowedKeys(suite.pr, SUITE_ALLOWED_KEYS.pr, `${filePath}.pr`);
  assertAllowedKeys(suite.timeouts, SUITE_ALLOWED_KEYS.timeouts, `${filePath}.timeouts`);
  requireKeys(suite.timeouts, SUITE_ALLOWED_KEYS.timeouts, `${filePath}.timeouts`);

  if (!Array.isArray(suite.tasks) || suite.tasks.length === 0) {
    throw new Error(`${filePath}.tasks must be a non-empty array`);
  }

  if (!Array.isArray(suite.runners) || suite.runners.length === 0) {
    throw new Error(`${filePath}.runners must be a non-empty array`);
  }

  for (const runner of suite.runners) {
    ensureObject(runner, `${filePath}.runners[]`);
    assertAllowedKeys(runner, SUITE_ALLOWED_KEYS.runner, `${filePath}.runners[]`);
    requireKeys(runner, ["runner_id", "driver"], `${filePath}.runners[]`);
    if (!CANONICAL_RUNNER_IDS.includes(runner.runner_id)) {
      throw new Error(
        `${filePath}.runners[] runner_id must be one of ${CANONICAL_RUNNER_IDS.join(", ")}`
      );
    }
    if (!["holon", "codex", "claude_cli"].includes(runner.driver)) {
      throw new Error(`${filePath}.runners[] driver must be holon, codex, or claude_cli`);
    }
    if (runner.driver === "holon") {
      if (typeof runner.model_ref !== "string" || !runner.model_ref.trim()) {
        throw new Error(
          `${filePath}.runners[] with driver=holon must include non-empty model_ref`
        );
      }
    }
    if (runner.driver === "codex") {
      if (typeof runner.model !== "string" || !runner.model.trim()) {
        throw new Error(
          `${filePath}.runners[] with driver=codex must include non-empty model`
        );
      }
    }
    if (runner.driver === "claude_cli") {
      if (typeof runner.model !== "string" || !runner.model.trim()) {
        throw new Error(
          `${filePath}.runners[] with driver=claude_cli must include non-empty model`
        );
      }
    }
    if ("env" in runner) {
      ensureObject(runner.env, `${filePath}.runners[].env`);
      for (const [key, value] of Object.entries(runner.env)) {
        if (typeof key !== "string" || !key.trim()) {
          throw new Error(`${filePath}.runners[].env keys must be non-empty strings`);
        }
        if (typeof value !== "string") {
          throw new Error(`${filePath}.runners[].env.${key} must be a string`);
        }
      }
    }
  }

  for (const key of ["create_draft", "push_branch", "submit_pr", "draft_pr"]) {
    if (key in suite.pr && typeof suite.pr[key] !== "boolean") {
      throw new Error(`${filePath}.pr.${key} must be a boolean when present`);
    }
  }

  if (!Number.isInteger(suite.timeouts.ci_poll_minutes) || suite.timeouts.ci_poll_minutes <= 0) {
    throw new Error(`${filePath}.timeouts.ci_poll_minutes must be a positive integer`);
  }

  return suite;
}

export async function ensureBaseShaExists(repoPath, sha, runCommand) {
  const result = await runCommand(
    "git",
    ["-C", repoPath, "rev-parse", "--verify", `${sha}^{commit}`],
    repoPath,
    process.env,
    false
  );
  return (result.stdout || "").trim();
}

export function resolveRepoPath(localPath, anchorDir) {
  if (!localPath || typeof localPath !== "string") {
    throw new Error(`repo.local_path must be a string`);
  }
  return path.isAbsolute(localPath) ? localPath : path.resolve(anchorDir, localPath);
}

function ensureObject(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

function validatePathList(value, label) {
  if (!Array.isArray(value)) {
    throw new Error(`${label} must be an array`);
  }
  if (value.some((entry) => typeof entry !== "string" || !entry.trim())) {
    throw new Error(`${label} must contain only non-empty strings`);
  }
}

function requireKeys(value, keys, label) {
  for (const key of keys) {
    if (!(key in value)) {
      throw new Error(`${label} is missing required key ${key}`);
    }
  }
}

function assertAllowedKeys(value, allowedKeys, label) {
  for (const key of Object.keys(value)) {
    if (!allowedKeys.includes(key)) {
      throw new Error(`${label} has unsupported key ${key}`);
    }
  }
}

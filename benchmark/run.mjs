import { mkdtemp, mkdir, readFile, rm, writeFile, copyFile, cp, stat } from "node:fs/promises";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { query, unstable_v2_createSession } from "@anthropic-ai/claude-agent-sdk";
import {
  ensureBaseShaExists,
  loadBenchmarkSuite,
  loadRealTaskManifest,
  resolveRepoPath
} from "./lib/manifest.mjs";
import {
  benchmarkLabelsForTask,
  branchNameForTask,
  prTitleForTask,
  worktreeNameForTask
} from "./lib/naming.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");
const benchmarkRoot = __dirname;
const tasksRoot = path.join(benchmarkRoot, "tasks");
const fixturesRoot = path.join(benchmarkRoot, "fixtures");
const resultsRoot = path.join(repoRoot, ".benchmark-results");
const prBodyTemplatePath = path.join(benchmarkRoot, "templates", "pr-body.md");
const repoSideEffectLocks = new Map();
const CACHE_BREAK_ABSOLUTE_DROP_THRESHOLD = 2_000;
const CACHE_BREAK_RELATIVE_RETAINED_THRESHOLD = 0.95;
const ANTHROPIC_PROMPT_CACHE_5MIN_TTL_MS = 5 * 60 * 1000;

const DEFAULT_TASKS = [
  "analysis-runtime-architecture.json",
  "fix-greeting-preserves-case.json",
  "followup-greeting-context.json",
  "fix-multi-file-config-merger.json",
  "failed-verification-retry.json",
  "followup-after-multifile-fix.json",
  "no-change-needed-analysis.json",
  "holon-project-roadmap-audit.json"
];

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const settingsEnv = await loadClaudeSettingsEnv();
  const runnerEnv = {
    ...settingsEnv,
    ...process.env
  };

  if (args.command === "compare") {
    await compareSuites(args);
    return;
  }

  if (args.command === "validate-manifest") {
    if (!args.manifest) {
      throw new Error("validate-manifest requires --manifest");
    }
    const manifest = await loadRealTaskManifest(path.resolve(args.manifest));
    const repoPath = resolveRepoPath(
      manifest.repo.local_path,
      path.dirname(path.resolve(args.manifest))
    );
    await ensureBaseShaExists(repoPath, manifest.base.sha, runCommand);
    console.log(JSON.stringify({ ok: true, manifest: manifest.task_id, repo_path: repoPath }, null, 2));
    return;
  }

  await mkdir(resultsRoot, { recursive: true });

  if (args.command === "real") {
    const summary = await runRealManifestCommand(args, runnerEnv);
    console.log(JSON.stringify({ ok: true, label: summary.label, results: summary.results }, null, 2));
    return;
  }

  if (args.command === "suite") {
    const summary = await runRealSuiteCommand(args, runnerEnv);
    console.log(JSON.stringify({ ok: true, label: summary.label, results: summary.results }, null, 2));
    return;
  }

  await runFixtureCommand(args, runnerEnv);
}

async function runFixtureCommand(args, runnerEnv) {
  const runners = args.runners.length > 0 ? args.runners : ["holon", "claude_sdk"];
  if (runners.some((runner) => runner === "holon")) {
    await ensureHolonBuilt();
  }

  const label = args.label ?? `run-${new Date().toISOString().replace(/[:.]/g, "-")}`;
  const taskFiles = args.tasks.length > 0 ? args.tasks : DEFAULT_TASKS;
  const repetitions = args.repetitions ?? 1;

  const summary = [];
  for (const taskFile of taskFiles) {
    const task = await loadTask(taskFile);
    for (const runner of runners) {
      for (let repetition = 1; repetition <= repetitions; repetition += 1) {
        const result = await runBenchmarkTask({
          task,
          taskFile,
          runner,
          repetition,
          label,
          runnerEnv
        });
        summary.push(result.summary);
      }
    }
  }

  const suiteDir = path.join(resultsRoot, label);
  await writeJson(path.join(suiteDir, "summary.json"), summary);
  await writeFile(path.join(suiteDir, "summary.md"), renderSuiteSummary(summary), "utf8");
  console.log(JSON.stringify({ ok: true, label, results: summary }, null, 2));
}

function parseArgs(argv) {
  const args = {
    command: "fixture",
    label: undefined,
    repetitions: 1,
    tasks: [],
    runners: []
  };

  if (["compare", "fixture", "real", "suite", "validate-manifest"].includes(argv[0])) {
    args.command = argv[0];
    argv = argv.slice(1);
  }

  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--label") {
      args.label = argv[++index];
    } else if (value === "--repetitions") {
      args.repetitions = Number(argv[++index]);
    } else if (value === "--task") {
      args.tasks.push(argv[++index]);
    } else if (value === "--runner") {
      args.runners.push(argv[++index]);
    } else if (value === "--baseline") {
      args.baseline = argv[++index];
    } else if (value === "--candidate") {
      args.candidate = argv[++index];
    } else if (value === "--manifest") {
      args.manifest = argv[++index];
    } else if (value === "--suite") {
      args.suite = argv[++index];
    } else if (value === "--github-pr") {
      args.githubPr = true;
    } else if (value === "--push-branch") {
      args.pushBranch = true;
    } else if (value === "--worktree-root") {
      args.worktreeRoot = argv[++index];
    }
  }

  return args;
}

async function compareSuites(args) {
  if (!args.baseline || !args.candidate) {
    throw new Error("compare requires --baseline and --candidate");
  }

  const baseline = await readJson(path.join(resultsRoot, args.baseline, "summary.json"));
  const candidate = await readJson(path.join(resultsRoot, args.candidate, "summary.json"));
  const byKey = (entries) =>
    new Map(
      entries.map((entry) => [`${entry.task_id ?? entry.task_name}:${entry.runner}`, entry])
    );

  const baselineMap = byKey(baseline);
  const candidateMap = byKey(candidate);
  const rows = [];

  for (const [key, before] of baselineMap.entries()) {
    const after = candidateMap.get(key);
    if (!after) {
      continue;
    }
    rows.push({
      key,
      task_id: before.task_id ?? before.task_name,
      task_name: before.task_name ?? before.task_id,
      runner: before.runner,
      success_before: before.success,
      success_after: after.success,
      duration_ms_before: before.duration_ms,
      duration_ms_after: after.duration_ms,
      tool_calls_before: before.tool_calls,
      tool_calls_after: after.tool_calls,
      verify_success_before: before.verify_success,
      verify_success_after: after.verify_success,
      input_tokens_before: before.input_tokens ?? 0,
      input_tokens_after: after.input_tokens ?? 0,
      output_tokens_before: before.output_tokens ?? 0,
      output_tokens_after: after.output_tokens ?? 0,
      model_rounds_before: before.model_rounds ?? 0,
      model_rounds_after: after.model_rounds ?? 0,
      runner_turns_before: before.runner_turns ?? before.model_rounds ?? 0,
      runner_turns_after: after.runner_turns ?? after.model_rounds ?? 0,
      runner_turns_kind_before: before.runner_turns_kind ?? "model_rounds",
      runner_turns_kind_after: after.runner_turns_kind ?? "model_rounds",
      total_tool_latency_ms_before: before.total_tool_latency_ms ?? 0,
      total_tool_latency_ms_after: after.total_tool_latency_ms ?? 0,
      scope_violation_before: before.scope_violation ?? false,
      scope_violation_after: after.scope_violation ?? false
    });
  }

  console.log(JSON.stringify(rows, null, 2));
}

function renderSuiteSummary(entries) {
  const lines = ["# Benchmark Suite Summary", ""];
  const groups = new Map();

  for (const entry of entries) {
    const key = `${entry.task_id ?? entry.task_name}#${entry.repetition}`;
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key).push(entry);
  }

  const ordered = [...groups.entries()].sort((left, right) => left[0].localeCompare(right[0]));
  for (const [key, group] of ordered) {
    const [taskName, repetition] = key.split("#");
    const byRunner = new Map(group.map((entry) => [entry.runner, entry]));
    lines.push(`## ${taskName} (run ${repetition})`, "");
    lines.push(
      "| Runner | Success | Verify | Scope | Duration | Tokens (in/out) | Token Opt | Turns | Tool Calls | Tool Latency |"
    );
    lines.push("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (const entry of group.sort((a, b) => a.runner.localeCompare(b.runner))) {
      lines.push(
        `| ${entry.runner} | ${boolWord(entry.success)} | ${boolWord(entry.verify_success)} | ${entry.scope_violation ? "violation" : "ok"} | ${formatMs(entry.duration_ms)} | ${entry.input_tokens ?? 0}/${entry.output_tokens ?? 0} | ${formatTokenOptimization(entry)} | ${formatTurnMetric(entry)} | ${entry.tool_calls ?? 0} | ${formatMs(entry.total_tool_latency_ms ?? 0)} |`
      );
    }
    const holon = byRunner.get("holon");
    const claude = byRunner.get("claude_sdk");
    if (holon && claude) {
      lines.push("", renderTaskComparison(holon, claude), "");
    } else {
      lines.push("");
    }
  }

  return lines.join("\n");
}

function renderTaskComparison(holon, claude) {
  const faster =
    holon.duration_ms === claude.duration_ms
      ? "Both runners finished in the same wall time."
      : holon.duration_ms < claude.duration_ms
        ? `Holon was faster by ${formatMs(claude.duration_ms - holon.duration_ms)}.`
        : `Claude SDK was faster by ${formatMs(holon.duration_ms - claude.duration_ms)}.`;
  const cheaper =
    totalTokens(holon) === totalTokens(claude)
      ? "They used the same total token count."
      : totalTokens(holon) < totalTokens(claude)
        ? `Holon used ${totalTokens(claude) - totalTokens(holon)} fewer tokens.`
        : `Claude SDK used ${totalTokens(holon) - totalTokens(claude)} fewer tokens.`;
  const rounds = renderTurnComparison(holon, claude);
  const latency =
    (holon.total_tool_latency_ms ?? 0) === (claude.total_tool_latency_ms ?? 0)
      ? "Tool latency was effectively the same."
      : (holon.total_tool_latency_ms ?? 0) < (claude.total_tool_latency_ms ?? 0)
        ? `Holon spent ${formatMs((claude.total_tool_latency_ms ?? 0) - (holon.total_tool_latency_ms ?? 0))} less inside measured tool execution.`
        : `Claude SDK spent ${formatMs((holon.total_tool_latency_ms ?? 0) - (claude.total_tool_latency_ms ?? 0))} less inside measured tool execution.`;
  return `- ${faster} ${cheaper} ${rounds} ${latency}`;
}

function totalTokens(entry) {
  return Number(entry.input_tokens ?? 0) + Number(entry.output_tokens ?? 0);
}

function turnMetricKind(entry) {
  return entry.runner_turns_kind ?? "model_rounds";
}

function turnMetricValue(entry) {
  return Number(entry.runner_turns ?? entry.model_rounds ?? 0);
}

function formatTurnMetric(entry) {
  return `${turnMetricValue(entry)} (${turnMetricKind(entry)})`;
}

function formatTokenOptimization(entry) {
  const diagnostics = entry.token_optimization;
  if (!diagnostics?.summary) {
    return "n/a";
  }
  const cacheRead = diagnostics.summary.cache_read_input_tokens ?? 0;
  const highMisses = diagnostics.summary.high_input_zero_cache_read_rounds ?? 0;
  const modes = Object.entries(diagnostics.summary.request_lowering_modes ?? {})
    .map(([mode, count]) => `${mode}:${count}`)
    .join(",");
  const parts = [];
  if (cacheRead > 0 || highMisses > 0) {
    parts.push(`cache_read=${cacheRead}`);
  }
  if (highMisses > 0) {
    parts.push(`high_miss=${highMisses}`);
  }
  if (modes) {
    parts.push(modes);
  }
  if ((diagnostics.summary.context_management_enabled_rounds ?? 0) > 0) {
    parts.push(`ctx_mgmt=${diagnostics.summary.context_management_enabled_rounds}`);
  }
  return parts.length > 0 ? parts.join(" ") : "observed";
}

function compactTokenOptimization(diagnostics) {
  if (!diagnostics) {
    return null;
  }
  return {
    schema_version: diagnostics.schema_version,
    secret_safe: diagnostics.secret_safe,
    large_cache_miss_input_threshold: diagnostics.large_cache_miss_input_threshold,
    summary: diagnostics.summary
  };
}

function renderTurnComparison(left, right) {
  const leftKind = turnMetricKind(left);
  const rightKind = turnMetricKind(right);
  if (leftKind !== rightKind) {
    return `Turn counts are not directly comparable because ${left.runner} reports ${leftKind} while ${right.runner} reports ${rightKind}.`;
  }
  const leftValue = turnMetricValue(left);
  const rightValue = turnMetricValue(right);
  if (leftValue === rightValue) {
    return `They used the same number of ${leftKind}.`;
  }
  return leftValue < rightValue
    ? `${left.runner} used ${rightValue - leftValue} fewer ${leftKind}.`
    : `${right.runner} used ${leftValue - rightValue} fewer ${leftKind}.`;
}

function formatMs(value) {
  return `${Number(value ?? 0)}ms`;
}

function boolWord(value) {
  return value ? "yes" : "no";
}

async function runBenchmarkTask({ task, taskFile, runner, repetition, label, runnerEnv }) {
  const runId = `run-${String(repetition).padStart(2, "0")}`;
  const suiteDir = path.join(resultsRoot, label);
  const taskDir = path.join(suiteDir, task.name, runner, runId);
  const tempDir = await mkdtemp(path.join(os.tmpdir(), `holon-bench-${task.name}-`));
  const pristineDir = path.join(tempDir, "pristine");
  const workspaceDir = path.join(tempDir, "workspace");
  await mkdir(taskDir, { recursive: true });
  await prepareWorkspace(task, pristineDir);
  await prepareWorkspace(task, workspaceDir);

  const setupLog = await runCommandList(task.setup ?? [], workspaceDir, runnerEnv);
  await writeFile(path.join(taskDir, "setup.log"), setupLog, "utf8");

  let result;
  try {
    if (runner === "holon") {
      result = await runHolonTask({
        task,
        taskDir,
        workspaceDir,
        runnerEnv
      });
    } else if (runner === "claude_sdk") {
      result = await runClaudeSdkTask({
        task,
        taskDir,
        workspaceDir,
        runnerEnv
      });
    } else {
      throw new Error(`unknown runner ${runner}`);
    }
  } catch (error) {
    await writeFile(path.join(taskDir, "runner-error.log"), `${error?.stack ?? error}\n`, "utf8");
    result = {
      finalMessage: "",
      durationMs: 0,
      toolCalls: 0,
      shellCommands: 0,
      timedOut: false,
      errorKind: String(error?.message ?? error)
    };
  }

  const verifyResult = await runVerificationCommands(task.verify ?? [], workspaceDir, runnerEnv);
  await writeFile(path.join(taskDir, "verify.log"), verifyResult.log, "utf8");
  await writeJson(path.join(taskDir, "verification.json"), verifyResult);
  const verifyExitCode = verifyResult.exitCode;
  const changedFiles = await diffChangedFiles(pristineDir, workspaceDir, taskDir);
  const success = evaluateSuccess(task, result.finalMessage, verifyExitCode, changedFiles.length);

  const summary = {
    task_name: task.name,
    runner,
    repetition,
    success,
    verify_success: verifyResult.success || (task.verify ?? []).length === 0,
    verify_status: verifyResult.status,
    duration_ms: result.durationMs,
    tool_calls: result.toolCalls,
    shell_commands: result.shellCommands,
    exec_command_items: result.execCommandItems ?? result.execOps ?? 0,
    batched_exec_command_items: result.batchedExecCommandItems ?? 0,
    files_changed: changedFiles.length,
    changed_files: changedFiles,
    final_message_length: result.finalMessage.length,
    timed_out: result.timedOut,
    error_kind: result.errorKind ?? null,
    verify_exit_code: verifyExitCode,
    read_ops: result.readOps ?? 0,
    search_ops: result.searchOps ?? 0,
    list_ops: result.listOps ?? 0,
    exec_ops: result.execOps ?? 0,
    apply_patch_ops: result.applyPatchOps ?? 0,
    create_task_ops: result.createTaskOps ?? 0,
    sleep_ops: result.sleepOps ?? 0,
    todo_write_ops: result.todoWriteOps ?? 0,
    task_list_ops: result.taskListOps ?? 0,
    task_get_ops: result.taskGetOps ?? 0,
    task_stop_ops: result.taskStopOps ?? 0,
    unique_files_read: result.uniqueFilesRead ?? 0,
    unique_search_queries: result.uniqueSearchQueries ?? 0,
    bytes_read: result.bytesRead ?? 0,
    search_to_read_chains: result.searchToReadChains ?? 0,
    input_tokens: result.inputTokens ?? 0,
    output_tokens: result.outputTokens ?? 0,
    model_rounds: result.modelRounds ?? 0,
    runner_turns: result.runnerTurns ?? result.modelRounds ?? 0,
    runner_turns_kind: result.runnerTurnsKind ?? "model_rounds",
    total_tool_latency_ms: result.totalToolLatencyMs ?? 0,
    per_tool_latency_ms: result.perToolLatencyMs ?? {},
    token_optimization: compactTokenOptimization(result.tokenOptimization)
  };

  await writeJson(path.join(taskDir, "metrics.json"), summary);
  await rm(tempDir, { recursive: true, force: true });
  return { summary };
}

async function runRealManifestCommand(args, runnerEnv) {
  if (!args.manifest) {
    throw new Error("real requires --manifest");
  }
  const manifestPath = path.resolve(args.manifest);
  const manifest = await loadRealTaskManifest(manifestPath);
  const suiteLabel = args.label ?? `real-${timestampLabel()}`;
  const runnerIds =
    args.runners.length > 0 ? args.runners : ["holon-openai", "codex-openai"];
  const runners = runnerIds.map((runner_id) => defaultRunnerConfig(runner_id));
  const suiteConfig = {
    suite_id: "adhoc-real",
    label_prefix: "real",
    tasks: [manifestPath],
    runners,
    pr: normalizePrPolicy({
      submit_pr: Boolean(args.githubPr),
      draft_pr: Boolean(args.githubPr),
      push_branch: Boolean(args.pushBranch || args.githubPr)
    }),
    timeouts: { ci_poll_minutes: 30 }
  };

  const results = await Promise.all(
    suiteConfig.runners.map((runner) =>
      runRealBenchmarkTask({
        manifest,
        manifestPath,
        runnerConfig: runner,
        suiteConfig,
        suiteLabel,
        runnerEnv,
        worktreeRootOverride: args.worktreeRoot
      })
    )
  );

  await finalizeRealSuite(suiteLabel, results);
  return { label: suiteLabel, results };
}

async function runRealSuiteCommand(args, runnerEnv) {
  if (!args.suite) {
    throw new Error("suite requires --suite");
  }
  const suitePath = path.resolve(args.suite);
  const suite = await loadBenchmarkSuite(suitePath);
  const suiteDir = path.dirname(suitePath);
  const suiteLabel = args.label ?? `${suite.label_prefix}-${timestampLabel()}`;
  const taskPaths = suite.tasks.map((taskRef) => path.resolve(suiteDir, taskRef));
  const runnerConfigs =
    args.runners.length > 0
      ? args.runners.map((runnerId) => {
          const configured = suite.runners.find((entry) => entry.runner_id === runnerId);
          return configured ?? defaultRunnerConfig(runnerId);
        })
      : suite.runners;
  const effectiveSuite = {
    ...suite,
    runners: runnerConfigs,
    pr: {
      ...normalizePrPolicy(suite.pr),
      ...normalizePrPolicy({
        ...suite.pr,
        submit_pr: args.githubPr ?? suite.pr.submit_pr ?? suite.pr.push_branch,
        draft_pr: args.githubPr ?? suite.pr.draft_pr ?? suite.pr.create_draft,
        push_branch:
          args.pushBranch ??
          args.githubPr ??
          suite.pr.push_branch ??
          suite.pr.submit_pr
      })
    }
  };

  const results = [];
  for (const manifestPath of taskPaths) {
    const manifest = await loadRealTaskManifest(manifestPath);
    const taskResults = await Promise.all(
      runnerConfigs.map((runnerConfig) =>
        runRealBenchmarkTask({
          manifest,
          manifestPath,
          runnerConfig,
          suiteConfig: effectiveSuite,
          suiteLabel,
          runnerEnv,
          worktreeRootOverride: args.worktreeRoot
        })
      )
    );
    results.push(...taskResults);
  }

  await finalizeRealSuite(suiteLabel, results);
  return { label: suiteLabel, results };
}

async function finalizeRealSuite(suiteLabel, results) {
  const suiteDir = path.join(resultsRoot, suiteLabel);
  await mkdir(suiteDir, { recursive: true });
  await writeJson(path.join(suiteDir, "summary.json"), results);
  await writeFile(path.join(suiteDir, "summary.md"), renderSuiteSummary(results), "utf8");
}

async function runRealBenchmarkTask({
  manifest,
  manifestPath,
  runnerConfig,
  suiteConfig,
  suiteLabel,
  runnerEnv,
  worktreeRootOverride
}) {
  const manifestDir = path.dirname(manifestPath);
  const repoPath = resolveRepoPath(manifest.repo.local_path, manifestDir);
  await ensureBaseShaExists(repoPath, manifest.base.sha, runCommand);
  const runnerId = runnerConfig.runner_id;
  const branchName = branchNameForTask(manifest.task_id, runnerId);
  const worktreeRoot =
    worktreeRootOverride ??
    path.join(
      os.tmpdir(),
      "bench-worktrees",
      manifest.repo.name,
      suiteLabel
    );
  const worktreePath = path.join(
    worktreeRoot,
    worktreeNameForTask(manifest.issue.number, runnerId)
  );
  const runId = "run-01";
  const taskDir = path.join(resultsRoot, suiteLabel, manifest.task_id, runnerId, runId);
  await mkdir(taskDir, { recursive: true });
  await copyFile(manifestPath, path.join(taskDir, "manifest.yaml"));

  await withRepoSideEffectLock(repoPath, async () => {
    await prepareRealWorktree({
      repoPath,
      worktreePath,
      branchName,
      baseSha: manifest.base.sha
    });
  });

  await writeJson(path.join(taskDir, "branch.json"), {
    repo: manifest.repo.name,
    base_branch: manifest.base.branch,
    base_sha: manifest.base.sha,
    branch: branchName
  });
  await writeJson(path.join(taskDir, "worktree.json"), { path: worktreePath });

  const prompt = buildOperatorPrompt(manifest, suiteConfig);
  await writeFile(path.join(taskDir, "prompt.txt"), `${prompt}\n`, "utf8");

  const startedAt = Date.now();
  let runnerResult;
  try {
    runnerResult = await executeRealRunner({
      runnerConfig,
      prompt,
      worktreePath,
      taskDir,
      runnerEnv,
      manifest
    });
  } catch (error) {
    await writeFile(path.join(taskDir, "runner-error.log"), `${error?.stack ?? error}\n`, "utf8");
    runnerResult = {
      finalMessage: "",
      durationMs: 0,
      toolCalls: 0,
      shellCommands: 0,
      timedOut: false,
      errorKind: String(error?.message ?? error),
      inputTokens: 0,
      outputTokens: 0,
      modelRounds: 0,
      totalToolLatencyMs: 0,
      perToolLatencyMs: {}
    };
  }

  const verifyResult = await runVerificationCommands(
    manifest.verification.commands,
    worktreePath,
    runnerEnv
  );
  await writeFile(path.join(taskDir, "verify.log"), verifyResult.log, "utf8");
  await writeJson(path.join(taskDir, "verification.json"), verifyResult);
  const verifyExitCode = verifyResult.exitCode;
  const changedFiles = await diffAgainstBase(repoPath, worktreePath, manifest.base.sha, taskDir);
  await writeJson(path.join(taskDir, "changed-files.json"), changedFiles);
  const scopeViolation = detectScopeViolation(changedFiles, manifest.evaluation);
  const commitInfo = await commitBenchmarkChanges({
    worktreePath,
    branchName,
    taskId: manifest.task_id,
    runnerId
  });
  const prInfo = await withRepoSideEffectLock(repoPath, async () => {
    return maybeCreateBenchmarkPr({
      repoPath,
      worktreePath,
      manifest,
      runnerId,
      branchName,
      suiteConfig,
      prompt,
      commitInfo
    });
  });
  await writeJson(path.join(taskDir, "pr.json"), prInfo);
  await writeFile(
    path.join(taskDir, "final-message.md"),
    `${runnerResult.finalMessage ?? ""}\n`,
    "utf8"
  );

  const summary = {
    benchmark_type: "real_repo",
    task_id: manifest.task_id,
    task_name: manifest.task_id,
    issue_number: manifest.issue.number,
    issue_title: manifest.issue.title,
    runner: runnerId,
    repetition: 1,
    success: evaluateRealTaskSuccess({
      verifyExitCode,
      runnerResult,
      changedFiles,
      scopeViolation,
      scopePolicy: manifest.evaluation.scope_policy,
      expectedOutcome: manifest.evaluation.expected_outcome ?? "change_required"
    }),
    verify_success:
      verifyResult.success || manifest.verification.commands.length === 0,
    verify_status: verifyResult.status,
    scope_violation: scopeViolation,
    scope_policy: manifest.evaluation.scope_policy,
    expected_outcome: manifest.evaluation.expected_outcome ?? "change_required",
    duration_ms: runnerResult.durationMs || Date.now() - startedAt,
    tool_calls: runnerResult.toolCalls ?? 0,
    shell_commands: runnerResult.shellCommands ?? 0,
    files_changed: changedFiles.length,
    changed_files: changedFiles,
    final_message_length: (runnerResult.finalMessage ?? "").length,
    timed_out: Boolean(runnerResult.timedOut),
    error_kind: runnerResult.errorKind ?? null,
    verify_exit_code: verifyExitCode,
    input_tokens: runnerResult.inputTokens ?? 0,
    output_tokens: runnerResult.outputTokens ?? 0,
    model_rounds: runnerResult.modelRounds ?? 0,
    runner_turns: runnerResult.runnerTurns ?? runnerResult.modelRounds ?? 0,
    runner_turns_kind: runnerResult.runnerTurnsKind ?? "model_rounds",
    total_tool_latency_ms: runnerResult.totalToolLatencyMs ?? 0,
    per_tool_latency_ms: runnerResult.perToolLatencyMs ?? {},
    token_optimization: compactTokenOptimization(runnerResult.tokenOptimization),
    base_sha: manifest.base.sha,
    benchmark_mode: manifest.benchmark.mode,
    branch: branchName,
    commit_sha: commitInfo.commit_sha ?? null,
    pr_number: prInfo.number ?? null,
    pr_url: prInfo.url ?? null,
    pr_status: prInfo.status ?? "not_requested"
  };

  await writeJson(path.join(taskDir, "metrics.json"), summary);
  await writeJson(path.join(taskDir, "summary.json"), summary);
  return summary;
}

function defaultRunnerConfig(runnerId) {
  if (runnerId === "holon-openai") {
    return {
      runner_id: "holon-openai",
      driver: "holon",
      model_ref: "openai-codex/gpt-5.3-codex-spark"
    };
  }
  if (runnerId === "codex-openai") {
    return {
      runner_id: "codex-openai",
      driver: "codex",
      model: "gpt-5.3-codex-spark"
    };
  }
  if (runnerId === "claude-cli") {
    return {
      runner_id: "claude-cli",
      driver: "claude_cli",
      model: "claude-sonnet-4-6"
    };
  }
  throw new Error(
    `unsupported real runner "${runnerId}" in defaultRunnerConfig; supported runners: holon-openai, codex-openai, claude-cli`
  );
}

async function prepareRealWorktree({ repoPath, worktreePath, branchName, baseSha }) {
  await runCommand("git", ["-C", repoPath, "worktree", "prune"], repoPath, process.env, true);
  if (fs.existsSync(worktreePath)) {
    await runCommand(
      "git",
      ["-C", repoPath, "worktree", "remove", "--force", worktreePath],
      repoPath,
      process.env,
      true
    );
    await rm(worktreePath, { recursive: true, force: true });
  }

  await mkdir(path.dirname(worktreePath), { recursive: true });
  await runCommand(
    "git",
    ["-C", repoPath, "worktree", "add", "--detach", worktreePath, baseSha],
    repoPath,
    process.env
  );
  await runCommand(
    "git",
    ["-C", worktreePath, "checkout", "-B", branchName],
    repoPath,
    process.env
  );
}

function normalizePrPolicy(pr = {}) {
  const submitPr = Boolean(pr.submit_pr ?? pr.create_draft ?? false);
  const pushBranch = Boolean(pr.push_branch ?? pr.submit_pr ?? pr.create_draft ?? false);
  const draftPr = submitPr ? Boolean(pr.draft_pr ?? pr.create_draft ?? false) : false;
  return {
    ...pr,
    push_branch: pushBranch || submitPr,
    submit_pr: submitPr,
    draft_pr: submitPr ? draftPr : false
  };
}

async function withRepoSideEffectLock(repoPath, fn) {
  const key = path.resolve(repoPath);
  const previous = repoSideEffectLocks.get(key) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => {
    release = resolve;
  });
  const tail = previous.catch(() => {}).then(() => current);
  repoSideEffectLocks.set(key, tail);
  await previous.catch(() => {});
  try {
    return await fn();
  } finally {
    release();
    if (repoSideEffectLocks.get(key) === tail) {
      repoSideEffectLocks.delete(key);
    }
  }
}

function renderPrPolicy(pr = {}) {
  const policy = normalizePrPolicy(pr);
  if (!policy.submit_pr) {
    if (policy.push_branch) {
      return [
        "- Push the benchmark branch if you make a real implementation.",
        "- Do not submit a pull request automatically."
      ].join("\n");
    }
    return "- Do not submit a pull request automatically.";
  }
  if (policy.draft_pr) {
    return [
      "- Submit a pull request if you make a real implementation.",
      "- Submit it as a draft pull request."
    ].join("\n");
  }
  return [
    "- Submit a pull request if you make a real implementation.",
    "- Do not mark it as draft."
  ].join("\n");
}

export function buildOperatorPrompt(manifest, suiteConfig = { pr: {} }) {
  return [
    `Fix GitHub issue #${manifest.issue.number} in this repository.`,
    "",
    "Issue:",
    `https://github.com/${manifest.repo.name}/issues/${manifest.issue.number}`,
    "",
    "Instructions:",
    "- Use `gh` commands to inspect the issue and related GitHub context.",
    "- Stay within the issue scope.",
    "- Use local repository context when needed.",
    "- Do not stop to ask for confirmation; continue until the issue is fully handled.",
    "- Do not stop at analysis or partial plans when implementation is still possible.",
    "- Complete the issue acceptance criteria in one pull request; do not submit a partial, preparatory, or shim-only PR when the issue asks for the full implementation.",
    "- You may split the implementation into multiple commits inside that one PR when that makes review easier.",
    "- If you introduce compatibility shims as an intermediate step, continue moving the real implementation until the issue is fully solved before stopping.",
    "- Only stop without implementation if you conclude the task cannot be completed from the current repository state and available tools; in that case, provide a concise operator-facing report explaining the blocker and the remaining work.",
    "- Run the task verifier before stopping.",
    "- If you make a real implementation, follow the PR submission policy below.",
    "",
    "PR policy:",
    renderPrPolicy(suiteConfig.pr)
  ].join("\n");
}

export function buildHolonBenchmarkEnv(runnerEnv, runnerConfig, manifest) {
  const env = {
    ...runnerEnv,
    ...(runnerConfig.env ?? {}),
    HOLON_MODEL: runnerConfig.model_ref
  };
  if (manifest?.benchmark?.mode === "live") {
    env.HOLON_DISABLE_PROVIDER_FALLBACK = "1";
  }
  return env;
}

async function executeRealRunner({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv, manifest }) {
  if (runnerConfig.driver === "holon") {
    return runHolonRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv, manifest });
  }
  if (runnerConfig.driver === "codex") {
    return runCodexRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv });
  }
  if (runnerConfig.driver === "claude_cli") {
    return runClaudeCliRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv });
  }
  throw new Error(`unsupported real runner driver ${runnerConfig.driver}`);
}

async function runHolonRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv, manifest }) {
  const homeDir = path.join(taskDir, "holon-home");
  const agentId = manifest.task_id;
  const env = buildHolonBenchmarkEnv(runnerEnv, runnerConfig, manifest);
  await ensureHolonBuilt(worktreePath);
  const holonBinary = resolveHolonBinary(worktreePath);
  const args = [
    "run",
    prompt,
    "--agent",
    agentId,
    "--create-agent",
    "--json",
    "--trust",
    "trusted-operator",
    "--home",
    homeDir,
    "--workspace-root",
    worktreePath,
    "--cwd",
    worktreePath
  ];

  const startedAt = Date.now();
  const run = await runCommand(
    holonBinary,
    args,
    worktreePath,
    env,
    true,
    false,
    manifest.budget.max_minutes * 60 * 1000
  );
  if (run.stderr) {
    await writeFile(path.join(taskDir, "runner.log"), run.stderr, "utf8");
  }
  if (!run.stdout.trim()) {
    throw new Error("holon run produced empty stdout");
  }
  const parsed = JSON.parse(run.stdout);
  await writeJson(path.join(taskDir, "holon-run.json"), parsed);
  const agentDir = path.join(homeDir, "agents", agentId);
  const briefs = await readAgentJsonlArtifact(agentDir, "briefs.jsonl", taskDir);
  await copyAgentJsonlArtifact(agentDir, taskDir, "briefs.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "events.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "tools.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "transcript.jsonl");

  const toolExecutions = await readAgentJsonlArtifact(agentDir, "tools.jsonl", taskDir);
  const toolMetrics = summarizeHolonToolExecutions(toolExecutions);
  const events = await readAgentJsonlArtifact(agentDir, "events.jsonl", taskDir);
  const transcript = await readAgentJsonlArtifact(agentDir, "transcript.jsonl", taskDir);
  const tokenOptimization = summarizeHolonTokenOptimization(tokenOptimizationEvents(events, transcript), toolExecutions, {
    modelRef: runnerConfig.model_ref
  });
  await writeJson(path.join(taskDir, "token-optimization.json"), tokenOptimization);
  const finalMessage = selectHolonFinalMessage(parsed, briefs);
  return {
    finalMessage,
    durationMs: Date.now() - startedAt,
    toolCalls: parsed.tool_calls ?? toolExecutions.length,
    shellCommands: parsed.shell_commands ?? 0,
    execCommandItems: parsed.exec_command_items ?? 0,
    batchedExecCommandItems: parsed.batched_exec_command_items ?? 0,
    timedOut: parsed.final_status === "max_turns_exceeded",
    errorKind: parsed.final_status !== "completed" ? parsed.final_status : null,
    inputTokens: parsed.input_tokens ?? 0,
    outputTokens: parsed.output_tokens ?? 0,
    modelRounds: parsed.model_rounds ?? 0,
    runnerTurns: parsed.model_rounds ?? 0,
    runnerTurnsKind: "provider_rounds",
    tokenOptimization,
    ...toolMetrics
  };
}

async function runCodexRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv }) {
  const codexRuntime = await prepareCodexRuntimeHome(taskDir, runnerEnv);
  const env = {
    ...runnerEnv,
    ...(runnerConfig.env ?? {})
  };
  const finalMessagePath = path.join(taskDir, "codex-last-message.txt");
  const args = [
    "exec",
    "--json",
    "--output-last-message",
    finalMessagePath,
    "--dangerously-bypass-approvals-and-sandbox",
    "--color",
    "never",
    "-C",
    worktreePath
  ];
  if (runnerConfig.model) {
    args.push("--model", runnerConfig.model);
  }
  args.push(prompt);

  const startedAt = Date.now();
  try {
    const run = await runCommand(
      "codex",
      args,
      worktreePath,
      env,
      true,
      false,
      60 * 60 * 1000
    );
    await writeFile(path.join(taskDir, "codex-events.jsonl"), run.stdout || "", "utf8");
    await writeFile(path.join(taskDir, "runner.log"), run.stderr || "", "utf8");
    const parsed = parseCodexJsonl(run.stdout || "");
    const finalMessage =
      (await readFile(finalMessagePath, "utf8").catch(() => "")) || parsed.finalMessage || "";

    return {
      finalMessage,
      durationMs: Date.now() - startedAt,
      toolCalls: parsed.toolCalls,
      shellCommands: parsed.shellCommands,
      timedOut: run.timedOut,
      errorKind: run.exitCode === 0 ? null : parsed.errorKind ?? `process_exit_${run.exitCode}`,
      inputTokens: parsed.inputTokens,
      outputTokens: parsed.outputTokens,
      modelRounds: parsed.codexCliTurns,
      runnerTurns: parsed.codexCliTurns,
      runnerTurnsKind: "codex_cli_turns",
      totalToolLatencyMs: 0,
      perToolLatencyMs: {}
    };
  } finally {
    await captureCodexSessionArtifacts(taskDir, codexRuntime);
  }
}

async function runClaudeCliRealTask({ runnerConfig, prompt, worktreePath, taskDir, runnerEnv }) {
  const env = {
    ...runnerEnv,
    ...(runnerConfig.env ?? {})
  };
  const benchmarkSystemAppend =
    "Stay within the current workspace. Prefer direct file tools over shell when possible. If you change code, verify the result before finishing.";
  const args = [
    "-p",
    "--verbose",
    "--output-format",
    "stream-json",
    "--no-session-persistence",
    "--permission-mode",
    "bypassPermissions",
    "--dangerously-skip-permissions",
    "--add-dir",
    worktreePath,
    "--append-system-prompt",
    benchmarkSystemAppend
  ];
  if (runnerConfig.model) {
    args.push("--model", runnerConfig.model);
  }
  args.push(prompt);

  const startedAt = Date.now();
  const run = await runCommand(
    "claude",
    args,
    worktreePath,
    env,
    true,
    false,
    60 * 60 * 1000
  );
  await writeFile(path.join(taskDir, "claude-events.jsonl"), run.stdout || "", "utf8");
  await writeFile(path.join(taskDir, "runner.log"), run.stderr || "", "utf8");
  const parsed = parseClaudeCliJsonl(run.stdout || "", worktreePath);

  return {
    finalMessage: parsed.finalMessage,
    durationMs: Date.now() - startedAt,
    toolCalls: parsed.toolCalls,
    shellCommands: parsed.shellCommands,
    timedOut: run.timedOut,
    errorKind: run.exitCode === 0 ? null : parsed.errorKind ?? `process_exit_${run.exitCode}`,
    inputTokens: parsed.inputTokens,
    outputTokens: parsed.outputTokens,
    modelRounds: parsed.claudeCliTurns,
    runnerTurns: parsed.claudeCliTurns,
    runnerTurnsKind: "claude_cli_turns",
    readOps: parsed.readOps,
    searchOps: parsed.searchOps,
    listOps: parsed.listOps,
    execOps: parsed.execOps,
    createTaskOps: parsed.createTaskOps,
    sleepOps: parsed.sleepOps,
    todoWriteOps: parsed.todoWriteOps,
    taskListOps: parsed.taskListOps,
    taskGetOps: parsed.taskGetOps,
    taskStopOps: parsed.taskStopOps,
    uniqueFilesRead: parsed.uniqueFilesRead,
    uniqueSearchQueries: parsed.uniqueSearchQueries,
    bytesRead: parsed.bytesRead,
    searchToReadChains: parsed.searchToReadChains,
    totalToolLatencyMs: 0,
    perToolLatencyMs: {}
  };
}

export function parseCodexJsonl(output) {
  const events = [];
  for (const rawLine of output.split("\n")) {
    const line = rawLine.trim();
    if (!line) {
      continue;
    }
    try {
      events.push(JSON.parse(line));
    } catch {
      continue;
    }
  }

  let finalMessage = "";
  let toolCalls = 0;
  let shellCommands = 0;
  let inputTokens = 0;
  let outputTokens = 0;
  let codexCliTurns = 0;
  let errorKind = null;

  for (const event of events) {
    if (event.type === "item.completed" || event.type === "item.started") {
      const type = event.item?.type;
      if (type === "agent_message" && event.item?.text) {
        finalMessage = event.item.text;
      }
      if (["command_execution", "file_change", "mcp_tool_call", "collab_tool_call", "web_search"].includes(type)) {
        if (event.type === "item.started" || type === "file_change") {
          toolCalls += 1;
        }
      }
      if (type === "command_execution") {
        shellCommands += event.type === "item.started" ? 1 : 0;
      }
    } else if (event.type === "turn.completed") {
      codexCliTurns += 1;
      inputTokens += Number(event.usage?.input_tokens ?? 0);
      outputTokens += Number(event.usage?.output_tokens ?? 0);
    } else if (event.type === "turn.failed") {
      errorKind = event.error?.message ?? "turn_failed";
    } else if (event.type === "error") {
      errorKind = event.message ?? "stream_error";
    }
  }

  return {
    finalMessage,
    toolCalls,
    shellCommands,
    inputTokens,
    outputTokens,
    codexCliTurns,
    errorKind
  };
}

export function parseClaudeCliJsonl(output, workspaceDir = "") {
  const events = [];
  for (const rawLine of output.split("\n")) {
    const line = rawLine.trim();
    if (!line) {
      continue;
    }
    try {
      events.push(JSON.parse(line));
    } catch {
      continue;
    }
  }

  const toolUseById = new Map();
  const uniqueFiles = new Set();
  const uniqueQueries = new Set();
  let finalMessage = "";
  let latestAssistantText = "";
  let toolCalls = 0;
  let shellCommands = 0;
  let inputTokens = 0;
  let outputTokens = 0;
  let claudeCliTurns = 0;
  let errorKind = null;
  let readOps = 0;
  let searchOps = 0;
  let listOps = 0;
  let execOps = 0;
  let createTaskOps = 0;
  let sleepOps = 0;
  let todoWriteOps = 0;
  let taskListOps = 0;
  let taskGetOps = 0;
  let taskStopOps = 0;
  let bytesRead = 0;
  let searchToReadChains = 0;
  let recentDiscovery = false;

  for (const event of events) {
    if (event.type === "assistant") {
      for (const block of event.message?.content ?? []) {
        if (block.type === "text" && block.text) {
          latestAssistantText = block.text;
        }
        if (block.type !== "tool_use") {
          continue;
        }
        toolUseById.set(block.id, { name: block.name, input: block.input ?? {} });
        toolCalls += 1;
        if (block.name === "Read") {
          readOps += 1;
          if (recentDiscovery) {
            searchToReadChains += 1;
          }
          recentDiscovery = false;
          if (block.input?.file_path) {
            uniqueFiles.add(relativizeIfPossible(block.input.file_path, workspaceDir));
          }
        } else if (block.name === "Grep") {
          searchOps += 1;
          recentDiscovery = true;
          if (block.input?.pattern) {
            uniqueQueries.add(String(block.input.pattern));
          }
        } else if (block.name === "Glob") {
          listOps += 1;
          recentDiscovery = true;
          if (block.input?.pattern) {
            uniqueQueries.add(String(block.input.pattern));
          }
        } else if (block.name === "Bash") {
          execOps += 1;
          shellCommands += 1;
          recentDiscovery = false;
        } else if (block.name === "TodoWrite") {
          todoWriteOps += 1;
          recentDiscovery = false;
        } else if (block.name === "Task") {
          createTaskOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskOutput") {
          taskGetOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskList") {
          taskListOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskStop") {
          taskStopOps += 1;
          recentDiscovery = false;
        } else {
          recentDiscovery = false;
        }
      }
      continue;
    }

    if (event.type === "user") {
      for (const block of event.message?.content ?? []) {
        if (block.type !== "tool_result" || !block.tool_use_id) {
          continue;
        }
        const toolUse = toolUseById.get(block.tool_use_id);
        if (toolUse?.name === "Read") {
          if (typeof block.content === "string") {
            bytesRead += Buffer.byteLength(block.content, "utf8");
          } else if (Array.isArray(block.content)) {
            bytesRead += Buffer.byteLength(JSON.stringify(block.content), "utf8");
          }
        }
      }
      continue;
    }

    if (event.type === "result") {
      claudeCliTurns += Number(event.num_turns ?? 0) || 1;
      inputTokens += Number(event.usage?.input_tokens ?? 0);
      outputTokens += Number(event.usage?.output_tokens ?? 0);
      if (event.subtype === "success") {
        finalMessage = event.result || latestAssistantText || "";
      } else {
        finalMessage = event.result || latestAssistantText || "";
        errorKind = event.subtype || "result_error";
      }
      continue;
    }

    if (event.type === "error") {
      errorKind = event.error?.message ?? event.message ?? "stream_error";
    }
  }

  return {
    finalMessage,
    toolCalls,
    shellCommands,
    inputTokens,
    outputTokens,
    claudeCliTurns,
    errorKind,
    readOps,
    searchOps,
    listOps,
    execOps,
    createTaskOps,
    sleepOps,
    todoWriteOps,
    taskListOps,
    taskGetOps,
    taskStopOps,
    uniqueFilesRead: uniqueFiles.size,
    uniqueSearchQueries: uniqueQueries.size,
    bytesRead,
    searchToReadChains
  };
}

function parseBooleanEnv(value, defaultValue) {
  if (value === undefined || value === null || value === "") {
    return defaultValue;
  }
  const normalized = String(value).trim().toLowerCase();
  if (["1", "true", "yes", "on"].includes(normalized)) {
    return true;
  }
  if (["0", "false", "no", "off"].includes(normalized)) {
    return false;
  }
  return defaultValue;
}

function resolveCodexBenchmarkSettings(runnerEnv) {
  return {
    projectDocDisabled: parseBooleanEnv(runnerEnv.CODEX_DISABLE_PROJECT_DOC, false),
    bundledSkillsDisabled: parseBooleanEnv(
      runnerEnv.CODEX_BENCHMARK_DISABLE_BUNDLED_SKILLS,
      false
    )
  };
}

export function codexBenchmarkConfigToml(settings = {}) {
  const projectDocDisabled = settings.projectDocDisabled ?? false;
  const bundledSkillsDisabled = settings.bundledSkillsDisabled ?? false;
  const lines = [];
  if (projectDocDisabled) {
    lines.push("project_doc_max_bytes = 0", "");
  }
  if (bundledSkillsDisabled) {
    lines.push("[skills.bundled]", "enabled = false", "");
  }
  return lines.join("\n");
}

async function prepareCodexRuntimeHome(taskDir, runnerEnv) {
  const sourceHome = runnerEnv.CODEX_HOME
    ? path.resolve(runnerEnv.CODEX_HOME)
    : path.join(os.homedir(), ".codex");
  const settings = resolveCodexBenchmarkSettings(runnerEnv);
  return {
    runtimeHome: sourceHome,
    runtimeUserHome: process.env.HOME ?? os.homedir(),
    sourceHomeKind: runnerEnv.CODEX_HOME ? "custom_env" : "default",
    seededEntries: [],
    projectDocDisabled: settings.projectDocDisabled,
    bundledSkillsDisabled: settings.bundledSkillsDisabled
  };
}

async function captureCodexSessionArtifacts(taskDir, codexRuntime) {
  await writeJson(path.join(taskDir, "codex-session.json"), {
    source_home_kind: codexRuntime.sourceHomeKind,
    runtime_home_archived: false,
    runtime_user_home_archived: false,
    auth_archived: false,
    project_doc_disabled: codexRuntime.projectDocDisabled,
    bundled_skills_disabled: codexRuntime.bundledSkillsDisabled,
    seeded_entries: codexRuntime.seededEntries,
    captured_entries: []
  });
}

export function collectChangedFilesFromGitOutputs(nameOnlyOutput, statusOutput) {
  const changed = new Set(
    String(nameOnlyOutput || "")
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
  );

  for (const rawLine of String(statusOutput || "").split("\n")) {
    const line = rawLine.trimEnd();
    if (!line) {
      continue;
    }
    if (line.startsWith("?? ")) {
      changed.add(line.slice(3).trim());
      continue;
    }
    const payload = line.slice(3).trim();
    if (!payload) {
      continue;
    }
    const renamed = payload.split(" -> ").at(-1)?.trim();
    if (renamed) {
      changed.add(renamed);
    }
  }

  return [...changed].sort();
}

async function diffAgainstBase(repoPath, worktreePath, baseSha, taskDir) {
  const nameOnly = await runCommand(
    "git",
    ["-C", worktreePath, "diff", "--name-only", baseSha],
    repoPath,
    process.env,
    true
  );
  const diff = await runCommand(
    "git",
    ["-C", worktreePath, "diff", baseSha],
    repoPath,
    process.env,
    true
  );
  const status = await runCommand(
    "git",
    ["-C", worktreePath, "status", "--porcelain", "--untracked-files=all"],
    repoPath,
    process.env,
    true
  );
  await writeFile(path.join(taskDir, "git.diff"), diff.stdout || "", "utf8");
  return collectChangedFilesFromGitOutputs(nameOnly.stdout || "", status.stdout || "");
}

export function detectScopeViolation(changedFiles, evaluation) {
  const allowed = evaluation.allowed_paths ?? [];
  const forbidden = evaluation.forbidden_paths ?? [];
  return changedFiles.some((file) => {
    const forbiddenHit = forbidden.some((prefix) => pathMatches(file, prefix));
    if (forbiddenHit) {
      return true;
    }
    if (allowed.length === 0) {
      return false;
    }
    return !allowed.some((prefix) => pathMatches(file, prefix));
  });
}

function pathMatches(file, prefix) {
  const normalizedFile = file.replaceAll("\\", "/");
  const normalizedPrefix = String(prefix).replaceAll("\\", "/").replace(/\/+$/, "");
  return normalizedFile === normalizedPrefix || normalizedFile.startsWith(`${normalizedPrefix}/`);
}

export function evaluateRealTaskSuccess({
  verifyExitCode,
  runnerResult,
  changedFiles = [],
  scopeViolation,
  scopePolicy = "soft",
  expectedOutcome = "change_required"
}) {
  const verifyOk = verifyExitCode === 0;
  const runnerOk = !runnerResult.errorKind && !runnerResult.timedOut;
  const hasChanges = changedFiles.length > 0;
  const scopeOk = scopePolicy === "hard" ? !scopeViolation : true;
  const outcomeOk =
    expectedOutcome === "change_required"
      ? hasChanges
      : expectedOutcome === "no_change_expected"
        ? !hasChanges
        : true;
  return verifyOk && runnerOk && scopeOk && outcomeOk;
}

function latestBriefOfKind(briefs, kind) {
  return [...briefs]
    .filter((brief) => brief?.kind === kind && typeof brief?.text === "string" && brief.text.trim())
    .sort((left, right) => String(left.created_at).localeCompare(String(right.created_at)))
    .at(-1);
}

function looksLikeProgressOnlyText(text) {
  const normalized = String(text ?? "").trim().toLowerCase();
  if (!normalized) {
    return true;
  }
  return [
    "delta since base checkpoint",
    "since base checkpoint",
    "new confirmed facts since the base checkpoint",
    "next bounded action",
    "queued work:",
    "progress update",
    "i'll quickly inspect",
    "i hit mixed schema"
  ].some((marker) => normalized.includes(marker));
}

export function selectHolonFinalMessage(runResult, briefs = []) {
  const parsedText = typeof runResult?.final_text === "string" ? runResult.final_text.trim() : "";
  const latestFailure = latestBriefOfKind(briefs, "failure")?.text?.trim() ?? "";
  const latestResult = latestBriefOfKind(briefs, "result")?.text?.trim() ?? "";

  if (runResult?.final_status && runResult.final_status !== "completed" && latestFailure) {
    return latestFailure;
  }
  if (parsedText && !looksLikeProgressOnlyText(parsedText)) {
    return parsedText;
  }
  if (
    runResult?.final_status === "completed" &&
    latestResult &&
    !looksLikeProgressOnlyText(latestResult)
  ) {
    return latestResult;
  }
  return parsedText || latestFailure || latestResult || "";
}

async function commitBenchmarkChanges({ worktreePath, branchName, taskId, runnerId }) {
  const status = await runCommand(
    "git",
    ["-C", worktreePath, "status", "--porcelain"],
    worktreePath,
    process.env,
    true
  );
  if (!status.stdout.trim()) {
    return { status: "no_changes" };
  }
  await runCommand("git", ["-C", worktreePath, "add", "-A"], worktreePath, process.env);
  await runCommand(
    "git",
    ["-C", worktreePath, "commit", "-m", `bench(${runnerId}): ${taskId}`],
    worktreePath,
    process.env
  );
  const commitSha = (
    await runCommand("git", ["-C", worktreePath, "rev-parse", "HEAD"], worktreePath, process.env)
  ).stdout.trim();
  return { status: "committed", branch: branchName, commit_sha: commitSha };
}

async function maybeCreateBenchmarkPr({
  repoPath,
  worktreePath,
  manifest,
  runnerId,
  branchName,
  suiteConfig,
  prompt,
  commitInfo
}) {
  if (commitInfo.status !== "committed") {
    return { status: "skipped_no_changes" };
  }
  if (!suiteConfig.pr?.push_branch) {
    return { status: "local_only", branch: branchName };
  }

  await runCommand(
    "git",
    ["-C", worktreePath, "push", "--force-with-lease", "origin", `HEAD:${branchName}`],
    repoPath,
    process.env
  );

  if (!suiteConfig.pr?.submit_pr) {
    return { status: "pushed_only", branch: branchName };
  }

  const prTitle = prTitleForTask(manifest.issue.number, manifest.issue.title, runnerId);
  const labels = benchmarkLabelsForTask(manifest.issue.number, runnerId);
  for (const label of labels) {
    await ensureGithubLabel(repoPath, manifest.repo.name, label);
  }

  const prBody = await renderPrBodyTemplate({
    task_id: manifest.task_id,
    runner: runnerId,
    base_sha: manifest.base.sha,
    issue_number: manifest.issue.number,
    benchmark_mode: manifest.benchmark.mode,
    prompt: prompt.trim()
  });
  const bodyPath = path.join(resultsRoot, ".tmp", `${manifest.task_id}-${runnerId}-pr-body.md`);
  await mkdir(path.dirname(bodyPath), { recursive: true });
  await writeFile(bodyPath, prBody, "utf8");

  const existing = await runCommand(
    "gh",
    [
      "pr",
      "list",
      "--repo",
      manifest.repo.name,
      "--head",
      branchName,
      "--state",
      "all",
      "--json",
      "number,url,state"
    ],
    repoPath,
    process.env,
    true
  );
  const matches = JSON.parse(existing.stdout || "[]");
  if (matches.length > 0) {
    const current = matches[0];
    await runCommand(
      "gh",
      [
        "pr",
        "edit",
        String(current.number),
        "--repo",
        manifest.repo.name,
        "--title",
        prTitle,
        "--body-file",
        bodyPath
      ],
      repoPath,
      process.env
    );
    await runCommand(
      "gh",
      ["pr", "edit", String(current.number), "--repo", manifest.repo.name, "--add-label", labels.join(",")],
      repoPath,
      process.env,
      true
    );
    if (!suiteConfig.pr?.draft_pr) {
      await runCommand(
        "gh",
        ["pr", "ready", String(current.number), "--repo", manifest.repo.name],
        repoPath,
        process.env,
        true
      );
    }
    return { status: "updated", number: current.number, url: current.url, state: current.state };
  }

  const createArgs = [
    "pr",
    "create",
    "--repo",
    manifest.repo.name,
    "--base",
    manifest.base.branch,
    "--head",
    branchName,
    "--title",
    prTitle,
    "--body-file",
    bodyPath,
    "--label",
    labels[0],
    "--label",
    labels[1],
    "--label",
    labels[2]
  ];
  if (suiteConfig.pr?.draft_pr) {
    createArgs.push("--draft");
  }

  const created = await runCommand("gh", createArgs, repoPath, process.env);
  const prUrl = (created.stdout || "").trim().split("\n").find(Boolean) ?? null;
  const numberMatch = prUrl?.match(/\/pull\/(\d+)/);
  return {
    status: "created",
    url: prUrl,
    number: numberMatch ? Number(numberMatch[1]) : null,
    state: "OPEN"
  };
}

async function renderPrBodyTemplate(values) {
  let template = await readFile(prBodyTemplatePath, "utf8");
  for (const [key, value] of Object.entries(values)) {
    template = template.replaceAll(`{{${key}}}`, String(value));
  }
  return template;
}

async function ensureGithubLabel(repoPath, repoName, label) {
  const labelSpec = {
    bench: { color: "BFDADC", description: "Benchmark-generated pull requests" },
    [`bench:task-${label.split("bench:task-")[1]}`]: {
      color: "D4C5F9",
      description: "Benchmark task label"
    },
    [`runner:${label.split("runner:")[1]}`]: {
      color: "C2E0C6",
      description: "Benchmark runner label"
    }
  }[label] ?? { color: "EDEDED", description: "Benchmark metadata label" };

  await runCommand(
    "gh",
    [
      "label",
      "create",
      label,
      "--repo",
      repoName,
      "--color",
      labelSpec.color,
      "--description",
      labelSpec.description,
      "--force"
    ],
    repoPath,
    process.env,
    true
  );
}

function timestampLabel() {
  return new Date().toISOString().replace(/[:.]/g, "-");
}

async function runHolonTask({ task, taskDir, workspaceDir, runnerEnv }) {
  const homeDir = path.join(taskDir, "holon-home");
  const agentId = task.name;
  const env = {
    ...runnerEnv,
    HOLON_HOME: homeDir,
    HOLON_WORKSPACE_DIR: workspaceDir
  };
  const holonBinary = resolveHolonBinary();
  const agentDir = path.join(homeDir, "agents", agentId);

  const startedAt = Date.now();
  const turns = task.turns ?? [{ prompt: task.prompt }];
  const dumpArgs = ["dump-prompt", turns[0].prompt, "--agent", agentId, "--trust"];
  dumpArgs.push(task.mode === "controlled" ? "trusted-integration" : "trusted-operator");
  const dump = await runCommand(holonBinary, dumpArgs, repoRoot, env, true);
  await writeFile(
    path.join(taskDir, "prompt.txt"),
    dump.stdout || turns.map((turn, index) => `Turn ${index + 1}: ${turn.prompt}`).join("\n\n"),
    "utf8"
  );

  let lastRun = null;
  let timedOut = false;
  for (const [index, turn] of turns.entries()) {
    const args = ["run", turn.prompt, "--agent", agentId, "--json", "--trust"];
    args.push(task.mode === "controlled" ? "trusted-integration" : "trusted-operator");
    const run = await runCommand(
      holonBinary,
      args,
      repoRoot,
      env,
      true,
      false,
      task.timeout_seconds * 1000
    );
    if (run.stderr) {
      await writeFile(path.join(taskDir, `run.stderr.${index + 1}.log`), run.stderr, "utf8");
    }
    if (run.timedOut) {
      timedOut = true;
      lastRun = holonRunFailure(agentId, "timeout", run);
      break;
    }
    if (run.exitCode !== 0) {
      lastRun = holonRunFailure(agentId, "process_exit", run);
      break;
    }
    if (!run.stdout.trim()) {
      lastRun = holonRunFailure(agentId, "empty_stdout", run);
      break;
    }
    try {
      lastRun = JSON.parse(run.stdout);
    } catch (error) {
      lastRun = holonRunFailure(agentId, "invalid_json", run, {
        parse_error: String(error)
      });
      break;
    }
    if (!lastRun?.final_status) {
      lastRun = holonRunFailure(agentId, "missing_final_status", run, {
        parsed_stdout: lastRun
      });
      break;
    }
    await writeJson(path.join(taskDir, "run.json"), lastRun);
  }

  if (lastRun && lastRun.runner_error_kind) {
    await writeJson(path.join(taskDir, "run.json"), lastRun);
  }

  const agentState =
    (await readJsonIfExists(path.join(agentDir, "agent.json"))) ??
    (await readJsonIfExists(path.join(agentDir, ".holon", "state", "agent.json")));
  const briefs = await readAgentJsonlArtifact(agentDir, "briefs.jsonl", taskDir);
  const events = await readAgentJsonlArtifact(agentDir, "events.jsonl", taskDir);
  const tasks = await readAgentJsonlArtifact(agentDir, "tasks.jsonl", taskDir);

  await writeJson(path.join(taskDir, "status.json"), { agent: agentState });
  await writeJson(path.join(taskDir, "briefs.json"), briefs);
  await writeJson(path.join(taskDir, "events.json"), events);
  await writeJson(path.join(taskDir, "tasks.json"), lastRun?.tasks ?? tasks);

  await copyAgentJsonlArtifact(agentDir, taskDir, "events.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "briefs.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "tools.jsonl");
  await copyAgentJsonlArtifact(agentDir, taskDir, "transcript.jsonl");

  const finalMessage = selectHolonFinalMessage(lastRun, briefs);
  await writeFile(path.join(taskDir, "final_message.md"), `${finalMessage}\n`, "utf8");

  const toolExecutions = await readAgentJsonlArtifact(agentDir, "tools.jsonl", taskDir);
  const toolMetrics = summarizeHolonToolExecutions(toolExecutions);
  const transcript = await readAgentJsonlArtifact(agentDir, "transcript.jsonl", taskDir);
  const tokenOptimization = summarizeHolonTokenOptimization(tokenOptimizationEvents(events, transcript), toolExecutions, {
    modelRef: env.HOLON_MODEL
  });
  await writeJson(path.join(taskDir, "token-optimization.json"), tokenOptimization);
  const failureKind = computeFailureKind(lastRun);

  return {
    finalMessage,
    durationMs: Date.now() - startedAt,
    toolCalls: toolExecutions.length,
    shellCommands: toolExecutions.filter((entry) => entry.tool_name === "exec_command").length,
    timedOut,
    ...failureKind,
    errorKind:
      timedOut
        ? "timeout"
        : failureKind.runner_error_kind ??
          lastRun?.runner_error_kind ??
          (lastRun && lastRun.final_status !== "completed" ? lastRun.final_status : null),
    inputTokens: lastRun?.input_tokens ?? 0,
    outputTokens: lastRun?.output_tokens ?? 0,
    modelRounds: lastRun?.model_rounds ?? 0,
    tokenOptimization,
    ...toolMetrics
  };
}

function computeFailureKind(lastRun) {
  const artifact = lastRun?.failure_artifact;
  if (!artifact?.kind) {
    return {};
  }
  return {
    runner_error_kind:
      artifact.category ? `${artifact.category}:${artifact.kind}` : artifact.kind
  };
}

function holonRunFailure(agentId, kind, run, extra = {}) {
  return {
    agent_id: agentId,
    final_status: "runtime_error",
    final_text: "",
    tasks: [],
    message_count: 0,
    changed_files: [],
    input_tokens: 0,
    output_tokens: 0,
    model_rounds: 0,
    tool_calls: 0,
    shell_commands: 0,
    exec_command_items: 0,
    batched_exec_command_items: 0,
    runner_error_kind: kind,
    exit_code: run.exitCode ?? 1,
    stderr: run.stderr ?? "",
    stdout: run.stdout ?? "",
    ...extra
  };
}

async function runClaudeSdkTask({ task, taskDir, workspaceDir, runnerEnv }) {
  if (task.turns && task.turns.length > 1) {
    return runClaudeSdkSessionTask({ task, taskDir, workspaceDir, runnerEnv });
  }
  const startedAt = Date.now();
  const tools =
    task.tool_profile === "read_only"
      ? ["Read", "Glob", "Grep"]
      : ["Read", "Write", "Edit", "Glob", "Grep", "Bash"];
  const transcript = [];
  let finalResult = "";
  let errorKind = null;
  let timedOut = false;
  let timeoutHandle = null;

  const benchmarkSystemAppend =
    "Stay within the current workspace. Prefer direct file tools over shell when possible. If you change code, verify the result before finishing.";
  const session = query({
    prompt: task.prompt,
    options: {
      cwd: workspaceDir,
      maxTurns: sdkMaxTurnsForTask(task),
      tools,
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      systemPrompt: {
        type: "preset",
        preset: "claude_code",
        append: benchmarkSystemAppend
      },
      model: runnerEnv.ANTHROPIC_MODEL || runnerEnv.HOLON_MODEL || "claude-sonnet-4-6",
      env: runnerEnv
    }
  });

  try {
    timeoutHandle = setTimeout(() => {
      timedOut = true;
      session.close();
    }, task.timeout_seconds * 1000);

    for await (const message of session) {
      transcript.push(message);
      if (message.type === "result") {
        if (message.subtype === "success") {
          finalResult = message.result;
        } else {
          finalResult = message.errors?.join("\n") ?? "";
          errorKind = message.subtype;
        }
      }
    }
  } finally {
    if (timeoutHandle) {
      clearTimeout(timeoutHandle);
    }
    session.close();
  }

  await writeJsonl(
    path.join(taskDir, "transcript.jsonl"),
    transcript.map((message) => JSON.stringify(message))
  );
  await writeFile(path.join(taskDir, "final_message.md"), `${finalResult}\n`, "utf8");
  await writeFile(path.join(taskDir, "prompt.txt"), `${task.prompt}\n`, "utf8");

  const toolProgressIds = new Set();
  const toolUseIds = new Set();
  let shellCommands = 0;
  let totalInputTokens = 0;
  let totalOutputTokens = 0;
  let modelRounds = 0;
  
  for (const message of transcript) {
    if (message.type === "tool_progress") {
      toolProgressIds.add(message.tool_use_id);
      if (message.tool_name === "Bash") {
        shellCommands += 1;
      }
    }
    if (message.type === "assistant") {
      for (const block of message.message?.content ?? []) {
        if (block.type === "tool_use") {
          toolUseIds.add(block.id);
          if (block.name === "Bash") {
            shellCommands += 1;
          }
        }
      }
    }
    if (message.type === "result" && message.usage) {
      modelRounds += 1;
      totalInputTokens += message.usage.input_tokens ?? 0;
      totalOutputTokens += message.usage.output_tokens ?? 0;
    }
  }

  const toolMetrics = summarizeSdkTranscript(transcript, workspaceDir);

  return {
    finalMessage: finalResult,
    durationMs: Date.now() - startedAt,
    toolCalls: Math.max(toolProgressIds.size, toolUseIds.size),
    shellCommands,
    timedOut,
    errorKind,
    inputTokens: totalInputTokens,
    outputTokens: totalOutputTokens,
    modelRounds,
    ...toolMetrics
  };
}

function sdkMaxTurnsForTask(task) {
  if (typeof task.sdk_max_turns === "number" && Number.isFinite(task.sdk_max_turns)) {
    return task.sdk_max_turns;
  }

  if (task.tool_profile === "read_only") {
    return 24;
  }

  return 12;
}

async function runClaudeSdkSessionTask({ task, taskDir, workspaceDir, runnerEnv }) {
  const startedAt = Date.now();
  const tools =
    task.tool_profile === "read_only"
      ? ["Read", "Glob", "Grep"]
      : ["Read", "Write", "Edit", "Glob", "Grep", "Bash"];
  const transcript = [];
  const benchmarkSystemAppend =
    "Stay within the current workspace. Prefer direct file tools over shell when possible. If you change code, verify the result before finishing.";
  const session = unstable_v2_createSession({
    cwd: workspaceDir,
    model: runnerEnv.ANTHROPIC_MODEL || runnerEnv.HOLON_MODEL || "claude-sonnet-4-6",
    tools,
    permissionMode: "bypassPermissions",
    allowDangerouslySkipPermissions: true,
    systemPrompt: benchmarkSystemAppend,
    env: runnerEnv
  });

  let finalResult = "";
  let errorKind = null;
  let timedOut = false;

  try {
    for (const turn of task.turns) {
      const turnResult = await collectSdkTurn(session, transcript, turn.prompt, task.timeout_seconds)
        .catch((error) => {
        errorKind = errorKind ?? "session_error";
        if (String(error).includes("timed out")) {
          timedOut = true;
        }
        return null;
      });

      if (timedOut) {
        break;
      }

      if (turnResult?.subtype === "success") {
        finalResult = turnResult.result;
      } else if (turnResult?.subtype) {
        finalResult = turnResult.errors?.join("\n") ?? "";
        errorKind = turnResult.subtype;
      }
    }
  } finally {
    session.close();
  }

  await writeJsonl(
    path.join(taskDir, "transcript.jsonl"),
    transcript.map((message) => JSON.stringify(message))
  );
  await writeFile(path.join(taskDir, "final_message.md"), `${finalResult}\n`, "utf8");
  await writeFile(
    path.join(taskDir, "prompt.txt"),
    task.turns.map((turn, index) => `Turn ${index + 1}: ${turn.prompt}`).join("\n\n"),
    "utf8"
  );

  const toolProgressIds = new Set();
  const toolUseIds = new Set();
  let shellCommands = 0;
  let totalInputTokens = 0;
  let totalOutputTokens = 0;
  let modelRounds = 0;
  
  for (const message of transcript) {
    if (message.type === "tool_progress") {
      toolProgressIds.add(message.tool_use_id);
      if (message.tool_name === "Bash") {
        shellCommands += 1;
      }
    }
    if (message.type === "assistant") {
      for (const block of message.message?.content ?? []) {
        if (block.type === "tool_use") {
          toolUseIds.add(block.id);
          if (block.name === "Bash") {
            shellCommands += 1;
          }
        }
      }
    }
    if (message.type === "result" && message.usage) {
      modelRounds += 1;
      totalInputTokens += message.usage.input_tokens ?? 0;
      totalOutputTokens += message.usage.output_tokens ?? 0;
    }
  }

  const toolMetrics = summarizeSdkTranscript(transcript, workspaceDir);

  return {
    finalMessage: finalResult,
    durationMs: Date.now() - startedAt,
    toolCalls: Math.max(toolProgressIds.size, toolUseIds.size),
    shellCommands,
    timedOut,
    errorKind,
    inputTokens: totalInputTokens,
    outputTokens: totalOutputTokens,
    modelRounds,
    ...toolMetrics
  };
}

async function collectSdkTurn(session, transcript, prompt, timeoutSeconds) {
  const iterator = session.stream();
  await session.send(prompt);
  const deadline = Date.now() + timeoutSeconds * 1000;
  for (;;) {
    const remainingMs = deadline - Date.now();
    if (remainingMs <= 0) {
      session.close();
      throw new Error(`session timed out after ${timeoutSeconds}s`);
    }
    const next = await Promise.race([
      iterator.next(),
      sleep(remainingMs).then(() => ({ timeout: true }))
    ]);
    if (next?.timeout) {
      session.close();
      throw new Error(`session timed out after ${timeoutSeconds}s`);
    }
    if (next.done) {
      return null;
    }
    transcript.push(next.value);
    if (next.value.type === "result") {
      return next.value;
    }
  }
}

async function prepareWorkspace(task, destination) {
  await mkdir(destination, { recursive: true });
  if (task.workspace.type === "fixture") {
    await cp(path.join(fixturesRoot, task.workspace.path), destination, { recursive: true });
    return;
  }

  if (task.workspace.type === "repo_snapshot") {
    await copyRepoSnapshot(task.workspace, destination);
    return;
  }

  throw new Error(`unsupported workspace type ${task.workspace.type}`);
}

async function ensureHolonBuilt(buildRoot = repoRoot) {
  if (process.env.HOLON_BENCHMARK_BINARY) {
    return;
  }
  await runCommand("cargo", ["build", "--release", "--quiet"], buildRoot, process.env);
}

function resolveHolonBinary(buildRoot = repoRoot) {
  const override = process.env.HOLON_BENCHMARK_BINARY;
  if (!override) {
    return path.join(buildRoot, "target", "release", "holon");
  }
  return path.isAbsolute(override) ? override : path.resolve(repoRoot, override);
}

async function loadTask(taskFile) {
  return readJson(path.join(tasksRoot, taskFile));
}

async function loadClaudeSettingsEnv() {
  const settingsPath = path.join(os.homedir(), ".claude", "settings.json");
  try {
    const parsed = JSON.parse(await readFile(settingsPath, "utf8"));
    return parsed.env ?? {};
  } catch {
    return {};
  }
}

async function runCommandList(commands, cwd, env) {
  let output = "";
  for (const command of commands) {
    const started = Date.now();
    try {
      const result = await runCommand("zsh", ["-lc", command], cwd, env, true);
      output += `$ ${command}\n${result.stdout}${result.stderr}\n[exit=${result.exitCode} duration_ms=${Date.now() - started}]\n\n`;
    } catch (error) {
      output += `$ ${command}\n${error.stdout ?? ""}${error.stderr ?? ""}\n[exit=${error.exitCode ?? 1} duration_ms=${Date.now() - started}]\n\n`;
    }
  }
  return output;
}

const DEFAULT_STALE_VERIFICATION_PATTERNS = [
  /\berror:\s+no test target named\b/i,
  /\berror:\s+no test named\b/i,
  /\bcould not find .*test target\b/i
];

function normalizeVerificationCommand(command) {
  if (typeof command === "string") {
    return {
      run: command,
      allow_failure: false,
      stale_if_output_matches: []
    };
  }
  return {
    run: command.run,
    allow_failure: Boolean(command.allow_failure),
    stale_if_output_matches: command.stale_if_output_matches ?? []
  };
}

function outputMatchesPattern(output, pattern) {
  try {
    return new RegExp(pattern, "i").test(output);
  } catch {
    return output.toLowerCase().includes(String(pattern).toLowerCase());
  }
}

export function classifyVerificationResult(command, exitCode, output) {
  const spec = normalizeVerificationCommand(command);
  if (exitCode === 0) {
    return { status: "passed", tolerated: true };
  }

  const isStale =
    DEFAULT_STALE_VERIFICATION_PATTERNS.some((pattern) => pattern.test(output)) ||
    spec.stale_if_output_matches.some((pattern) => outputMatchesPattern(output, pattern));
  if (isStale) {
    return { status: "stale", tolerated: true };
  }
  if (spec.allow_failure) {
    return { status: "allowed_failure", tolerated: true };
  }
  return { status: "failed", tolerated: false };
}

export async function runVerificationCommands(commands, cwd, env) {
  const sections = [];
  const commandResults = [];
  let exitCode = 0;

  for (const rawCommand of commands) {
    const command = normalizeVerificationCommand(rawCommand);
    const started = Date.now();
    let stdout = "";
    let stderr = "";
    let commandExitCode = 0;

    try {
      const result = await runCommand("zsh", ["-lc", command.run], cwd, env, true);
      stdout = result.stdout ?? "";
      stderr = result.stderr ?? "";
      commandExitCode = Number(result.exitCode ?? 0);
    } catch (error) {
      stdout = error.stdout ?? "";
      stderr = error.stderr ?? "";
      commandExitCode = Number(error.exitCode ?? 1);
    }

    const output = `${stdout}${stderr}`;
    const classification = classifyVerificationResult(command, commandExitCode, output);
    if (!classification.tolerated && exitCode === 0) {
      exitCode = commandExitCode || 1;
    }

    commandResults.push({
      command: command.run,
      exit_code: commandExitCode,
      status: classification.status,
      allow_failure: command.allow_failure,
      stale_if_output_matches: command.stale_if_output_matches
    });
    sections.push(
      `$ ${command.run}\n${stdout}${stderr}\n[exit=${commandExitCode} duration_ms=${Date.now() - started} verification_status=${classification.status}]\n`
    );
  }

  const status = commandResults.some((entry) => entry.status === "failed")
    ? "failed"
    : commandResults.some((entry) => entry.status === "stale")
      ? "stale"
      : commandResults.some((entry) => entry.status === "allowed_failure")
        ? "tolerated"
        : "passed";

  return {
    exitCode,
    success: exitCode === 0,
    status,
    commands: commandResults,
    log: sections.join("\n")
  };
}

function evaluateSuccess(task, finalMessage, verifyExitCode, filesChangedCount) {
  const requiredSubstrings = task.success_criteria?.required_substrings ?? [];
  const forbiddenSubstrings = task.success_criteria?.forbidden_substrings ?? [];
  const requiredRegexes = task.success_criteria?.required_regexes ?? [];
  const maxFilesChanged = task.success_criteria?.max_files_changed;
  const minFinalMessageLength = task.success_criteria?.min_final_message_length;
  const maxFinalMessageLength = task.success_criteria?.max_final_message_length;
  const normalized = finalMessage.toLowerCase();
  const requiredOk = requiredSubstrings.every((value) =>
    normalized.includes(String(value).toLowerCase())
  );
  const forbiddenOk = forbiddenSubstrings.every(
    (value) => !normalized.includes(String(value).toLowerCase())
  );
  const regexOk = requiredRegexes.every((pattern) => new RegExp(pattern, "i").test(finalMessage));
  const verifyExpected = task.success_criteria?.verify_exit_code;
  const verifyOk =
    verifyExpected === undefined ? true : verifyExitCode === Number(verifyExpected);
  const lengthOk =
    minFinalMessageLength === undefined
      ? true
      : finalMessage.trim().length >= Number(minFinalMessageLength);
  const maxLengthOk =
    maxFinalMessageLength === undefined
      ? true
      : finalMessage.trim().length <= Number(maxFinalMessageLength);
  const fileCountOk =
    maxFilesChanged === undefined ? true : filesChangedCount <= Number(maxFilesChanged);
  return requiredOk && forbiddenOk && regexOk && verifyOk && lengthOk && maxLengthOk && fileCountOk;
}

async function copyRepoSnapshot(workspace, destination) {
  const repoPath = path.resolve(repoRoot, workspace.path ?? ".");
  const includePaths = workspace.include_paths?.length > 0 ? workspace.include_paths : ["."];
  const tracked = await listTrackedFiles(repoPath, includePaths);
  for (const relativePath of tracked) {
    const source = path.join(repoPath, relativePath);
    const target = path.join(destination, relativePath);
    await mkdir(path.dirname(target), { recursive: true });
    await copyFile(source, target);
  }
}

function summarizeHolonToolExecutions(entries) {
  const uniqueFiles = new Set();
  const uniqueQueries = new Set();
  const perToolLatencyMs = {};
  let readOps = 0;
  let searchOps = 0;
  let listOps = 0;
  let execOps = 0;
  let batchedExecCommandItems = 0;
  let applyPatchOps = 0;
  let createTaskOps = 0;
  let sleepOps = 0;
  let todoWriteOps = 0;
  let taskListOps = 0;
  let taskGetOps = 0;
  let taskStopOps = 0;
  let bytesRead = 0;
  let searchToReadChains = 0;
  let totalToolLatencyMs = 0;
  let recentDiscovery = false;

  for (const entry of entries) {
    const name = entry.tool_name;
    const durationMs = Number(entry.duration_ms ?? 0);
    totalToolLatencyMs += durationMs;
    perToolLatencyMs[name] = (perToolLatencyMs[name] ?? 0) + durationMs;
    if (name === "Read") {
      readOps += 1;
      if (recentDiscovery) {
        searchToReadChains += 1;
      }
      recentDiscovery = false;
      if (entry.input?.file_path) {
        uniqueFiles.add(String(entry.input.file_path));
      }
      if (typeof entry.output?.content === "string") {
        bytesRead += Buffer.byteLength(entry.output.content, "utf8");
      }
    } else if (name === "Grep") {
      searchOps += 1;
      recentDiscovery = true;
      if (entry.input?.pattern) {
        uniqueQueries.add(String(entry.input.pattern));
      }
    } else if (name === "Glob") {
      listOps += 1;
      recentDiscovery = true;
    } else if (name === "ExecCommand" || name === "exec_command") {
      execOps += 1;
      recentDiscovery = false;
    } else if (name === "ExecCommandBatch") {
      const completedCount = Number(entry.output?.envelope?.result?.completed_count ?? 0);
      const failedCount = Number(entry.output?.envelope?.result?.failed_count ?? 0);
      const executedItemCount = completedCount + failedCount;
      execOps += executedItemCount;
      batchedExecCommandItems += executedItemCount;
      recentDiscovery = false;
    } else if (name === "ApplyPatch") {
      applyPatchOps += 1;
      recentDiscovery = false;
    } else if (name === "CreateTask") {
      createTaskOps += 1;
      recentDiscovery = false;
    } else if (name === "TodoWrite") {
      todoWriteOps += 1;
      recentDiscovery = false;
    } else if (name === "TaskList") {
      taskListOps += 1;
      recentDiscovery = false;
    } else if (name === "TaskGet") {
      taskGetOps += 1;
      recentDiscovery = false;
    } else if (name === "TaskStop") {
      taskStopOps += 1;
      recentDiscovery = false;
    } else if (name === "Sleep") {
      sleepOps += 1;
      recentDiscovery = false;
    }
  }

  return {
    readOps,
    searchOps,
    listOps,
    execOps,
    batchedExecCommandItems,
    applyPatchOps,
    createTaskOps,
    sleepOps,
    todoWriteOps,
    taskListOps,
    taskGetOps,
    taskStopOps,
    uniqueFilesRead: uniqueFiles.size,
    uniqueSearchQueries: uniqueQueries.size,
    bytesRead,
    searchToReadChains,
    totalToolLatencyMs,
    perToolLatencyMs
  };
}

export function summarizeHolonTokenOptimization(events, toolExecutions = [], options = {}) {
  const toolSummaries = toolExecutions.map(summarizeToolPayloadSize);
  const truncatedMutationToolCallRejections = events.filter((event) => {
    const kind = event?.kind ?? event?.event ?? event?.type;
    return kind === "truncated_mutation_tool_call_rejected";
  }).length;
  let nextToolIndex = 0;
  let previousTool = null;
  const rounds = [];
  const anthropicCacheState = {
    segmentId: 0,
    previousShape: null,
    seenPrefixFingerprints: new Set(),
    lastPositiveCacheRound: null,
    lastMissRound: null
  };

  for (const event of events) {
    const kind = event?.kind ?? event?.event ?? event?.type;
    const data = event?.data ?? event;
    if (kind === "tool_executed") {
      previousTool = toolSummaries[nextToolIndex] ?? {
        name: data?.tool_name ?? "unknown",
        input_bytes: 0,
        output_bytes: 0,
        status: data?.status ?? "unknown"
      };
      nextToolIndex += 1;
      continue;
    }
    if (!isProviderRoundEvent(event)) {
      continue;
    }

    const attempt = winningProviderAttempt(data?.provider_attempt_timeline);
    const modelRef =
      attempt?.model_ref ??
      data?.provider_attempt_timeline?.winning_model_ref ??
      options.modelRef ??
      null;
    const provider = attempt?.provider ?? providerFromModelRef(modelRef);
    const cacheUsage = data?.provider_cache_usage ?? {};
    const inputTokens = Number(data?.input_tokens ?? data?.token_usage?.input_tokens ?? 0);
    const cacheReadInputTokens = Number(cacheUsage.read_input_tokens ?? 0);
    const cacheCreationInputTokens = Number(cacheUsage.creation_input_tokens ?? 0);
    const round = Number(data?.round ?? event?.round ?? rounds.length + 1);
    const requestDiagnostics = data?.provider_request_diagnostics ?? {};
    const requestLoweringMode = inferRequestLoweringMode({
      provider,
      modelRef,
      round,
      promptCacheKey: data?.prompt_cache_key,
      cacheUsage,
      requestDiagnostics
    });
    const highInputZeroCacheRead =
      inputTokens >= 10_000 && cacheReadInputTokens === 0 && provider === "anthropic";
    const eventTimestampMs = eventTimestampMillis(event);
    const anthropicCache = anthropicCacheDiagnostic(provider, requestDiagnostics);
    const workingMemoryRevision = numberOrNull(data?.working_memory_revision);
    const compressionEpoch = numberOrNull(data?.compression_epoch);
    const contextManagement = contextManagementDiagnostic(provider, data, requestDiagnostics);
    const preciseAnthropicCache =
      provider === "anthropic"
        ? prepareAnthropicCacheRoundDiagnostics({
            modelRef,
            workingMemoryRevision,
            compressionEpoch,
            anthropicCache,
            cacheReadInputTokens,
            eventTimestampMs,
            contextManagement,
            state: anthropicCacheState
          })
        : {};

    const roundEntry = {
      round,
      provider,
      model_ref: modelRef,
      request_lowering_mode: requestLoweringMode,
      input_tokens: inputTokens,
      output_tokens: Number(data?.output_tokens ?? data?.token_usage?.output_tokens ?? 0),
      cache_read_input_tokens: cacheReadInputTokens,
      cache_creation_input_tokens: cacheCreationInputTokens,
      high_input_zero_cache_read: highInputZeroCacheRead,
      prompt_cache_key_present: typeof data?.prompt_cache_key === "string" && data.prompt_cache_key.length > 0,
      working_memory_revision: workingMemoryRevision,
      compression_epoch: compressionEpoch,
      incremental_continuation: incrementalContinuationDiagnostic(
        provider,
        round,
        requestLoweringMode,
        requestDiagnostics
      ),
      openai_remote_compaction: openaiRemoteCompactionDiagnostic(provider, requestDiagnostics),
      context_management: contextManagement,
      previous_tool: previousTool,
      anthropic_cache: anthropicCache,
      ...preciseAnthropicCache
    };
    rounds.push(roundEntry);
    if (provider === "anthropic") {
      updateAnthropicCacheState(anthropicCacheState, roundEntry);
    }
  }

  const summary = {
    ...summarizeTokenOptimizationRounds(rounds),
    truncated_mutation_tool_call_rejections: truncatedMutationToolCallRejections,
    exec_command_cost: summarizeExecCommandCost(toolSummaries)
  };
  return {
    schema_version: 1,
    generated_from: "holon_events",
    secret_safe: true,
    large_cache_miss_input_threshold: 10_000,
    summary,
    rounds
  };
}

function summarizeToolPayloadSize(entry) {
  const summary = {
    name: entry?.tool_name ?? "unknown",
    input_bytes: approximateJsonBytes(entry?.input),
    output_bytes: approximateJsonBytes(entry?.output),
    status: entry?.status ?? "unknown"
  };
  const execCost = summarizeExecToolCost(entry);
  if (execCost) {
    summary.exec_command_cost = execCost;
  }
  return summary;
}

function summarizeExecToolCost(entry) {
  const name = entry?.tool_name ?? "unknown";
  if (name === "ExecCommand" || name === "exec_command") {
    return summarizeSingleExecCommandCost(entry);
  }
  if (name !== "ExecCommandBatch") {
    return null;
  }
  const items = Array.isArray(entry?.output?.envelope?.result?.items)
    ? entry.output.envelope.result.items
    : [];
  return {
    item_count: items.length,
    items: items.map((item) =>
      summarizeExecCommandCostEnvelope({
        input: { cmd: item?.cmd },
        output: { envelope: { result: item?.result } }
      })
    )
  };
}

function summarizeSingleExecCommandCost(entry) {
  return summarizeExecCommandCostEnvelope(entry);
}

function summarizeExecCommandCostEnvelope(entry) {
  const diagnostics = entry?.output?.envelope?.result?.command_diagnostics ?? {};
  const cmd = typeof entry?.input?.cmd === "string" ? entry.input.cmd : "";
  const result = entry?.output?.envelope?.result ?? {};
  const indexedArtifactCount =
    (result?.stdout_artifact == null ? 0 : 1) + (result?.stderr_artifact == null ? 0 : 1);
  const artifactsCount = Array.isArray(result?.artifacts) ? result.artifacts.length : 0;
  return {
    cmd_char_count: numberOrNull(diagnostics.cmd_char_count) ?? countChars(cmd),
    cmd_estimated_tokens: numberOrNull(diagnostics.cmd_estimated_tokens) ?? Math.ceil(countChars(cmd) / 4),
    contains_heredoc: Boolean(diagnostics.contains_heredoc ?? cmd.includes("<<")),
    contains_inline_script: Boolean(diagnostics.contains_inline_script ?? commandContainsInlineScript(cmd)),
    exceeds_soft_threshold: Boolean(diagnostics.exceeds_soft_threshold ?? countChars(cmd) > 4000),
    effective_max_output_tokens: numberOrNull(diagnostics.effective_max_output_tokens),
    output_char_budget: numberOrNull(diagnostics.output_char_budget),
    output_truncated: Boolean(result?.truncated),
    artifact_count: artifactsCount || indexedArtifactCount
  };
}

function summarizeExecCommandCost(toolSummaries) {
  const stats = {
    command_count: 0,
    batch_item_count: 0,
    heredoc_count: 0,
    inline_script_count: 0,
    soft_threshold_exceeded_count: 0,
    output_truncated_count: 0,
    artifact_count: 0,
    max_cmd_char_count: 0,
    total_cmd_char_count: 0,
    command_length_buckets: {
      le_500: 0,
      le_2000: 0,
      le_4000: 0,
      gt_4000: 0
    }
  };
  for (const summary of toolSummaries) {
    const cost = summary.exec_command_cost;
    if (!cost) {
      continue;
    }
    if (Array.isArray(cost.items)) {
      stats.batch_item_count += cost.items.length;
      for (const item of cost.items) {
        addExecCost(stats, item);
      }
      continue;
    }
    addExecCost(stats, cost);
  }
  return stats;
}

function addExecCost(stats, cost) {
  stats.command_count += 1;
  const cmdChars = Number(cost.cmd_char_count ?? 0);
  stats.total_cmd_char_count += cmdChars;
  stats.max_cmd_char_count = Math.max(stats.max_cmd_char_count, cmdChars);
  if (cmdChars <= 500) {
    stats.command_length_buckets.le_500 += 1;
  } else if (cmdChars <= 2000) {
    stats.command_length_buckets.le_2000 += 1;
  } else if (cmdChars <= 4000) {
    stats.command_length_buckets.le_4000 += 1;
  } else {
    stats.command_length_buckets.gt_4000 += 1;
  }
  if (cost.contains_heredoc) {
    stats.heredoc_count += 1;
  }
  if (cost.contains_inline_script) {
    stats.inline_script_count += 1;
  }
  if (cost.exceeds_soft_threshold) {
    stats.soft_threshold_exceeded_count += 1;
  }
  if (cost.output_truncated) {
    stats.output_truncated_count += 1;
  }
  stats.artifact_count += Number(cost.artifact_count ?? 0);
}

function countChars(value) {
  return Array.from(String(value ?? "")).length;
}

function commandContainsInlineScript(cmd) {
  const lower = String(cmd ?? "").toLowerCase();
  return [
    "python -",
    "python3 -",
    "node -",
    "ruby -",
    "perl -",
    "bash -c",
    "sh -c",
    "zsh -c"
  ].some((needle) => lower.includes(needle));
}

function approximateEscapedJsonStringBytes(value) {
  let bytes = 2;
  for (const char of String(value)) {
    switch (char) {
      case "\"":
      case "\\":
      case "\b":
      case "\f":
      case "\n":
      case "\r":
      case "\t":
        bytes += 2;
        break;
      default: {
        const codePoint = char.codePointAt(0);
        bytes += codePoint !== undefined && codePoint <= 0x1f
          ? 6
          : Buffer.byteLength(char, "utf8");
      }
    }
  }
  return bytes;
}

function estimateJsonBytes(value, state) {
  if (state.nodes >= state.maxNodes || state.bytes >= state.maxBytes) {
    return 0;
  }
  state.nodes += 1;

  if (value === null) {
    return 4;
  }
  if (value === undefined || typeof value === "function" || typeof value === "symbol") {
    return 0;
  }
  if (typeof value === "string") {
    return approximateEscapedJsonStringBytes(value);
  }
  if (typeof value === "number") {
    return Number.isFinite(value) ? Buffer.byteLength(String(value), "utf8") : 4;
  }
  if (typeof value === "boolean") {
    return value ? 4 : 5;
  }
  if (typeof value === "bigint") {
    return Buffer.byteLength(String(value), "utf8");
  }

  if (Array.isArray(value)) {
    if (state.seen.has(value)) {
      return 0;
    }
    state.seen.add(value);
    let bytes = 2;
    for (let index = 0; index < value.length; index += 1) {
      if (index > 0) {
        bytes += 1;
      }
      const item = value[index];
      const itemBytes =
        item === undefined || typeof item === "function" || typeof item === "symbol"
          ? 4
          : estimateJsonBytes(item, state);
      bytes += itemBytes;
      state.bytes += itemBytes;
      if (state.nodes >= state.maxNodes || state.bytes >= state.maxBytes) {
        break;
      }
    }
    state.seen.delete(value);
    return bytes;
  }

  if (typeof value === "object") {
    if (state.seen.has(value)) {
      return 0;
    }
    state.seen.add(value);
    let bytes = 2;
    let first = true;
    for (const key of Object.keys(value)) {
      const propertyValue = value[key];
      if (
        propertyValue === undefined ||
        typeof propertyValue === "function" ||
        typeof propertyValue === "symbol"
      ) {
        continue;
      }
      if (!first) {
        bytes += 1;
      }
      first = false;
      const keyBytes = approximateEscapedJsonStringBytes(key);
      const valueBytes = estimateJsonBytes(propertyValue, state);
      bytes += keyBytes + 1 + valueBytes;
      state.bytes += keyBytes + 1 + valueBytes;
      if (state.nodes >= state.maxNodes || state.bytes >= state.maxBytes) {
        break;
      }
    }
    state.seen.delete(value);
    return bytes;
  }

  return Buffer.byteLength(String(value), "utf8");
}

function approximateJsonBytes(value) {
  return estimateJsonBytes(value, {
    seen: new Set(),
    nodes: 0,
    bytes: 0,
    maxNodes: 50_000,
    maxBytes: 50 * 1024 * 1024
  });
}

function winningProviderAttempt(timeline) {
  const attempts = Array.isArray(timeline?.attempts) ? timeline.attempts : [];
  let fallbackAttempt = null;

  for (let index = attempts.length - 1; index >= 0; index -= 1) {
    const attempt = attempts[index];
    if (attempt?.outcome === "succeeded") {
      return attempt;
    }
    if (!fallbackAttempt && (attempt?.model_ref || attempt?.provider)) {
      fallbackAttempt = attempt;
    }
  }

  return fallbackAttempt;
}

function providerFromModelRef(modelRef) {
  const normalized = String(modelRef ?? "").toLowerCase();
  if (normalized.startsWith("anthropic/")) {
    return "anthropic";
  }
  if (normalized.startsWith("openai-codex/")) {
    return "openai-codex";
  }
  if (normalized.startsWith("openai/")) {
    return "openai";
  }
  return "unknown";
}

function isProviderRoundEvent(event) {
  const kind = event?.kind ?? event?.event ?? event?.type;
  return kind === "provider_round_completed" || kind === "assistant_round";
}

export function tokenOptimizationEvents(events, transcript) {
  const transcriptProviderRounds = transcript.filter(isProviderRoundEvent);
  if (transcriptProviderRounds.length === 0) {
    return events;
  }

  const nonProviderEvents = events.filter((event) => !isProviderRoundEvent(event));
  const timestampedEvents = [...nonProviderEvents, ...transcriptProviderRounds].map((event, index) => ({
    event,
    index,
    timestamp: eventTimestampMillis(event)
  }));
  if (timestampedEvents.every((entry) => entry.timestamp !== null)) {
    return timestampedEvents
      .sort((left, right) => left.timestamp - right.timestamp || left.index - right.index)
      .map((entry) => entry.event);
  }

  const replacements = buildProviderRoundReplacementState(transcriptProviderRounds);
  let replacedAnyProviderRound = false;
  const mergedEvents = events.map((event) => {
    if (!isProviderRoundEvent(event)) {
      return event;
    }
    const replacement = takeProviderRoundReplacement(event, replacements);
    replacedAnyProviderRound ||= replacement !== event;
    return replacement;
  });
  const unmatchedTranscriptRounds = remainingProviderRoundReplacements(replacements);

  if (replacedAnyProviderRound) {
    return [...mergedEvents, ...unmatchedTranscriptRounds];
  }
  return [...nonProviderEvents, ...transcriptProviderRounds];
}

function providerRoundKey(event) {
  const data = event?.data ?? event;
  const round = data?.round ?? event?.round ?? event?.round_number ?? event?.provider_round;
  if (round !== undefined && round !== null) {
    return `round:${round}`;
  }
  const timestamp = eventTimestampMillis(event);
  return timestamp === null ? null : `time:${timestamp}`;
}

function buildProviderRoundReplacementState(providerRounds) {
  const keyed = new Map();
  const unkeyed = [];
  for (const roundEvent of providerRounds) {
    const key = providerRoundKey(roundEvent);
    if (!key) {
      unkeyed.push(roundEvent);
      continue;
    }
    const bucket = keyed.get(key) ?? [];
    bucket.push(roundEvent);
    keyed.set(key, bucket);
  }
  return { keyed, unkeyed };
}

function takeProviderRoundReplacement(event, replacements) {
  const key = providerRoundKey(event);
  if (key) {
    const bucket = replacements.keyed.get(key);
    if (bucket?.length) {
      const replacement = bucket.shift();
      if (bucket.length === 0) {
        replacements.keyed.delete(key);
      }
      return replacement;
    }
  }
  return replacements.unkeyed.shift() ?? event;
}

function remainingProviderRoundReplacements(replacements) {
  return [...replacements.keyed.values()].flat().concat(replacements.unkeyed);
}

function inferRequestLoweringMode({ provider, round, promptCacheKey, cacheUsage, requestDiagnostics }) {
  if (typeof requestDiagnostics?.request_lowering_mode === "string") {
    return requestDiagnostics.request_lowering_mode;
  }
  if (provider === "anthropic") {
    return "prompt_cache_blocks";
  }
  if ((provider === "openai" || provider === "openai-codex") && promptCacheKey) {
    return round > 1 ? "full_request_with_prompt_cache_key" : "prompt_cache_key";
  }
  if (cacheUsage?.read_input_tokens || cacheUsage?.creation_input_tokens) {
    return "provider_cache_usage_observed";
  }
  return "full_request";
}

function incrementalContinuationDiagnostic(provider, round, requestLoweringMode, requestDiagnostics) {
  if (provider !== "openai" && provider !== "openai-codex") {
    return {
      status: "not_applicable_provider",
      fallback_reason: null
    };
  }
  if (requestDiagnostics?.incremental_continuation) {
    return {
      status: requestDiagnostics.incremental_continuation.status ?? (
        requestLoweringMode === "incremental_continuation" ? "hit" : "fallback_full_request"
      ),
      fallback_reason: requestDiagnostics.incremental_continuation.fallback_reason ?? null,
      incremental_input_items: numberOrNull(
        requestDiagnostics.incremental_continuation.incremental_input_items
      ),
      full_input_items: numberOrNull(requestDiagnostics.incremental_continuation.full_input_items),
      first_mismatch_path: requestDiagnostics.incremental_continuation.first_mismatch_path ?? null,
      mismatch_kind: requestDiagnostics.incremental_continuation.mismatch_kind ?? null
    };
  }
  if (round <= 1) {
    return {
      status: "not_applicable_initial_round",
      fallback_reason: null
    };
  }
  if (requestLoweringMode === "incremental_continuation") {
    return {
      status: "hit",
      fallback_reason: null
    };
  }
  return {
    status: "fallback_full_request",
    fallback_reason: "incremental_continuation_not_observed_in_provider_round"
  };
}

function openaiRemoteCompactionDiagnostic(provider, requestDiagnostics) {
  if (provider !== "openai" && provider !== "openai-codex") {
    return null;
  }
  const compaction = requestDiagnostics?.openai_remote_compaction;
  if (!compaction) {
    return null;
  }
  return {
    status: compaction.status ?? "unknown",
    trigger_reason: compaction.trigger_reason ?? null,
    endpoint_kind: compaction.endpoint_kind ?? null,
    http_status: compaction.http_status == null ? null : numberOrNull(compaction.http_status),
    input_items: numberOrNull(compaction.input_items),
    output_items: numberOrNull(compaction.output_items),
    compaction_items: numberOrNull(compaction.compaction_items),
    latest_compaction_index: numberOrNull(compaction.latest_compaction_index),
    encrypted_content_hashes: Array.isArray(compaction.encrypted_content_hashes)
      ? compaction.encrypted_content_hashes
      : [],
    encrypted_content_bytes: Array.isArray(compaction.encrypted_content_bytes)
      ? compaction.encrypted_content_bytes.map(numberOrNull)
      : [],
    request_shape_hash: compaction.request_shape_hash ?? null,
    continuation_generation: numberOrNull(compaction.continuation_generation),
    error: compaction.error ?? null
  };
}

function anthropicCacheDiagnostic(provider, requestDiagnostics) {
  if (provider !== "anthropic") {
    return null;
  }
  const cache = requestDiagnostics?.anthropic_cache;
  if (!cache) {
    return null;
  }
  return {
    tools_count: numberOrNull(cache.tools_count),
    tools_hash: cache.tools_hash ?? null,
    system_hash: cache.system_hash ?? null,
    system_block_count: numberOrNull(cache.system_block_count),
    estimated_system_tokens: numberOrNull(cache.estimated_system_tokens),
    context_hash_by_stability: cache.context_hash_by_stability ?? {},
    conversation_message_count: numberOrNull(cache.conversation_message_count),
    conversation_content_block_count: numberOrNull(cache.conversation_content_block_count),
    cache_breakpoints: cache.cache_breakpoints ?? [],
    tokens_before_last_breakpoint: numberOrNull(cache.tokens_before_last_breakpoint),
    tokens_after_last_breakpoint: numberOrNull(cache.tokens_after_last_breakpoint),
    automatic_cache_control_requested: cache.automatic_cache_control_requested ?? false
  };
}

function prepareAnthropicCacheRoundDiagnostics({
  modelRef,
  workingMemoryRevision,
  compressionEpoch,
  anthropicCache,
  cacheReadInputTokens,
  eventTimestampMs,
  contextManagement,
  state
}) {
  const currentShape = anthropicComparableShape({
    modelRef,
    workingMemoryRevision,
    compressionEpoch,
    anthropicCache
  });
  const shapeChangedFields = state.previousShape
    ? anthropicShapeChangedFields(state.previousShape, currentShape)
    : [];
  const previousSegmentPositiveRound =
    shapeChangedFields.length > 0 ? state.lastPositiveCacheRound : null;
  if (shapeChangedFields.length > 0) {
    state.segmentId += 1;
    state.seenPrefixFingerprints = new Set();
    state.lastPositiveCacheRound = null;
    state.lastMissRound = null;
  }

  const cacheBreakpoints = Array.isArray(anthropicCache?.cache_breakpoints)
    ? anthropicCache.cache_breakpoints
    : [];
  const cacheBreakpointsWithReuse = cacheBreakpoints.map((breakpoint) => {
    const fingerprint = breakpoint?.canonical_prefix_fingerprint ?? null;
    const seenBefore =
      typeof fingerprint === "string" && state.seenPrefixFingerprints.has(fingerprint);
    return {
      ...breakpoint,
      seen_in_previous_comparable_rounds: seenBefore
    };
  });
  const currentContainsPriorCacheablePrefix = cacheBreakpointsWithReuse.some(
    (breakpoint) => breakpoint.seen_in_previous_comparable_rounds
  );

  const baseline = state.lastPositiveCacheRound;
  const classificationBaseline = baseline ?? previousSegmentPositiveRound;
  const previousCacheRead = classificationBaseline
    ? Number(classificationBaseline.cache_read_input_tokens ?? 0)
    : null;
  const dropTokens = previousCacheRead === null ? null : previousCacheRead - cacheReadInputTokens;
  const dropRatio =
    previousCacheRead !== null && previousCacheRead > 0
      ? dropTokens / previousCacheRead
      : null;
  const previousRoundElapsedMs =
    classificationBaseline &&
    Number.isFinite(eventTimestampMs) &&
    Number.isFinite(classificationBaseline?.event_timestamp_ms)
      ? Math.max(0, eventTimestampMs - classificationBaseline.event_timestamp_ms)
      : null;
  const materialDrop =
    previousCacheRead !== null &&
    previousCacheRead > 0 &&
    dropTokens >= CACHE_BREAK_ABSOLUTE_DROP_THRESHOLD &&
    cacheReadInputTokens < previousCacheRead * CACHE_BREAK_RELATIVE_RETAINED_THRESHOLD;
  const appliedContextManagement = (contextManagement?.applied_edit_count ?? 0) > 0;

  let cacheBreakClassification =
    cacheReadInputTokens > 0 ? "normal_cache_read" : "non_material_zero_cache_read";
  let cacheBreakReason =
    cacheReadInputTokens > 0
      ? "cache read is positive"
      : "cache read is zero without a material drop from the baseline";
  if (cacheReadInputTokens <= 0 && !classificationBaseline) {
    cacheBreakClassification = "true_warmup";
    cacheBreakReason = "no positive cache-read baseline in stable-shape segment";
  } else if (materialDrop && appliedContextManagement) {
    cacheBreakClassification = "context_management_applied";
    cacheBreakReason = "Anthropic reported context-management applied edits on cache-read drop";
  } else if (cacheReadInputTokens <= 0 && state.lastMissRound) {
    cacheBreakClassification = "continued_cache_miss";
    cacheBreakReason = "zero cache read followed an already-classified miss in this segment";
  } else if (
    materialDrop &&
    classificationBaseline?.compression_epoch !== compressionEpoch &&
    classificationBaseline?.compression_epoch !== null &&
    compressionEpoch !== null
  ) {
    cacheBreakClassification = "expected_after_compaction";
    cacheBreakReason = "compression epoch changed since last positive cache read";
  } else if (
    materialDrop &&
    previousRoundElapsedMs !== null &&
    previousRoundElapsedMs >= ANTHROPIC_PROMPT_CACHE_5MIN_TTL_MS
  ) {
    cacheBreakClassification = "ttl_possible";
    cacheBreakReason = "elapsed time since last positive cache read exceeded 5 minute prompt-cache TTL";
  } else if (materialDrop && currentContainsPriorCacheablePrefix) {
    cacheBreakClassification = "likely_server_side_drop";
    cacheBreakReason = "known prior cacheable prefix remained present but cache read dropped";
  } else if (materialDrop && shapeChangedFields.length > 0) {
    cacheBreakClassification = "client_prefix_changed";
    cacheBreakReason = `client prefix changed: ${shapeChangedFields.join(", ")}`;
  } else if (materialDrop && hasOnlyNewRollingTailBreakpoint(cacheBreakpointsWithReuse)) {
    cacheBreakClassification = "moving_breakpoint_non_reuse";
    cacheBreakReason = "current tail breakpoint is new and no prior cacheable prefix is present";
  } else if (materialDrop) {
    cacheBreakClassification = "client_prefix_changed";
    cacheBreakReason = "no prior known cacheable prefix remained present";
  }

  return {
    event_timestamp_ms: eventTimestampMs,
    stable_shape_segment_id: state.segmentId,
    anthropic_cache: {
      ...anthropicCache,
      cache_breakpoints: cacheBreakpointsWithReuse
    },
    last_positive_cache_read_round: baseline?.round ?? null,
    last_positive_cache_read_input_tokens: baseline
      ? Number(baseline.cache_read_input_tokens ?? 0)
      : null,
    cache_break_baseline_round: classificationBaseline?.round ?? null,
    contains_prior_known_cacheable_prefix: currentContainsPriorCacheablePrefix,
    cache_break_classification: cacheBreakClassification,
    cache_break_reason: cacheBreakReason,
    prev_cache_read_input_tokens: previousCacheRead,
    cache_read_drop_tokens: dropTokens === null ? null : Math.max(0, dropTokens),
    cache_read_drop_ratio: dropRatio === null ? null : Math.max(0, dropRatio),
    request_shape_changed: shapeChangedFields.length > 0,
    shape_changed_fields: shapeChangedFields,
    hit_to_miss_changed_fields: materialDrop ? shapeChangedFields : [],
    previous_round_elapsed_ms: previousRoundElapsedMs
  };
}

function updateAnthropicCacheState(state, roundEntry) {
  state.previousShape = anthropicComparableShape({
    modelRef: roundEntry.model_ref,
    workingMemoryRevision: roundEntry.working_memory_revision,
    compressionEpoch: roundEntry.compression_epoch,
    anthropicCache: roundEntry.anthropic_cache
  });
  for (const breakpoint of roundEntry.anthropic_cache?.cache_breakpoints ?? []) {
    const fingerprint = breakpoint?.canonical_prefix_fingerprint;
    if (typeof fingerprint === "string" && fingerprint.length > 0) {
      state.seenPrefixFingerprints.add(fingerprint);
    }
  }
  if (roundEntry.cache_read_input_tokens > 0) {
    state.lastPositiveCacheRound = roundEntry;
    state.lastMissRound = null;
  } else if (
    roundEntry.cache_break_classification &&
    roundEntry.cache_break_classification !== "true_warmup"
  ) {
    state.lastMissRound = roundEntry;
  }
}

function anthropicComparableShape({ modelRef, workingMemoryRevision, compressionEpoch, anthropicCache }) {
  return {
    model_ref: modelRef,
    working_memory_revision: workingMemoryRevision,
    compression_epoch: compressionEpoch,
    anthropic_cache: anthropicCache
  };
}

function hasOnlyNewRollingTailBreakpoint(cacheBreakpoints) {
  if (!cacheBreakpoints.length) {
    return false;
  }
  return cacheBreakpoints.every(
    (breakpoint) =>
      breakpoint?.stability === "conversation_tail" &&
      !breakpoint?.seen_in_previous_comparable_rounds
  );
}

function anthropicShapeChangedFields(previous, current) {
  const changed = [];
  if (previous.model_ref !== current.model_ref) {
    changed.push("model_ref");
  }
  if (previous.working_memory_revision !== current.working_memory_revision) {
    changed.push("working_memory_revision");
  }
  if (previous.compression_epoch !== current.compression_epoch) {
    changed.push("compression_epoch");
  }

  const previousCache = previous.anthropic_cache ?? {};
  const currentCache = current.anthropic_cache ?? {};
  for (const field of [
    "tools_hash",
    "system_hash",
    "system_block_count",
    "tokens_after_last_breakpoint",
    "automatic_cache_control_requested"
  ]) {
    if (stableJson(previousCache[field]) !== stableJson(currentCache[field])) {
      changed.push(`anthropic_cache.${field}`);
    }
  }
  if (
    stableJson(previousCache.context_hash_by_stability ?? {}) !==
    stableJson(currentCache.context_hash_by_stability ?? {})
  ) {
    changed.push("anthropic_cache.context_hash_by_stability");
  }
  if (
    stableJson(cacheBreakpointShape(previousCache.cache_breakpoints)) !==
    stableJson(cacheBreakpointShape(currentCache.cache_breakpoints))
  ) {
    changed.push("anthropic_cache.cache_breakpoints");
  }
  return changed;
}

function cacheBreakpointShape(cacheBreakpoints) {
  if (!Array.isArray(cacheBreakpoints)) {
    return {
      stable_breakpoints: [],
      rolling_tail_breakpoints: 0
    };
  }
  const stableBreakpoints = [];
  let rollingTailBreakpoints = 0;
  for (const breakpoint of cacheBreakpoints) {
    if (breakpoint?.stability === "conversation_tail") {
      rollingTailBreakpoints += 1;
      continue;
    }
    stableBreakpoints.push({
      location: breakpoint?.location ?? null,
      stability: breakpoint?.stability ?? null,
      estimated_prefix_tokens: numberOrNull(breakpoint?.estimated_prefix_tokens),
      content_hash: breakpoint?.content_hash ?? null
    });
  }
  return {
    stable_breakpoints: stableBreakpoints,
    rolling_tail_breakpoints: rollingTailBreakpoints
  };
}

function stableJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map(stableJson).join(",")}]`;
  }
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function eventTimestampMillis(event) {
  for (const value of [event?.created_at, event?.timestamp, event?.data?.created_at, event?.data?.timestamp]) {
    if (typeof value !== "string") {
      continue;
    }
    const millis = Date.parse(value);
    if (Number.isFinite(millis)) {
      return millis;
    }
  }
  return null;
}

function contextManagementDiagnostic(provider, data, requestDiagnostics = {}) {
  if (provider !== "anthropic") {
    return {
      status: "not_applicable_provider",
      disabled_reason: null
    };
  }
  const detail = data?.context_management;
  const responseDetail =
    requestDiagnostics?.anthropic_context_management ??
    data?.provider_response_diagnostics?.anthropic_context_management ??
    null;
  const appliedEdits = Array.isArray(responseDetail?.applied_edits)
    ? responseDetail.applied_edits
    : [];
  const appliedEditSummary = summarizeContextManagementAppliedEdits(appliedEdits);
  if (detail?.enabled) {
    return {
      status: "enabled",
      disabled_reason: null,
      eligible_tool_result_count: Number(detail.eligible_tool_result_count ?? 0),
      eligible_tool_result_bytes: Number(detail.eligible_tool_result_bytes ?? 0),
      retained_recent_tool_result_count: Number(detail.retained_recent_tool_result_count ?? 0),
      excluded_tool_result_count: Number(detail.excluded_tool_result_count ?? 0),
      applied_edits: appliedEdits,
      applied_edit_count: appliedEditSummary.applied_edit_count,
      applied_edit_counts: appliedEditSummary.applied_edit_counts,
      cleared_input_tokens: appliedEditSummary.cleared_input_tokens,
      cleared_tool_uses: appliedEditSummary.cleared_tool_uses,
      cleared_thinking_turns: appliedEditSummary.cleared_thinking_turns,
      cleared_thinking_tokens: appliedEditSummary.cleared_thinking_tokens
    };
  }
  return {
    status: "disabled",
    disabled_reason: detail?.disabled_reason ?? "context_management_not_enabled_for_round",
    applied_edits: appliedEdits,
    applied_edit_count: appliedEditSummary.applied_edit_count,
    applied_edit_counts: appliedEditSummary.applied_edit_counts,
    cleared_input_tokens: appliedEditSummary.cleared_input_tokens,
    cleared_tool_uses: appliedEditSummary.cleared_tool_uses,
    cleared_thinking_turns: appliedEditSummary.cleared_thinking_turns,
    cleared_thinking_tokens: appliedEditSummary.cleared_thinking_tokens
  };
}

function numberOrNull(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function summarizeContextManagementAppliedEdits(appliedEdits) {
  const summary = {
    applied_edit_count: appliedEdits.length,
    applied_edit_counts: {},
    cleared_input_tokens: 0,
    cleared_tool_uses: 0,
    cleared_thinking_turns: 0,
    cleared_thinking_tokens: 0
  };
  for (const edit of appliedEdits) {
    const kind = edit?.type ?? edit?.kind ?? "unknown";
    summary.applied_edit_counts[kind] = (summary.applied_edit_counts[kind] ?? 0) + 1;
    summary.cleared_input_tokens += numericField(edit, [
      "cleared_input_tokens",
      "cleared_tokens",
      "input_tokens"
    ]);
    summary.cleared_tool_uses += numericField(edit, [
      "cleared_tool_uses",
      "cleared_tool_use_count",
      "tool_uses"
    ]);
    summary.cleared_thinking_turns += numericField(edit, [
      "cleared_thinking_turns",
      "cleared_thinking_turn_count",
      "thinking_turns"
    ]);
    summary.cleared_thinking_tokens += numericField(edit, [
      "cleared_thinking_tokens",
      "thinking_tokens"
    ]);
  }
  return summary;
}

function numericField(object, fieldNames) {
  for (const field of fieldNames) {
    const value = Number(object?.[field]);
    if (Number.isFinite(value)) {
      return value;
    }
  }
  return 0;
}

function summarizeTokenOptimizationRounds(rounds) {
  const requestLoweringModes = {};
  let cacheReadInputTokens = 0;
  let cacheCreationInputTokens = 0;
  let highInputZeroCacheReadRounds = 0;
  const incrementalFallbackReasons = {};
  let contextManagementEnabledRounds = 0;
  let contextManagementEligibleToolResultBytes = 0;
  let contextManagementEligibleToolResultCount = 0;
  let contextManagementAppliedRounds = 0;
  const contextManagementAppliedEditCounts = {};
  let contextManagementClearedInputTokens = 0;
  let contextManagementClearedToolUses = 0;
  let openaiRemoteCompactionRounds = 0;
  const openaiRemoteCompactionStatuses = {};
  let openaiRemoteCompactionInputItems = 0;
  let openaiRemoteCompactionOutputItems = 0;
  let openaiRemoteCompactionItems = 0;
  const incrementalMismatchKinds = {};
  let cacheMissWithContextManagementAppliedRounds = 0;
  let cacheRecoveredAfterContextManagementAppliedRounds = 0;
  let previousContextManagementAppliedMiss = false;
  const cacheBreakClassificationCounts = {};
  let clientShapeChangedCacheBreakRounds = 0;
  let ttlPossibleCacheBreakRounds = 0;
  let likelyServerSideCacheBreakRounds = 0;
  let expectedAfterCompactionCacheBreakRounds = 0;
  let continuedCacheMissRounds = 0;
  let movingBreakpointNonReuseRounds = 0;

  for (const round of rounds) {
    requestLoweringModes[round.request_lowering_mode] =
      (requestLoweringModes[round.request_lowering_mode] ?? 0) + 1;
    cacheReadInputTokens += round.cache_read_input_tokens;
    cacheCreationInputTokens += round.cache_creation_input_tokens;
    if (round.high_input_zero_cache_read) {
      highInputZeroCacheReadRounds += 1;
    }
    const reason = round.incremental_continuation?.fallback_reason;
    if (reason) {
      incrementalFallbackReasons[reason] = (incrementalFallbackReasons[reason] ?? 0) + 1;
    }
    const mismatchKind = round.incremental_continuation?.mismatch_kind;
    if (mismatchKind) {
      incrementalMismatchKinds[mismatchKind] = (incrementalMismatchKinds[mismatchKind] ?? 0) + 1;
    }
    if (round.context_management?.status === "enabled") {
      contextManagementEnabledRounds += 1;
      contextManagementEligibleToolResultBytes +=
        round.context_management.eligible_tool_result_bytes ?? 0;
      contextManagementEligibleToolResultCount +=
        round.context_management.eligible_tool_result_count ?? 0;
    }
    if ((round.context_management?.applied_edit_count ?? 0) > 0) {
      contextManagementAppliedRounds += 1;
      for (const [kind, count] of Object.entries(round.context_management.applied_edit_counts ?? {})) {
        contextManagementAppliedEditCounts[kind] =
          (contextManagementAppliedEditCounts[kind] ?? 0) + count;
      }
      contextManagementClearedInputTokens += round.context_management.cleared_input_tokens ?? 0;
      contextManagementClearedToolUses += round.context_management.cleared_tool_uses ?? 0;
    }
    if (round.openai_remote_compaction) {
      openaiRemoteCompactionRounds += 1;
      const status = round.openai_remote_compaction.status ?? "unknown";
      openaiRemoteCompactionStatuses[status] =
        (openaiRemoteCompactionStatuses[status] ?? 0) + 1;
      openaiRemoteCompactionInputItems += round.openai_remote_compaction.input_items ?? 0;
      openaiRemoteCompactionOutputItems += round.openai_remote_compaction.output_items ?? 0;
      openaiRemoteCompactionItems += round.openai_remote_compaction.compaction_items ?? 0;
    }
    if (round.cache_break_classification) {
      cacheBreakClassificationCounts[round.cache_break_classification] =
        (cacheBreakClassificationCounts[round.cache_break_classification] ?? 0) + 1;
      if (
        round.cache_break_classification === "client_shape_changed" ||
        round.cache_break_classification === "client_prefix_changed"
      ) {
        clientShapeChangedCacheBreakRounds += 1;
      } else if (round.cache_break_classification === "ttl_possible") {
        ttlPossibleCacheBreakRounds += 1;
      } else if (
        round.cache_break_classification === "likely_server_side" ||
        round.cache_break_classification === "likely_server_side_drop"
      ) {
        likelyServerSideCacheBreakRounds += 1;
      } else if (round.cache_break_classification === "expected_after_compaction") {
        expectedAfterCompactionCacheBreakRounds += 1;
      } else if (round.cache_break_classification === "continued_cache_miss") {
        continuedCacheMissRounds += 1;
      } else if (round.cache_break_classification === "moving_breakpoint_non_reuse") {
        movingBreakpointNonReuseRounds += 1;
      }
    }
    if (round.cache_break_classification === "context_management_applied") {
      cacheMissWithContextManagementAppliedRounds += 1;
      previousContextManagementAppliedMiss = true;
    } else if (previousContextManagementAppliedMiss && round.cache_read_input_tokens > 0) {
      cacheRecoveredAfterContextManagementAppliedRounds += 1;
      previousContextManagementAppliedMiss = false;
    } else if (round.cache_read_input_tokens > 0) {
      previousContextManagementAppliedMiss = false;
    }
  }

  const topCacheMissRounds = rounds
    .filter((round) => round.high_input_zero_cache_read)
    .sort((left, right) => right.input_tokens - left.input_tokens)
    .slice(0, 10)
    .map((round) => ({
      round: round.round,
      provider: round.provider,
      model_ref: round.model_ref,
      input_tokens: round.input_tokens,
      previous_tool: round.previous_tool
    }));

  return {
    rounds: rounds.length,
    request_lowering_modes: requestLoweringModes,
    cache_read_input_tokens: cacheReadInputTokens,
    cache_creation_input_tokens: cacheCreationInputTokens,
    high_input_zero_cache_read_rounds: highInputZeroCacheReadRounds,
    incremental_fallback_reasons: incrementalFallbackReasons,
    incremental_mismatch_kinds: incrementalMismatchKinds,
    context_management_enabled_rounds: contextManagementEnabledRounds,
    context_management_eligible_tool_result_bytes: contextManagementEligibleToolResultBytes,
    context_management_eligible_tool_result_count: contextManagementEligibleToolResultCount,
    context_management_applied_rounds: contextManagementAppliedRounds,
    context_management_applied_edit_counts: contextManagementAppliedEditCounts,
    context_management_cleared_input_tokens: contextManagementClearedInputTokens,
    context_management_cleared_tool_uses: contextManagementClearedToolUses,
    openai_remote_compaction_rounds: openaiRemoteCompactionRounds,
    openai_remote_compaction_statuses: openaiRemoteCompactionStatuses,
    openai_remote_compaction_input_items: openaiRemoteCompactionInputItems,
    openai_remote_compaction_output_items: openaiRemoteCompactionOutputItems,
    openai_remote_compaction_items: openaiRemoteCompactionItems,
    cache_miss_with_context_management_applied_rounds: cacheMissWithContextManagementAppliedRounds,
    cache_recovered_after_context_management_applied_rounds:
      cacheRecoveredAfterContextManagementAppliedRounds,
    cache_break_classification_counts: cacheBreakClassificationCounts,
    client_shape_changed_cache_break_rounds: clientShapeChangedCacheBreakRounds,
    ttl_possible_cache_break_rounds: ttlPossibleCacheBreakRounds,
    likely_server_side_cache_break_rounds: likelyServerSideCacheBreakRounds,
    expected_after_compaction_cache_break_rounds: expectedAfterCompactionCacheBreakRounds,
    continued_cache_miss_rounds: continuedCacheMissRounds,
    moving_breakpoint_non_reuse_rounds: movingBreakpointNonReuseRounds,
    top_cache_miss_rounds: topCacheMissRounds
  };
}

function summarizeSdkTranscript(transcript, workspaceDir) {
  const toolUseById = new Map();
  const uniqueFiles = new Set();
  const uniqueQueries = new Set();
  let readOps = 0;
  let searchOps = 0;
  let listOps = 0;
  let execOps = 0;
  let createTaskOps = 0;
  let sleepOps = 0;
  let todoWriteOps = 0;
  let taskListOps = 0;
  let taskGetOps = 0;
  let taskStopOps = 0;
  let bytesRead = 0;
  let searchToReadChains = 0;
  let recentDiscovery = false;

  for (const message of transcript) {
    if (message.type === "assistant") {
      for (const block of message.message?.content ?? []) {
        if (block.type !== "tool_use") {
          continue;
        }
        toolUseById.set(block.id, { name: block.name, input: block.input ?? {} });
        if (block.name === "Read") {
          readOps += 1;
          if (recentDiscovery) {
            searchToReadChains += 1;
          }
          recentDiscovery = false;
          if (block.input?.file_path) {
            uniqueFiles.add(relativizeIfPossible(block.input.file_path, workspaceDir));
          }
        } else if (block.name === "Grep") {
          searchOps += 1;
          recentDiscovery = true;
          if (block.input?.pattern) {
            uniqueQueries.add(String(block.input.pattern));
          }
        } else if (block.name === "Glob") {
          listOps += 1;
          recentDiscovery = true;
          if (block.input?.pattern) {
            uniqueQueries.add(String(block.input.pattern));
          }
        } else if (block.name === "Bash") {
          execOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TodoWrite") {
          todoWriteOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskList") {
          taskListOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskGet") {
          taskGetOps += 1;
          recentDiscovery = false;
        } else if (block.name === "TaskStop") {
          taskStopOps += 1;
          recentDiscovery = false;
        }
      }
    }

    if (message.type === "user") {
      for (const block of message.message?.content ?? []) {
        if (block.type !== "tool_result" || !block.tool_use_id) {
          continue;
        }
        const toolUse = toolUseById.get(block.tool_use_id);
        if (toolUse?.name === "Read") {
          const toolResult = message.tool_use_result;
          const fileContent = toolResult?.file?.content;
          if (typeof fileContent === "string") {
            bytesRead += Buffer.byteLength(fileContent, "utf8");
          } else if (typeof block.content === "string") {
            bytesRead += Buffer.byteLength(block.content, "utf8");
          }
        }
      }
    }
  }

  return {
    readOps,
    searchOps,
    listOps,
    execOps,
    createTaskOps,
    sleepOps,
    todoWriteOps,
    taskListOps,
    taskGetOps,
    taskStopOps,
    uniqueFilesRead: uniqueFiles.size,
    uniqueSearchQueries: uniqueQueries.size,
    bytesRead,
    searchToReadChains
  };
}

function relativizeIfPossible(filePath, workspaceDir) {
  try {
    const relative = path.relative(workspaceDir, filePath);
    if (!relative.startsWith("..") && !path.isAbsolute(relative)) {
      return relative;
    }
  } catch {}
  return String(filePath);
}

async function listTrackedFiles(repoPath, includePaths) {
  const args = ["-C", repoPath, "ls-files", "-z", "--", ...includePaths];
  const result = await runCommand("git", args, repoRoot, process.env, true);
  return (result.stdout || "")
    .split("\0")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function extractLastExitCode(log) {
  const matches = [...log.matchAll(/\[exit=(\d+)/g)];
  if (matches.length === 0) {
    return 0;
  }
  return Number(matches[matches.length - 1][1]);
}

async function diffChangedFiles(pristineDir, workspaceDir, taskDir) {
  const nameOnly = await runCommand(
    "git",
    ["diff", "--no-index", "--name-only", pristineDir, workspaceDir],
    repoRoot,
    process.env,
    true,
    true
  );
  const diff = await runCommand(
    "git",
    ["diff", "--no-index", pristineDir, workspaceDir],
    repoRoot,
    process.env,
    true,
    true
  );
  await writeFile(path.join(taskDir, "git.diff"), diff.stdout || "", "utf8");
  return (nameOnly.stdout || "")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => line.replace(`${workspaceDir}/`, "").replace(`${pristineDir}/`, ""));
}

async function writeJson(pathname, value) {
  await writeFile(pathname, JSON.stringify(value, null, 2), "utf8");
}

async function readJson(pathname) {
  return JSON.parse(await readFile(pathname, "utf8"));
}

async function readJsonIfExists(pathname) {
  try {
    return JSON.parse(await readFile(pathname, "utf8"));
  } catch {
    return null;
  }
}

async function writeJsonl(pathname, rows) {
  await writeFile(pathname, rows.join("\n") + (rows.length ? "\n" : ""), "utf8");
}

async function readJsonlSafe(pathname) {
  try {
    const content = await readFile(pathname, "utf8");
    return content
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  } catch {
    return [];
  }
}

function agentJsonlArtifactCandidates(agentDir, filename, taskDir = null) {
  return [
    taskDir ? path.join(taskDir, filename) : null,
    path.join(agentDir, filename),
    path.join(agentDir, ".holon", "ledger", filename)
  ].filter(Boolean);
}

async function readAgentJsonlArtifact(agentDir, filename, taskDir = null) {
  for (const candidate of agentJsonlArtifactCandidates(agentDir, filename, taskDir)) {
    const rows = await readJsonlSafe(candidate);
    if (rows.length > 0) {
      return rows;
    }
  }
  return [];
}

async function copyAgentJsonlArtifact(agentDir, taskDir, filename) {
  for (const candidate of agentJsonlArtifactCandidates(agentDir, filename)) {
    if (await isNonEmptyFile(candidate)) {
      await copyIfExists(candidate, path.join(taskDir, filename));
      return;
    }
  }
}

async function isNonEmptyFile(pathname) {
  try {
    const entry = await stat(pathname);
    return entry.isFile() && entry.size > 0;
  } catch {
    return false;
  }
}

async function copyIfExists(from, to) {
  try {
    await copyFile(from, to);
  } catch {}
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function randomPort() {
  const server = await import("node:net").then(({ createServer }) => createServer());
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  const port = address.port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function runCommand(
  command,
  args,
  cwd,
  env,
  tolerateFailure = false,
  allowDiffExit = false,
  timeoutMs = null
) {
  return new Promise((resolve, reject) => {
    const childEnv = {
      ...env,
      PWD: cwd
    };
    const child = spawn(command, args, {
      cwd,
      env: childEnv,
      stdio: ["ignore", "pipe", "pipe"]
    });
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    const timeoutHandle =
      typeof timeoutMs === "number" && timeoutMs > 0
        ? setTimeout(() => {
            timedOut = true;
            child.kill("SIGTERM");
            setTimeout(() => child.kill("SIGKILL"), 1000).unref();
          }, timeoutMs)
        : null;
    child.stdout.on("data", (chunk) => {
      stdout += String(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("error", (error) => {
      if (timeoutHandle) {
        clearTimeout(timeoutHandle);
      }
      reject(error);
    });
    child.on("exit", (exitCode) => {
      if (timeoutHandle) {
        clearTimeout(timeoutHandle);
      }
      const result = { stdout, stderr, exitCode: exitCode ?? 1, timedOut };
      if (timedOut) {
        resolve(result);
        return;
      }
      if (exitCode === 0 || (allowDiffExit && exitCode === 1) || tolerateFailure) {
        resolve(result);
      } else {
        reject(Object.assign(new Error(stderr || stdout || `${command} failed`), result));
      }
    });
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === __filename) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}

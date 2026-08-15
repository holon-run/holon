import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  ensureBaseShaExists,
  loadBenchmarkSuite,
  loadRealTaskManifest,
  resolveRepoPath,
  validateBenchmarkSuite,
  validateRealTaskManifest
} from "../lib/manifest.mjs";
import {
  assertHolonProviderRoundTelemetry,
  buildPairedSummary,
  buildHolonBenchmarkEnv,
  buildOperatorPrompt,
  classifyVerificationResult,
  classifyBenchmarkFinalizationDecision,
  classifyGithubCiChecks,
  classifyHolonBenchmarkCompletion,
  collectChangedFilesFromGitOutputs,
  copyHolonProviderHttpTraceArtifacts,
  captureHolonProviderRequests,
  codexBenchmarkConfigToml,
  detectScopeViolation,
  evaluateRealTaskSuccess,
  orderPairedRunners,
  normalizeHolonEventEnvelope,
  normalizeProviderRoundUsage,
  parseClaudeCliJsonl,
  parseCodexJsonl,
  resolveDriverHolonBinary,
  readHolonAuditEvents,
  selectHolonFinalMessage,
  shouldRunManifestVerifier,
  summarizeHolonTokenOptimization,
  tokenOptimizationEvents
} from "../run.mjs";
import {
  artifactDirForTask,
  benchmarkLabelsForTask,
  branchNameForTask,
  prTitleForTask,
  worktreeNameForTask
} from "../lib/naming.mjs";

test("resolveDriverHolonBinary always selects the benchmark driver repository", () => {
  const originalOverride = process.env.HOLON_BENCHMARK_BINARY;
  const driverRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

  try {
    delete process.env.HOLON_BENCHMARK_BINARY;
    assert.equal(
      resolveDriverHolonBinary(),
      path.join(driverRoot, "target", "release", "holon")
    );

    process.env.HOLON_BENCHMARK_BINARY = "target/benchmark/holon";
    assert.equal(
      resolveDriverHolonBinary(),
      path.join(driverRoot, "target", "benchmark", "holon")
    );
  } finally {
    if (originalOverride === undefined) {
      delete process.env.HOLON_BENCHMARK_BINARY;
    } else {
      process.env.HOLON_BENCHMARK_BINARY = originalOverride;
    }
  }
});

test("validateRealTaskManifest accepts a phase-1 manifest", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-1611-tool-guidance-markdown",
    repo: { name: "holon-run/holon", local_path: "." },
    issue: { number: 15, title: "Dogfood task" },
    base: { branch: "main", sha: "abc123" },
    benchmark: { mode: "replay" },
    task: {
      kind: "implementation",
      operator_prompt: "Fix it."
    },
    verification: {
      commands: ["cargo test"]
    },
    evaluation: {
      summary: "Tests pass.",
      expected_outcome: "either",
      scope_policy: "soft",
      allowed_paths: ["src/prompt"],
      forbidden_paths: ["src/runtime"]
    },
    budget: { max_minutes: 90, max_operator_followups: 0 },
    review: { mode: "standardized", expected_comment_count: 2 },
    metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
  });

  assert.equal(manifest.task_id, "holon-1611-tool-guidance-markdown");
});

test("validateRealTaskManifest allows issue-driven tasks without operator_prompt", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-1611-tool-guidance-markdown",
    repo: { name: "holon-run/holon", local_path: "." },
    issue: { number: 15, title: "Dogfood task" },
    base: { branch: "main", sha: "abc123" },
    benchmark: { mode: "live" },
    task: {
      kind: "implementation"
    },
    verification: {
      commands: ["cargo test"]
    },
    evaluation: {
      expected_outcome: "change_required",
      scope_policy: "soft",
      allowed_paths: [],
      forbidden_paths: []
    },
    budget: { max_minutes: 90, max_operator_followups: 0 },
    review: { mode: "none" },
    metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
  });

  assert.equal(manifest.task.kind, "implementation");
  assert.equal("operator_prompt" in manifest.task, false);
});

test("validateRealTaskManifest rejects unsupported keys and followups", () => {
  assert.throws(
    () =>
      validateRealTaskManifest({
        schema_version: 1,
        task_id: "holon-1611-tool-guidance-markdown",
        repo: { name: "holon-run/holon", local_path: "." },
        issue: { number: 15, title: "Dogfood task" },
        base: { branch: "main", sha: "abc123" },
        benchmark: { mode: "live" },
        task: {
          kind: "implementation",
          operator_prompt: "Fix it.",
          extra: true
        },
        verification: {
          commands: ["cargo test"]
        },
        evaluation: {
          expected_outcome: "change_required",
          scope_policy: "soft",
          allowed_paths: ["src/prompt"],
          forbidden_paths: ["src/runtime"]
        },
        budget: { max_minutes: 90, max_operator_followups: 1 },
        review: { mode: "standardized" },
        metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
      }),
    /unsupported key extra/
  );
});

test("validateRealTaskManifest rejects empty verification commands and invalid path entries", () => {
  assert.throws(
    () =>
      validateRealTaskManifest({
        schema_version: 1,
        task_id: "holon-1611-tool-guidance-markdown",
        repo: { name: "holon-run/holon", local_path: "." },
        issue: { number: 15, title: "Dogfood task" },
        base: { branch: "main", sha: "abc123" },
        benchmark: { mode: "live" },
        task: {
          kind: "implementation",
          operator_prompt: "Fix it."
        },
        verification: {
          commands: ["", "cargo test"]
        },
        evaluation: {
          summary: "Tests pass.",
          expected_outcome: "change_required",
          scope_policy: "soft",
          allowed_paths: ["src/prompt", "   "],
          forbidden_paths: ["src/runtime"]
        },
        budget: { max_minutes: 90, max_operator_followups: 0 },
        review: { mode: "standardized" },
        metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
      }),
    /verification\.commands must contain only non-empty strings/
  );
});

test("validateRealTaskManifest accepts structured verification commands", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-1611-tool-guidance-markdown",
    repo: { name: "holon-run/holon", local_path: "." },
    issue: { number: 15, title: "Dogfood task" },
    base: { branch: "main", sha: "abc123" },
    benchmark: { mode: "live" },
    task: {
      kind: "implementation"
    },
    verification: {
      commands: [
        "cargo test",
        {
          run: "cargo test runtime_flow --test runtime_flow --quiet",
          stale_if_output_matches: ["no test target named"],
          allow_failure: false
        }
      ]
    },
    evaluation: {
      expected_outcome: "change_required",
      scope_policy: "soft",
      allowed_paths: [],
      forbidden_paths: []
    },
    budget: { max_minutes: 90, max_operator_followups: 0 },
    review: { mode: "none" },
    metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
  });

  assert.equal(manifest.verification.commands.length, 2);
});

test("validateRealTaskManifest defaults absent verification for real PR benchmarks", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-1611-tool-guidance-markdown",
    repo: { name: "holon-run/holon", local_path: "." },
    issue: { number: 15, title: "Dogfood task" },
    base: { branch: "main", sha: "abc123" },
    benchmark: { mode: "live" },
    task: {
      kind: "implementation"
    },
    evaluation: {
      expected_outcome: "change_required",
      scope_policy: "soft",
      allowed_paths: [],
      forbidden_paths: []
    },
    budget: { max_minutes: 90, max_operator_followups: 0 },
    review: { mode: "none" },
    metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
  });

  assert.deepEqual(manifest.verification, { commands: [] });
});

test("buildOperatorPrompt uses an issue-driven template with PR policy", () => {
  const prompt = buildOperatorPrompt(
    validateRealTaskManifest({
      schema_version: 1,
      task_id: "holon-1611-tool-guidance-markdown",
      repo: { name: "holon-run/holon", local_path: "." },
      issue: { number: 15, title: "Dogfood task" },
      base: { branch: "main", sha: "abc123" },
      benchmark: { mode: "replay" },
      task: {
        kind: "implementation",
        operator_prompt: "Extract the registry without changing behavior."
      },
      verification: {
        commands: ["cargo test prompt::tools::"]
      },
      evaluation: {
        summary: "Prompt tools stay green.",
        expected_outcome: "change_required",
        scope_policy: "soft",
        allowed_paths: ["src/prompt"],
        forbidden_paths: ["src/runtime"]
      },
      budget: { max_minutes: 90, max_operator_followups: 0 },
      review: { mode: "none" },
      metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
    }),
    {
      pr: {
        submit_pr: true,
        draft_pr: true
      }
    }
  );

  assert.match(prompt, /Fix GitHub issue #15 in this repository\./);
  assert.match(prompt, /Use `gh` commands to inspect the issue and related GitHub context\./);
  assert.match(prompt, /Do not stop to ask for confirmation; continue until the issue is fully handled\./);
  assert.match(prompt, /Do not stop at analysis or partial plans when implementation is still possible\./);
  assert.match(prompt, /Complete the issue acceptance criteria in one pull request/);
  assert.match(prompt, /multiple commits inside that one PR/);
  assert.match(prompt, /continue moving the real implementation until the issue is fully solved/);
  assert.match(prompt, /Only stop without implementation if you conclude the task cannot be completed/);
  assert.match(prompt, /https:\/\/github\.com\/holon-run\/holon\/issues\/15/);
  assert.match(prompt, /Submit a pull request if you make a real implementation\./);
  assert.match(prompt, /Submit it as a draft pull request\./);
  assert.doesNotMatch(prompt, /Extract the registry without changing behavior\./);
  assert.doesNotMatch(prompt, /cargo test prompt::tools::/);
  assert.doesNotMatch(prompt, /allowed_paths/);
});

test("buildHolonBenchmarkEnv disables provider fallback only for live benchmark runs", () => {
  const env = buildHolonBenchmarkEnv(
    {
      PATH: process.env.PATH ?? "",
      EXISTING_FLAG: "1"
    },
    {
      model_ref: "openai-codex/gpt-5.3-codex-spark",
      env: {
        HOLON_ANTHROPIC_CONTEXT_MANAGEMENT: "true"
      }
    },
    {
      benchmark: {
        mode: "live"
      }
    }
  );

  assert.equal(env.EXISTING_FLAG, "1");
  assert.equal(env.HOLON_ANTHROPIC_CONTEXT_MANAGEMENT, "true");
  assert.equal(env.HOLON_MODEL, "openai-codex/gpt-5.3-codex-spark");
  assert.equal(env.HOLON_DISABLE_PROVIDER_FALLBACK, "1");

  const replayEnv = buildHolonBenchmarkEnv(
    {
      PATH: process.env.PATH ?? "",
      EXISTING_FLAG: "1"
    },
    {
      model_ref: "openai-codex/gpt-5.3-codex-spark"
    },
    {
      benchmark: {
        mode: "replay"
      }
    }
  );

  assert.equal(replayEnv.EXISTING_FLAG, "1");
  assert.equal(replayEnv.HOLON_MODEL, "openai-codex/gpt-5.3-codex-spark");
  assert.equal("HOLON_DISABLE_PROVIDER_FALLBACK" in replayEnv, false);
});

test("classifyHolonBenchmarkCompletion reports incomplete for awake running state", () => {
  const result = classifyHolonBenchmarkCompletion({
    runTimedOut: false,
    runFinalStatus: "completed",
    durableState: {
      agent_status: "awake_running",
      current_work_item_state: "open",
      work_plan_in_progress_count: 1
    }
  });
  assert.equal(result.terminal_state, "incomplete");
  assert.equal(result.classification, "agent_incomplete");
});

test("classifyHolonBenchmarkCompletion reports runner_interrupted on timeout", () => {
  const result = classifyHolonBenchmarkCompletion({
    runTimedOut: true,
    runFinalStatus: null,
    durableState: {
      agent_status: "asleep",
      current_work_item_state: "completed",
      work_plan_in_progress_count: 0
    }
  });
  assert.equal(result.terminal_state, "incomplete");
  assert.equal(result.classification, "runner_interrupted");
});

test("classifyBenchmarkFinalizationDecision blocks incomplete runner state", () => {
  const blocked = classifyBenchmarkFinalizationDecision({
    terminalState: "incomplete",
    completionClassification: "agent_incomplete"
  });
  assert.equal(blocked.can_finalize, false);
  assert.equal(blocked.reason, "agent_incomplete");

  const allowed = classifyBenchmarkFinalizationDecision({
    terminalState: "terminal",
    completionClassification: "completed"
  });
  assert.equal(allowed.can_finalize, true);
});

test("captureHolonProviderRequests redacts sensitive fields", () => {
  const captured = captureHolonProviderRequests({
    transcript: [
      {
        kind: "assistant_round",
        round: 2,
        created_at: "2026-05-04T00:00:00Z",
        stop_reason: "completed",
        data: {
          requested_model: "openai-codex/gpt-5.3-codex-spark",
          provider_request_diagnostics: {
            request_lowering_mode: "provider_window_replay",
            authorization: "Bearer secret-value"
          },
          token_usage: {
            input_tokens: 123,
            output_tokens: 45
          },
          blocks: [
            { type: "text", text: "你好" },
            { type: "tool_use", name: "ExecCommand", id: "call_1" }
          ]
        }
      }
    ],
    events: []
  });
  assert.equal(captured.request_body_available, false);
  assert.equal(captured.rounds.length, 1);
  assert.equal(
    captured.rounds[0].assistant_blocks[0].text,
    "你好"
  );
  assert.equal(
    captured.rounds[0].assistant_blocks[0].bytes,
    Buffer.byteLength("你好", "utf8")
  );
  assert.equal(
    captured.rounds[0].provider_request_diagnostics.authorization,
    "[REDACTED]"
  );
  assert.equal(captured.rounds[0].token_usage.input_tokens, 123);
  assert.equal(captured.rounds[0].token_usage.output_tokens, 45);
});

test("copyHolonProviderHttpTraceArtifacts copies home-scoped traces", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "holon-trace-test-"));
  try {
    const homeDir = path.join(root, "home");
    const taskDir = path.join(root, "task");
    const traceDir = path.join(homeDir, ".holon", "http-trace", "agent-1");
    await fs.mkdir(traceDir, { recursive: true });
    await fs.writeFile(path.join(traceDir, "trace.jsonl"), "{\"type\":\"request\"}\n", "utf8");

    const copied = await copyHolonProviderHttpTraceArtifacts(homeDir, "agent-1", taskDir);

    assert.equal(copied, true);
    assert.equal(
      await fs.readFile(path.join(taskDir, "provider-http-trace", "trace.jsonl"), "utf8"),
      "{\"type\":\"request\"}\n"
    );
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("buildOperatorPrompt preserves legacy push-only PR policy", () => {
  const prompt = buildOperatorPrompt(
    validateRealTaskManifest({
      schema_version: 1,
      task_id: "holon-1611-tool-guidance-markdown",
      repo: { name: "holon-run/holon", local_path: "." },
      issue: { number: 15, title: "Dogfood task" },
      base: { branch: "main", sha: "abc123" },
      benchmark: { mode: "replay" },
      task: {
        kind: "implementation"
      },
      verification: {
        commands: ["cargo test"]
      },
      evaluation: {
        expected_outcome: "change_required",
        scope_policy: "soft",
        allowed_paths: [],
        forbidden_paths: []
      },
      budget: { max_minutes: 90, max_operator_followups: 0 },
      review: { mode: "none" },
      metadata: { difficulty: "medium", benchmark_group: "prompt-system" }
    }),
    {
      pr: {
        push_branch: true,
        create_draft: false
      }
    }
  );

  assert.match(prompt, /Push the benchmark branch if you make a real implementation\./);
  assert.match(prompt, /Do not submit a pull request automatically\./);
  assert.doesNotMatch(prompt, /Submit a pull request if you make a real implementation\./);
});

test("collectChangedFilesFromGitOutputs includes untracked files", () => {
  const files = collectChangedFilesFromGitOutputs(
    "src/runtime.rs\n",
    " M src/runtime.rs\n?? tests/new_runtime_flow.rs\nR  old.rs -> renamed.rs\n"
  );

  assert.deepEqual(files, [
    "renamed.rs",
    "src/runtime.rs",
    "tests/new_runtime_flow.rs"
  ]);
});

test("parseCodexJsonl tracks Codex CLI turns separately from tokens and tolerates junk lines", () => {
  const parsed = parseCodexJsonl(
    [
      "{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\"}}",
      "{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"working\"}}",
      "not-json",
      "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":11,\"output_tokens\":7}}",
      "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}"
    ].join("\n")
  );

  assert.equal(parsed.shellCommands, 1);
  assert.equal(parsed.toolCalls, 1);
  assert.equal(parsed.finalMessage, "working");
  assert.equal(parsed.inputTokens, 16);
  assert.equal(parsed.outputTokens, 10);
  assert.equal(parsed.codexCliTurns, 2);
});

test("parseClaudeCliJsonl tracks Claude CLI turns, tools, and final output", () => {
  const workspaceDir = "/tmp/worktree";
  const parsed = parseClaudeCliJsonl(
    [
      JSON.stringify({ type: "system", subtype: "init" }),
      JSON.stringify({
        type: "assistant",
        message: {
          content: [
            { type: "text", text: "I will inspect the file." },
            {
              type: "tool_use",
              id: "tool_1",
              name: "Read",
              input: { file_path: "/tmp/worktree/src/main.rs" }
            }
          ]
        }
      }),
      JSON.stringify({
        type: "user",
        message: {
          content: [
            {
              type: "tool_result",
              tool_use_id: "tool_1",
              content: "fn main() {}"
            }
          ]
        }
      }),
      JSON.stringify({
        type: "assistant",
        message: {
          content: [
            {
              type: "tool_use",
              id: "tool_2",
              name: "Bash",
              input: { command: "cargo test" }
            }
          ]
        }
      }),
      JSON.stringify({
        type: "result",
        subtype: "success",
        result: "Done",
        num_turns: 2,
        usage: {
          input_tokens: 120,
          output_tokens: 34
        }
      }),
      "junk line"
    ].join("\n"),
    workspaceDir
  );

  assert.equal(parsed.finalMessage, "Done");
  assert.equal(parsed.toolCalls, 2);
  assert.equal(parsed.shellCommands, 1);
  assert.equal(parsed.inputTokens, 120);
  assert.equal(parsed.outputTokens, 34);
  assert.equal(parsed.claudeCliTurns, 2);
  assert.equal(parsed.readOps, 1);
  assert.equal(parsed.execOps, 1);
  assert.equal(parsed.uniqueFilesRead, 1);
  assert.equal(parsed.bytesRead, Buffer.byteLength("fn main() {}", "utf8"));
  assert.equal(parsed.searchToReadChains, 0);
});

test("parseClaudeCliJsonl falls back to latest assistant text on non-success results", () => {
  const parsed = parseClaudeCliJsonl(
    [
      JSON.stringify({
        type: "assistant",
        message: {
          content: [{ type: "text", text: "Need more permissions." }]
        }
      }),
      JSON.stringify({
        type: "result",
        subtype: "error",
        result: "",
        num_turns: 1,
        usage: {
          input_tokens: 10,
          output_tokens: 5
        }
      })
    ].join("\n")
  );

  assert.equal(parsed.finalMessage, "Need more permissions.");
  assert.equal(parsed.errorKind, "error");
  assert.equal(parsed.claudeCliTurns, 1);
});

test("classifyVerificationResult tolerates stale verifier failures", () => {
  const stale = classifyVerificationResult(
    {
      run: "cargo test runtime_flow --test runtime_flow --quiet",
      stale_if_output_matches: ["no test target named"]
    },
    101,
    "error: no test target named `runtime_flow` in default-run packages"
  );
  assert.equal(stale.status, "stale");
  assert.equal(stale.tolerated, true);

  const failure = classifyVerificationResult("cargo test", 101, "test failed");
  assert.equal(failure.status, "failed");
  assert.equal(failure.tolerated, false);
});

test("selectHolonFinalMessage prefers failure brief over progressy final_text", () => {
  const message = selectHolonFinalMessage(
    {
      final_status: "runtime_error",
      final_text:
        "I hit mixed schema in messages.jsonl; I'll quickly inspect the schema first."
    },
    [
      {
        kind: "result",
        created_at: "2026-04-25T03:11:00Z",
        text: "Delta since base checkpoint..."
      },
      {
        kind: "failure",
        created_at: "2026-04-25T03:22:35Z",
        text: "Turn failed while processing system_tick: max_output_tokens"
      }
    ]
  );

  assert.equal(
    message,
    "Turn failed while processing system_tick: max_output_tokens"
  );
});

test("codexBenchmarkConfigToml is empty by default", () => {
  const config = codexBenchmarkConfigToml();
  assert.equal(config, "");
});

test("codexBenchmarkConfigToml supports explicit overrides", () => {
  const config = codexBenchmarkConfigToml({
    projectDocDisabled: true,
    bundledSkillsDisabled: true
  });
  assert.match(config, /project_doc_max_bytes = 0/);
  assert.match(config, /\[skills\.bundled\]/);
  assert.match(config, /enabled = false/);
});

test("scope policy can be measured softly or enforced hard", () => {
  const changedFiles = ["src/runtime/provider_turn.rs"];
  const scopeViolation = detectScopeViolation(changedFiles, {
    allowed_paths: ["src/prompt"],
    forbidden_paths: ["src/runtime"]
  });
  assert.equal(scopeViolation, true);
  assert.equal(
    evaluateRealTaskSuccess({
      verifyExitCode: 0,
      runnerResult: { errorKind: null, timedOut: false },
      changedFiles: ["src/runtime/provider_turn.rs"],
      scopeViolation,
      scopePolicy: "soft",
      expectedOutcome: "change_required"
    }),
    true
  );
  assert.equal(
    evaluateRealTaskSuccess({
      verifyExitCode: 0,
      runnerResult: { errorKind: null, timedOut: false },
      changedFiles: ["src/runtime/provider_turn.rs"],
      scopeViolation,
      scopePolicy: "hard",
      expectedOutcome: "change_required"
    }),
    false
  );
});

test("real task success honors expected outcome", () => {
  assert.equal(
    evaluateRealTaskSuccess({
      verifyExitCode: 0,
      runnerResult: { errorKind: null, timedOut: false },
      changedFiles: [],
      scopeViolation: false,
      scopePolicy: "soft",
      expectedOutcome: "change_required"
    }),
    false
  );
  assert.equal(
    evaluateRealTaskSuccess({
      verifyExitCode: 0,
      runnerResult: { errorKind: null, timedOut: false },
      changedFiles: [],
      scopeViolation: false,
      scopePolicy: "soft",
      expectedOutcome: "either"
    }),
    true
  );
  assert.equal(
    evaluateRealTaskSuccess({
      verifyExitCode: 0,
      runnerResult: { errorKind: null, timedOut: false },
      changedFiles: ["src/runtime.rs"],
      scopeViolation: false,
      scopePolicy: "soft",
      expectedOutcome: "no_change_expected"
    }),
    false
  );
});

test("github ci summary classifies check buckets", () => {
  assert.deepEqual(
    classifyGithubCiChecks([
      { name: "Rust", bucket: "pass" },
      { name: "Coverage", bucket: "pass" }
    ]),
    {
      status: "passed",
      success: true,
      pending: 0,
      failed: 0,
      passed: 2,
      checks: [
        { name: "Rust", bucket: "pass" },
        { name: "Coverage", bucket: "pass" }
      ]
    }
  );
  assert.equal(
    classifyGithubCiChecks([
      { name: "Rust", bucket: "pass" },
      { name: "Holon Trigger", bucket: "pending" }
    ]).status,
    "pending"
  );
  assert.equal(
    classifyGithubCiChecks([
      { name: "Rust", bucket: "fail" },
      { name: "Coverage", bucket: "pass" }
    ]).success,
    false
  );
});

test("validateBenchmarkSuite rejects invalid and duplicate runner ids", () => {
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "openai-phase1",
        label_prefix: "openai-phase1",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [{ runner_id: "Not_A_Runner", driver: "holon", model_ref: "openai-codex/gpt-5.3-codex-spark" }],
        pr: { submit_pr: true, draft_pr: true, push_branch: true },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /runner_id must be a stable lowercase slug/
  );
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "deepseek-pilot",
        label_prefix: "deepseek-pilot",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [
          { runner_id: "deepseek", driver: "holon", model_ref: "deepseek/model-a" },
          { runner_id: "deepseek", driver: "holon", model_ref: "deepseek/model-b" }
        ],
        pr: { submit_pr: false },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /runner_id must be unique/
  );
});

test("validateBenchmarkSuite accepts repeated paired execution metadata", () => {
  const suite = validateBenchmarkSuite({
    suite_id: "deepseek-pilot",
    label_prefix: "deepseek-pilot",
    tasks: ["benchmarks/tasks/task.yaml"],
    repetitions: 5,
    execution: {
      runner_order: "paired_randomized",
      random_seed: 20260814,
      max_parallel_runners: 1,
      cooldown_ms: 5000
    },
    runners: [
      {
        runner_id: "deepseek-anthropic-messages",
        driver: "holon",
        model_ref: "deepseek@default/deepseek-v4-flash",
        transport: "anthropic_messages",
        endpoint: "default"
      },
      {
        runner_id: "deepseek-openai-responses",
        driver: "holon",
        model_ref: "deepseek@responses/deepseek-v4-flash",
        transport: "openai_responses",
        endpoint: "responses",
        pricing: {
          currency: "USD",
          effective_at: "2026-08-16T16:00:00Z",
          source: "provider pricing page",
          cache_miss_input_per_million: 0.28,
          cache_read_input_per_million: 0.028,
          cache_creation_input_per_million: 0,
          output_per_million: 0.42
        }
      }
    ],
    pr: { submit_pr: false },
    timeouts: { ci_poll_minutes: 30 }
  });

  assert.equal(suite.repetitions, 5);
  assert.equal(suite.execution.max_parallel_runners, 1);
  assert.equal(suite.runners[1].endpoint, "responses");
  assert.equal(suite.runners[1].pricing.currency, "USD");
});

test("validateBenchmarkSuite rejects incomplete or invalid pricing", () => {
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "deepseek-pilot",
        label_prefix: "deepseek-pilot",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [
          {
            runner_id: "deepseek",
            driver: "holon",
            model_ref: "deepseek/model",
            pricing: {
              currency: "USD",
              effective_at: "not-a-date",
              source: "provider pricing page",
              cache_miss_input_per_million: 0.28,
              cache_read_input_per_million: 0.028,
              cache_creation_input_per_million: 0,
              output_per_million: 0.42
            }
          }
        ],
        pr: { submit_pr: false },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /effective_at must be an ISO-8601 timestamp/
  );
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "deepseek-pilot",
        label_prefix: "deepseek-pilot",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [
          {
            runner_id: "deepseek",
            driver: "holon",
            model_ref: "deepseek/model",
            pricing: {
              currency: "USD",
              effective_at: "2026",
              source: "provider pricing page",
              cache_miss_input_per_million: 0.28,
              cache_read_input_per_million: 0.028,
              cache_creation_input_per_million: 0,
              output_per_million: 0.42
            }
          }
        ],
        pr: { submit_pr: false },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /effective_at must be an ISO-8601 timestamp/
  );
});

test("validateBenchmarkSuite requires serial execution for paired ordering", () => {
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "deepseek-pilot",
        label_prefix: "deepseek-pilot",
        tasks: ["benchmarks/tasks/task.yaml"],
        execution: {
          runner_order: "paired_randomized",
          random_seed: 1,
          max_parallel_runners: 2,
          cooldown_ms: 0
        },
        runners: [
          { runner_id: "one", driver: "holon", model_ref: "deepseek/model-a" },
          { runner_id: "two", driver: "holon", model_ref: "deepseek/model-b" }
        ],
        pr: { submit_pr: false },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /max_parallel_runners must be 1/
  );
});

test("validateBenchmarkSuite requires driver-specific runner fields", () => {
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "openai-phase1",
        label_prefix: "openai-phase1",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [{ runner_id: "holon-openai", driver: "holon" }],
        pr: { submit_pr: true, draft_pr: true, push_branch: true },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /driver=holon must include non-empty model_ref/
  );
});

test("validateBenchmarkSuite accepts canonical PR policy booleans", () => {
  const suite = validateBenchmarkSuite({
    suite_id: "openai-phase1",
    label_prefix: "openai-phase1",
    tasks: ["benchmarks/tasks/task.yaml"],
    runners: [
      { runner_id: "holon-openai", driver: "holon", model_ref: "openai-codex/gpt-5.3-codex-spark" },
      { runner_id: "codex-openai", driver: "codex", model: "gpt-5.3-codex-spark" }
    ],
    pr: { submit_pr: true, draft_pr: true },
    timeouts: { ci_poll_minutes: 30 }
  });

  assert.equal(suite.pr.submit_pr, true);
  assert.equal(suite.pr.draft_pr, true);
});

test("validateBenchmarkSuite accepts anthropic holon and claude-cli runners", () => {
  const suite = validateBenchmarkSuite({
    suite_id: "anthropic-phase1",
    label_prefix: "anthropic-phase1",
    tasks: ["benchmarks/tasks/task.yaml"],
    runners: [
      {
        runner_id: "holon-anthropic",
        driver: "holon",
        model_ref: "anthropic/claude-sonnet-4-6",
        env: {
          HOLON_ANTHROPIC_CONTEXT_MANAGEMENT: "true",
          HOLON_ANTHROPIC_CONTEXT_MANAGEMENT_TRIGGER_INPUT_TOKENS: "30000"
        }
      },
      { runner_id: "claude-cli", driver: "claude_cli", model: "claude-sonnet-4-6" }
    ],
    pr: { submit_pr: true, draft_pr: true, push_branch: true },
    timeouts: { ci_poll_minutes: 30 }
  });

  assert.equal(suite.runners.length, 2);
  assert.equal(suite.runners[0].runner_id, "holon-anthropic");
  assert.equal(suite.runners[0].env.HOLON_ANTHROPIC_CONTEXT_MANAGEMENT, "true");
  assert.equal(suite.runners[1].runner_id, "claude-cli");
});

test("naming helpers follow canonical conventions", () => {
  assert.equal(
    branchNameForTask(
      "holon-1611-tool-guidance-markdown",
      "holon-openai",
      1,
      "OpenAI Phase 1"
    ),
    "bench/openai-phase-1/holon-1611-tool-guidance-markdown/holon-openai/run-01"
  );
  assert.equal(
    worktreeNameForTask(15, "codex-openai", 3),
    "bench-0015-codex-openai-run-03"
  );
  assert.deepEqual(
    artifactDirForTask("/results", "suite", "task", "runner", 3),
    { runId: "run-03", path: "/results/suite/task/runner/run-03" }
  );
  assert.equal(
    prTitleForTask(15, "Dogfood: tool guidance", "holon-openai"),
    "[bench][holon-openai][#15] Dogfood: tool guidance"
  );
  assert.deepEqual(benchmarkLabelsForTask(15, "holon-openai"), [
    "bench",
    "bench:task-15",
    "runner:holon-openai"
  ]);
});

test("orderPairedRunners is deterministic and alternating reverses even repetitions", () => {
  const runners = [
    { runner_id: "one" },
    { runner_id: "two" },
    { runner_id: "three" }
  ];
  const first = orderPairedRunners({
    runnerConfigs: runners,
    runnerOrder: "paired_randomized",
    randomSeed: 20260814,
    taskId: "task-a",
    repetition: 1
  });
  const second = orderPairedRunners({
    runnerConfigs: runners,
    runnerOrder: "paired_randomized",
    randomSeed: 20260814,
    taskId: "task-a",
    repetition: 1
  });
  assert.deepEqual(first, second);
  assert.deepEqual(
    orderPairedRunners({
      runnerConfigs: runners,
      runnerOrder: "alternating",
      taskId: "task-a",
      repetition: 2
    }).map((runner) => runner.runner_id),
    ["three", "two", "one"]
  );
  assert.deepEqual(runners.map((runner) => runner.runner_id), ["one", "two", "three"]);
});

test("buildPairedSummary preserves execution order and emits metric deltas", () => {
  const entries = buildPairedSummary(
    [
      {
        task_id: "task-a",
        repetition: 1,
        runner: "responses",
        pair_order: 1,
        transport: "openai_responses",
        success: true,
        duration_ms: 80,
        input_tokens: 90,
        logical_input_tokens: 90,
        output_tokens: 20,
        provider_duration_ms: 50,
        provider_retry_count: 0,
        reasoning_tokens: 8,
        cache_read_input_tokens: 4,
        cache_miss_input_tokens: 86,
        estimated_cost_usd: 0.00004
      },
      {
        task_id: "task-a",
        repetition: 1,
        runner: "anthropic",
        pair_order: 2,
        transport: "anthropic_messages",
        success: false,
        duration_ms: 100,
        input_tokens: 100,
        logical_input_tokens: 120,
        output_tokens: 30,
        provider_duration_ms: 70,
        provider_retry_count: 1,
        reasoning_tokens: 5,
        cache_read_input_tokens: 10,
        cache_miss_input_tokens: 100,
        estimated_cost_usd: 0.00006
      }
    ],
    [{ runner_id: "anthropic" }, { runner_id: "responses" }]
  );

  assert.deepEqual(entries[0].scheduled_runner_order, ["responses", "anthropic"]);
  assert.equal(entries[0].complete, true);
  assert.equal(entries[0].comparisons[0].duration_ms_delta, -20);
  assert.equal(entries[0].comparisons[0].provider_retry_count_delta, -1);
  assert.equal(entries[0].comparisons[0].logical_input_tokens_delta, -30);
  assert.equal(entries[0].comparisons[0].cache_miss_input_tokens_delta, -14);
  assert.ok(Math.abs(entries[0].comparisons[0].estimated_cost_usd_delta + 0.00002) < 1e-12);
});

test("readHolonAuditEvents paginates stable DB event envelopes", async () => {
  const calls = [];
  const pages = [
    {
      event_log_epoch: "epoch-1",
      newest_seq: 2,
      has_newer: true,
      events: [
        {
          id: "audit-1",
          event_seq: 1,
          event_log_epoch: "epoch-1",
          contract_version: 1,
          ts: "2026-08-15T00:00:00Z",
          agent_id: "agent",
          type: "provider_round_completed",
          payload_schema: "holon.runtime_event.legacy",
          payload_schema_version: 1,
          payload: { round: 1 }
        },
        {
          id: "audit-2",
          event_seq: 2,
          event_log_epoch: "epoch-1",
          contract_version: 1,
          ts: "2026-08-15T00:00:01Z",
          agent_id: "agent",
          type: "tool_executed",
          payload_schema: "holon.runtime_event.legacy",
          payload_schema_version: 1,
          payload: { tool_name: "ExecCommand" }
        }
      ]
    },
    {
      event_log_epoch: "epoch-1",
      newest_seq: 3,
      has_newer: false,
      events: [
        {
          id: "audit-3",
          event_seq: 3,
          event_log_epoch: "epoch-1",
          contract_version: 1,
          ts: "2026-08-15T00:00:02Z",
          agent_id: "agent",
          type: "provider_round_completed",
          payload_schema: "holon.runtime_event.legacy",
          payload_schema_version: 1,
          payload: { round: 2 }
        }
      ]
    }
  ];
  const events = await readHolonAuditEvents({
    holonBinary: "holon",
    agentId: "agent",
    homeDir: "/tmp/home",
    cwd: "/tmp/repo",
    env: {},
    execute: async (_command, args) => {
      calls.push(args);
      return { stdout: JSON.stringify(pages[calls.length - 1]), stderr: "", exitCode: 0 };
    }
  });

  assert.deepEqual(events.map((event) => event.event_seq), [1, 2, 3]);
  assert.equal(events[0].kind, "provider_round_completed");
  assert.equal(events[0].data.round, 1);
  assert.deepEqual(calls[1].slice(-2), ["--after-seq", "2"]);
});

test("event envelope normalization rejects invalid events", () => {
  assert.throws(
    () => normalizeHolonEventEnvelope({ event_seq: 0, type: "x", payload: {} }),
    /invalid event sequence/
  );
});

test("DB event export failures are explicit", async () => {
  await assert.rejects(
    readHolonAuditEvents({
      holonBinary: "holon",
      agentId: "agent",
      homeDir: "/tmp/home",
      cwd: "/tmp/repo",
      env: {},
      execute: async () => ({
        stdout: "{\"events\":[]}",
        stderr: "runtime DB unavailable",
        exitCode: 2
      })
    }),
    /holon DB event export failed with exit code 2: runtime DB unavailable/
  );

  await assert.rejects(
    readHolonAuditEvents({
      holonBinary: "holon",
      agentId: "agent",
      homeDir: "/tmp/home",
      cwd: "/tmp/repo",
      env: {},
      execute: async () => ({ stdout: "not-json", stderr: "", exitCode: 0 })
    }),
    /failed to parse holon DB event export/
  );
});

test("provider usage normalization is transport-neutral and cost-aware", () => {
  const pricing = {
    currency: "USD",
    effective_at: "2026-08-16T16:00:00Z",
    source: "provider pricing page",
    cache_miss_input_per_million: 1,
    cache_read_input_per_million: 0.1,
    cache_creation_input_per_million: 2,
    output_per_million: 3
  };
  const responses = normalizeProviderRoundUsage({
    provider: "deepseek",
    modelRef: "deepseek@responses/deepseek-v4-flash",
    transport: "openai_responses",
    pricing,
    data: {
      input_tokens: 1000,
      output_tokens: 100,
      provider_cache_usage: { read_input_tokens: 800, creation_input_tokens: 0 }
    }
  });
  assert.equal(responses.logical_input_tokens, 1000);
  assert.equal(responses.cache_miss_input_tokens, 200);
  assert.equal(responses.estimated_cost_usd, 0.00058);

  const anthropic = normalizeProviderRoundUsage({
    provider: "deepseek",
    modelRef: "deepseek@default/deepseek-v4-flash",
    transport: "anthropic_messages",
    pricing,
    data: {
      input_tokens: 200,
      output_tokens: 100,
      provider_cache_usage: { read_input_tokens: 800, creation_input_tokens: 50 }
    }
  });
  assert.equal(anthropic.logical_input_tokens, 1050);
  assert.equal(anthropic.cache_miss_input_tokens, 200);
  assert.equal(anthropic.estimated_cost_usd, 0.00068);

  const fullHit = normalizeProviderRoundUsage({
    provider: "deepseek",
    modelRef: "deepseek@responses/deepseek-v4-flash",
    transport: "openai_responses",
    data: {
      input_tokens: 1000,
      output_tokens: 10,
      provider_cache_usage: { read_input_tokens: 1000, creation_input_tokens: 0 }
    }
  });
  assert.equal(fullHit.cache_miss_input_tokens, 0);

  const noHit = normalizeProviderRoundUsage({
    provider: "deepseek",
    modelRef: "deepseek@responses/deepseek-v4-flash",
    transport: "openai_responses",
    data: {
      input_tokens: 1000,
      output_tokens: 10,
      provider_cache_usage: { read_input_tokens: 0, creation_input_tokens: 0 }
    }
  });
  assert.equal(noHit.cache_miss_input_tokens, 1000);

  const explicitResponsesWins = normalizeProviderRoundUsage({
    provider: "anthropic",
    modelRef: "fallback/model",
    transport: "openai_responses",
    data: {
      input_tokens: 1000,
      output_tokens: 10,
      provider_cache_usage: { read_input_tokens: 800, creation_input_tokens: 0 }
    }
  });
  assert.equal(explicitResponsesWins.usage_semantics, "openai_responses");
  assert.equal(explicitResponsesWins.logical_input_tokens, 1000);
});

test("provider usage normalization reports invalid or unavailable data explicitly", () => {
  const usage = normalizeProviderRoundUsage({
    provider: "openai",
    modelRef: "openai/gpt",
    data: {
      input_tokens: -1,
      output_tokens: null,
      provider_cache_usage: { read_input_tokens: 5 }
    }
  });
  assert.equal(usage.estimated_cost_usd, null);
  assert.ok(usage.usage_validation_issues.includes("input_tokens_negative"));
  assert.ok(usage.usage_validation_issues.includes("output_tokens_missing"));
  assert.ok(usage.usage_validation_issues.includes("cache_read_exceeds_logical_input"));

  const invalidPriced = normalizeProviderRoundUsage({
    provider: "openai",
    modelRef: "openai/gpt",
    pricing: {
      currency: "USD",
      effective_at: "2026-08-16T16:00:00Z",
      source: "provider pricing page",
      cache_miss_input_per_million: 1,
      cache_read_input_per_million: 0.1,
      cache_creation_input_per_million: 2,
      output_per_million: 3
    },
    data: { input_tokens: null, output_tokens: 10 }
  });
  assert.equal(invalidPriced.estimated_cost_usd, null);
});

test("provider round telemetry cannot silently disappear after model execution", () => {
  assert.throws(
    () =>
      assertHolonProviderRoundTelemetry({
        modelRounds: 2,
        tokenOptimization: { summary: { rounds: 0 } }
      }),
    /contained no provider_round_completed telemetry/
  );
  assert.doesNotThrow(() =>
    assertHolonProviderRoundTelemetry({
      modelRounds: 2,
      tokenOptimization: { summary: { rounds: 2 } }
    })
  );
});

test("summarizeHolonTokenOptimization reports local compaction and cache warm-up", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "provider_round_completed",
      data: {
        round: 4,
        input_tokens: 900,
        output_tokens: 20,
        provider_cache_usage: { read_input_tokens: 0, creation_input_tokens: 0 },
        turn_local_compaction: {
          trigger_reason: "estimated_tokens_exceeded_trigger",
          pre_compaction_estimated_tokens: 1400,
          projected_estimated_tokens: 850,
          compacted_rounds: 2,
          exact_tail_rounds: 1,
          degraded_rounds: 0,
          compacted_tool_results: 1,
          preserved_artifact_refs: 3,
          trigger_budget_fallback_applied: true,
          strict_fallback_applied: false
        }
      }
    },
    {
      kind: "provider_round_completed",
      data: {
        round: 5,
        input_tokens: 950,
        output_tokens: 20,
        provider_cache_usage: { read_input_tokens: 700, creation_input_tokens: 0 },
        turn_local_compaction: null
      }
    }
  ]);

  assert.equal(diagnostics.summary.turn_local_compaction_telemetry_status, "available");
  assert.equal(diagnostics.summary.turn_local_compaction_applied_rounds, 1);
  assert.equal(diagnostics.summary.turn_local_compaction_pre_estimated_tokens, 1400);
  assert.equal(diagnostics.summary.turn_local_compaction_projected_estimated_tokens, 850);
  assert.equal(diagnostics.summary.turn_local_compacted_rounds, 2);
  assert.equal(diagnostics.summary.turn_local_compacted_tool_results, 1);
  assert.equal(diagnostics.summary.turn_local_preserved_artifact_refs, 3);
  assert.equal(diagnostics.summary.turn_local_compaction_cache_warmup_observed, 1);
  assert.equal(diagnostics.summary.turn_local_compaction_cache_warmup_hits, 1);
  assert.equal(diagnostics.rounds[0].turn_local_compaction.status, "applied");
  assert.equal(
    diagnostics.rounds[0].turn_local_compaction.trigger_budget_fallback_applied,
    true
  );
  assert.equal(diagnostics.rounds[1].turn_local_compaction.status, "not_applied");
});

test("summarizeHolonTokenOptimization marks legacy local compaction telemetry unavailable", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "provider_round_completed",
      data: {
        round: 1,
        input_tokens: 100,
        output_tokens: 10
      }
    }
  ]);

  assert.equal(diagnostics.rounds[0].turn_local_compaction.status, "unavailable");
  assert.equal(diagnostics.summary.turn_local_compaction_telemetry_status, "unavailable");
});

test("manifest verifier runs only when GitHub CI is not the verification source", () => {
  assert.equal(shouldRunManifestVerifier({ pr: { submit_pr: false } }), true);
  assert.equal(shouldRunManifestVerifier({ pr: { submit_pr: true } }), false);
});

test("summarizeHolonTokenOptimization reports Anthropic cache miss rounds safely", () => {
  const diagnostics = summarizeHolonTokenOptimization(
    [
      {
        kind: "tool_executed",
        data: {
          tool_name: "ExecCommand"
        }
      },
      {
        kind: "provider_round_completed",
        data: {
          round: 7,
          reasoning_tokens: null,
          input_tokens: 35_000,
          output_tokens: 120,
          provider_cache_usage: {
            read_input_tokens: 0,
            creation_input_tokens: 0
          },
          prompt_cache_key: "agent-cache-key",
          working_memory_revision: 4,
          compression_epoch: 2,
          provider_attempt_timeline: {
            attempts: [
              {
                provider: "anthropic",
                model_ref: "anthropic/claude-sonnet-4-6",
                outcome: "failed",
                duration_ms: 25
              },
              {
                provider: "anthropic",
                model_ref: "anthropic/claude-sonnet-4-6",
                outcome: "succeeded",
                duration_ms: 75
              }
            ],
            aggregated_token_usage: {
              output_tokens_details: {
                reasoning_tokens: 40
              }
            },
            winning_model_ref: "anthropic/claude-sonnet-4-6"
          }
        }
      }
    ],
    [
      {
        tool_name: "ExecCommand",
        status: "success",
        input: {
          cmd: "cat <<'EOF' > /tmp/large-file\nsecret-ish payload omitted\nEOF"
        },
        output: {
          content: "ok"
        }
      }
    ]
  );

  assert.equal(diagnostics.secret_safe, true);
  assert.equal(diagnostics.summary.high_input_zero_cache_read_rounds, 1);
  assert.equal(diagnostics.summary.request_lowering_modes.prompt_cache_blocks, 1);
  assert.equal(diagnostics.summary.provider_duration_ms, 100);
  assert.equal(diagnostics.summary.provider_attempt_count, 2);
  assert.equal(diagnostics.summary.provider_retry_count, 1);
  assert.equal(diagnostics.summary.provider_error_count, 1);
  assert.equal(diagnostics.summary.reasoning_tokens, 40);
  assert.equal(diagnostics.rounds[0].request_lowering_mode, "prompt_cache_blocks");
  assert.equal(diagnostics.rounds[0].previous_tool.name, "ExecCommand");
  assert.equal(typeof diagnostics.rounds[0].previous_tool.input_bytes, "number");
  assert.equal(diagnostics.rounds[0].previous_tool.exec_command_cost.contains_heredoc, true);
  assert.equal(diagnostics.summary.exec_command_cost.heredoc_count, 1);
  assert.equal(JSON.stringify(diagnostics).includes("secret-ish payload omitted"), false);
});

test("summarizeHolonTokenOptimization reports exec command cost without raw command text", () => {
  const largeCommand = `python3 - <<'PY'\n${"print('secret payload')\n".repeat(300)}PY`;
  const diagnostics = summarizeHolonTokenOptimization(
    [
      {
        kind: "tool_executed",
        data: {
          tool_name: "ExecCommand"
        }
      },
      {
        kind: "provider_round_completed",
        data: {
          round: 1,
          input_tokens: 100,
          output_tokens: 20,
          provider_attempt_timeline: {
            attempts: [{ provider: "openai", model_ref: "openai/gpt-5.4", outcome: "succeeded" }]
          }
        }
      }
    ],
    [
      {
        tool_name: "ExecCommand",
        status: "success",
        input: { cmd: largeCommand },
        output: {
          envelope: {
            result: {
              truncated: true,
              stdout_artifact: 0,
              artifacts: [{ path: "/tmp/stdout.log" }],
              command_diagnostics: {
                cmd_preview: "python3 - <<'PY'...",
                cmd_char_count: Array.from(largeCommand).length,
                cmd_estimated_tokens: 1600,
                contains_heredoc: true,
                contains_inline_script: true,
                exceeds_soft_threshold: true,
                effective_max_output_tokens: 2000,
                output_char_budget: 8000
              }
            }
          }
        }
      }
    ]
  );

  assert.equal(diagnostics.summary.exec_command_cost.command_count, 1);
  assert.equal(diagnostics.summary.exec_command_cost.inline_script_count, 1);
  assert.equal(diagnostics.summary.exec_command_cost.soft_threshold_exceeded_count, 1);
  assert.equal(diagnostics.summary.exec_command_cost.output_truncated_count, 1);
  assert.equal(diagnostics.summary.exec_command_cost.artifact_count, 1);
  assert.equal(JSON.stringify(diagnostics).includes("secret payload"), false);
});

test("summarizeHolonTokenOptimization reads assistant_round ledger diagnostics", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "assistant_round",
      round: 2,
      data: {
        token_usage: {
          input_tokens: 649,
          output_tokens: 78
        },
        provider_cache_usage: {
          read_input_tokens: 9600,
          creation_input_tokens: 0
        },
        prompt_cache_key: "agent-cache-key",
        provider_attempt_timeline: {
          attempts: [
            {
              provider: "anthropic",
              model_ref: "anthropic/claude-opus-4-6",
              outcome: "succeeded",
              token_usage: {
                input_tokens: 649,
                output_tokens: 78
              }
            }
          ],
          winning_model_ref: "anthropic/claude-opus-4-6"
        },
        provider_request_diagnostics: {
          request_lowering_mode: "claude_cli_like_prompt_cache",
          anthropic_cache: {
            cache_strategy: "claude_cli_like",
            system_hash: "system",
            tools_hash: "tools",
            context_hash_by_stability: {
              agent_scoped: "context"
            },
            cache_breakpoints: [
              {
                provider_payload_path: "system[1]",
                canonical_prefix_fingerprint: "prefix",
                stability: "provider_system"
              }
            ]
          }
        },
        context_management: {
          enabled: true,
          eligible_tool_result_bytes: 1024,
          eligible_tool_result_count: 3,
          retained_recent_tool_result_count: 2
        }
      }
    }
  ]);

  assert.equal(diagnostics.summary.rounds, 1);
  assert.equal(diagnostics.summary.cache_read_input_tokens, 9600);
  assert.equal(
    diagnostics.summary.request_lowering_modes.claude_cli_like_prompt_cache,
    1
  );
  assert.equal(diagnostics.summary.context_management_enabled_rounds, 1);
  assert.equal(diagnostics.rounds[0].round, 2);
});

test("tokenOptimizationEvents preserves tool/provider chronological ordering", () => {
  const events = [
    {
      kind: "tool_executed",
      created_at: "2026-04-28T19:08:46.000Z",
      data: { tool_name: "ExecCommand", status: "success" }
    },
    {
      kind: "tool_executed",
      created_at: "2026-04-28T19:08:48.000Z",
      data: { tool_name: "ApplyPatch", status: "success" }
    }
  ];
  const transcript = [
    {
      kind: "assistant_round",
      round: 1,
      created_at: "2026-04-28T19:08:45.000Z",
      data: { token_usage: { input_tokens: 100, output_tokens: 10 } }
    },
    {
      kind: "assistant_round",
      round: 2,
      created_at: "2026-04-28T19:08:47.000Z",
      data: { token_usage: { input_tokens: 100, output_tokens: 10 } }
    },
    {
      kind: "assistant_round",
      round: 3,
      created_at: "2026-04-28T19:08:49.000Z",
      data: { token_usage: { input_tokens: 100, output_tokens: 10 } }
    }
  ];

  const diagnostics = summarizeHolonTokenOptimization(
    tokenOptimizationEvents(events, transcript),
    [
      { tool_name: "ExecCommand", input: {}, output: {}, status: "success" },
      { tool_name: "ApplyPatch", input: {}, output: {}, status: "success" }
    ]
  );

  assert.equal(diagnostics.rounds[0].previous_tool, null);
  assert.equal(diagnostics.rounds[1].previous_tool.name, "ExecCommand");
  assert.equal(diagnostics.rounds[2].previous_tool.name, "ApplyPatch");
});

test("summarizeHolonTokenOptimization exposes OpenAI continuation fallback reason", () => {
  const diagnostics = summarizeHolonTokenOptimization(
    [
      {
        kind: "provider_round_completed",
        data: {
          round: 2,
          input_tokens: 1200,
          output_tokens: 80,
          prompt_cache_key: "default",
          provider_attempt_timeline: {
            attempts: [
              {
                provider: "openai-codex",
                model_ref: "openai-codex/gpt-5.3-codex-spark",
                outcome: "succeeded"
              }
            ]
          }
        }
      }
    ],
    [],
    {
      modelRef: "openai-codex/gpt-5.3-codex-spark"
    }
  );

  assert.equal(
    diagnostics.rounds[0].incremental_continuation.status,
    "fallback_full_request"
  );
  assert.equal(
    diagnostics.summary.incremental_fallback_reasons
      .incremental_continuation_not_observed_in_provider_round,
    1
  );
});

test("summarizeHolonTokenOptimization reports OpenAI incremental continuation hits", () => {
  const diagnostics = summarizeHolonTokenOptimization(
    [
      {
        kind: "provider_round_completed",
        data: {
          round: 2,
          input_tokens: 120,
          output_tokens: 80,
          provider_request_diagnostics: {
            request_lowering_mode: "incremental_continuation",
            incremental_continuation: {
              status: "hit",
              incremental_input_items: 1,
              full_input_items: 3
            }
          },
          provider_attempt_timeline: {
            attempts: [
              {
                provider: "openai",
                model_ref: "openai/gpt-5.4",
                outcome: "succeeded"
              }
            ]
          }
        }
      }
    ],
    [],
    {
      modelRef: "openai/gpt-5.4"
    }
  );

  assert.equal(diagnostics.rounds[0].request_lowering_mode, "incremental_continuation");
  assert.equal(diagnostics.rounds[0].incremental_continuation.status, "hit");
  assert.equal(diagnostics.rounds[0].incremental_continuation.incremental_input_items, 1);
  assert.equal(diagnostics.summary.request_lowering_modes.incremental_continuation, 1);
  assert.deepEqual(diagnostics.summary.incremental_fallback_reasons, {});
});

test("summarizeHolonTokenOptimization reports OpenAI remote compaction", () => {
  const diagnostics = summarizeHolonTokenOptimization(
    [
      {
        kind: "provider_round_completed",
        data: {
          round: 3,
          input_tokens: 900,
          output_tokens: 80,
          provider_request_diagnostics: {
            request_lowering_mode: "provider_window_compacted",
            openai_remote_compaction: {
              status: "compacted",
              trigger_reason: "provider_window_item_threshold",
              endpoint_kind: "responses_compact",
              http_status: null,
              input_items: 12,
              output_items: 3,
              compaction_items: 2,
              latest_compaction_index: 2,
              encrypted_content_hashes: ["hash-a", "hash-b"],
              encrypted_content_bytes: [8, 9],
              request_shape_hash: "shape-hash",
              continuation_generation: 4
            }
          },
          provider_attempt_timeline: {
            attempts: [
              {
                provider: "openai",
                model_ref: "openai/gpt-5.4",
                outcome: "succeeded"
              }
            ]
          }
        }
      }
    ],
    [],
    {
      modelRef: "openai/gpt-5.4"
    }
  );

  assert.equal(diagnostics.rounds[0].request_lowering_mode, "provider_window_compacted");
  assert.equal(diagnostics.rounds[0].openai_remote_compaction.status, "compacted");
  assert.equal(diagnostics.rounds[0].openai_remote_compaction.endpoint_kind, "responses_compact");
  assert.equal(diagnostics.rounds[0].openai_remote_compaction.http_status, null);
  assert.equal(diagnostics.rounds[0].openai_remote_compaction.input_items, 12);
  assert.equal(diagnostics.summary.request_lowering_modes.provider_window_compacted, 1);
  assert.equal(diagnostics.summary.openai_remote_compaction_rounds, 1);
  assert.equal(diagnostics.summary.openai_remote_compaction_statuses.compacted, 1);
  assert.equal(diagnostics.summary.openai_remote_compaction_input_items, 12);
  assert.equal(diagnostics.summary.openai_remote_compaction_output_items, 3);
  assert.equal(diagnostics.summary.openai_remote_compaction_items, 2);
});

test("summarizeHolonTokenOptimization reports truncated mutation call rejections", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "truncated_mutation_tool_call_rejected",
      data: {
        tool_name: "ApplyPatch",
        tool_call_id: "patch-1",
        stop_reason: "max_tokens"
      }
    },
    {
      kind: "provider_round_completed",
      data: {
        round: 1,
        input_tokens: 500,
        output_tokens: 50,
        provider_attempt_timeline: {
          attempts: [
            {
              provider: "openai",
              model_ref: "openai/gpt-5.4",
              outcome: "succeeded"
            }
          ]
        }
      }
    }
  ]);

  assert.equal(diagnostics.summary.truncated_mutation_tool_call_rejections, 1);
});

test("summarizeHolonTokenOptimization reports Anthropic context management usage", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "provider_round_completed",
      data: {
        round: 3,
        input_tokens: 22_000,
        output_tokens: 90,
        context_management: {
          enabled: true,
          eligible_tool_result_count: 2,
          eligible_tool_result_bytes: 8192,
          retained_recent_tool_result_count: 3,
          excluded_tool_result_count: 1
        },
        provider_attempt_timeline: {
          attempts: [
            {
              provider: "anthropic",
              model_ref: "anthropic/claude-sonnet-4-6",
              outcome: "succeeded"
            }
          ]
        }
      }
    }
  ]);

  assert.equal(diagnostics.rounds[0].context_management.status, "enabled");
  assert.equal(
    diagnostics.rounds[0].context_management.eligible_tool_result_bytes,
    8192
  );
  assert.equal(diagnostics.summary.context_management_enabled_rounds, 1);
  assert.equal(
    diagnostics.summary.context_management_eligible_tool_result_bytes,
    8192
  );
});

test("summarizeHolonTokenOptimization reports Anthropic cache diagnostics", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    {
      kind: "provider_round_completed",
      data: {
        round: 1,
        input_tokens: 15_000,
        output_tokens: 500,
        provider: "anthropic",
        model_ref: "anthropic/claude-sonnet-4-6",
        provider_request_diagnostics: {
          request_lowering_mode: "prompt_cache_blocks",
          anthropic_cache: {
            tools_count: 3,
            tools_hash: "abc123",
            system_hash: "def456",
            system_block_count: 2,
            estimated_system_tokens: 500,
            context_hash_by_stability: {
              "stable": "hash1",
              "agent_scoped": "hash2"
            },
            conversation_message_count: 2,
            conversation_content_block_count: 3,
            cache_breakpoints: [
              {
                location: "system_blocks[0]",
                stability: "stable",
                estimated_prefix_tokens: 0,
                content_hash: "bp_hash1"
              },
              {
                location: "messages[0].content[1]",
                stability: "turn_scoped",
                estimated_prefix_tokens: 550,
                content_hash: "bp_hash2"
              }
            ],
            tokens_before_last_breakpoint: 550,
            tokens_after_last_breakpoint: 500,
            automatic_cache_control_requested: false
          }
        },
        provider_attempt_timeline: {
          attempts: [
            {
              provider: "anthropic",
              model_ref: "anthropic/claude-sonnet-4-6",
              outcome: "succeeded"
            }
          ]
        }
      }
    }
  ]);

  assert.equal(diagnostics.rounds[0].anthropic_cache.tools_count, 3);
  assert.equal(diagnostics.rounds[0].anthropic_cache.system_block_count, 2);
  assert.equal(diagnostics.rounds[0].anthropic_cache.cache_breakpoints.length, 2);
  assert.equal(diagnostics.rounds[0].anthropic_cache.cache_breakpoints[0].stability, "stable");
  assert.equal(diagnostics.rounds[0].anthropic_cache.tokens_before_last_breakpoint, 550);
  assert.equal(diagnostics.rounds[0].anthropic_cache.tokens_after_last_breakpoint, 500);
});

test("summarizeHolonTokenOptimization classifies normal cache reads", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 12_000, createdAt: "2026-04-28T00:00:00Z" }),
    anthropicProviderRound({ round: 2, cacheRead: 11_600, createdAt: "2026-04-28T00:00:30Z" })
  ]);

  assert.equal(diagnostics.rounds[0].cache_break_classification, "normal_cache_read");
  assert.equal(diagnostics.rounds[1].cache_break_classification, "normal_cache_read");
  assert.equal(diagnostics.rounds[1].cache_read_drop_tokens, 400);
  assert.equal(diagnostics.summary.cache_break_classification_counts.normal_cache_read, 2);
});

test("summarizeHolonTokenOptimization classifies missing segment baseline as true warmup", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 0 }),
    anthropicProviderRound({ round: 2, cacheRead: 0 })
  ]);

  assert.equal(diagnostics.rounds[0].cache_break_classification, "true_warmup");
  assert.equal(diagnostics.rounds[1].cache_break_classification, "true_warmup");
  assert.equal(
    diagnostics.rounds[1].cache_break_reason,
    "no positive cache-read baseline in stable-shape segment"
  );
  assert.equal(diagnostics.summary.cache_break_classification_counts.true_warmup, 2);
});

test("summarizeHolonTokenOptimization reports non-material zero cache reads accurately", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 1_000 }),
    anthropicProviderRound({ round: 2, cacheRead: 0 })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "non_material_zero_cache_read");
  assert.equal(
    diagnostics.rounds[1].cache_break_reason,
    "cache read is zero without a material drop from the baseline"
  );
});

test("summarizeHolonTokenOptimization classifies matching stable-prefix cache drops as server-side", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      createdAt: "2026-04-28T00:00:00Z",
      stablePrefixFingerprint: "stable-x"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      createdAt: "2026-04-28T00:00:20Z",
      stablePrefixFingerprint: "stable-x"
    })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "likely_server_side_drop");
  assert.equal(diagnostics.rounds[1].stable_prefix_matches_cache_baseline, true);
  assert.equal(
    diagnostics.rounds[1].cache_break_reason,
    "provider-visible stable prefix matched the positive cache-read baseline"
  );
});

test("summarizeHolonTokenOptimization classifies stable-prefix cache drop as likely server-side", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      createdAt: "2026-04-28T00:00:00Z",
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      createdAt: "2026-04-28T00:00:20Z",
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "likely_server_side_drop");
  assert.equal(diagnostics.rounds[1].contains_prior_known_cacheable_prefix, true);
  assert.equal(
    diagnostics.rounds[1].anthropic_cache.cache_breakpoints[0].seen_in_previous_comparable_rounds,
    true
  );
  assert.equal(diagnostics.rounds[1].request_shape_changed, false);
  assert.equal(diagnostics.rounds[1].last_positive_cache_read_input_tokens, 18_000);
  assert.equal(diagnostics.rounds[1].cache_read_drop_tokens, 18_000);
  assert.equal(diagnostics.summary.likely_server_side_cache_break_rounds, 1);
});

test("summarizeHolonTokenOptimization classifies client prefix cache drops", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 18_000, systemHash: "system-a" }),
    anthropicProviderRound({ round: 2, cacheRead: 0, systemHash: "system-b" })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "client_prefix_changed");
  assert.equal(diagnostics.rounds[1].stable_shape_segment_id, 1);
  assert.equal(diagnostics.rounds[1].request_shape_changed, true);
  assert.deepEqual(diagnostics.rounds[1].shape_changed_fields, ["anthropic_cache.system_hash"]);
});

test("summarizeHolonTokenOptimization explains stable-prefix component changes", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      stablePrefixFingerprint: "stable-a"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      stablePrefixFingerprint: "stable-b",
      stablePrefixComponents: [
        { name: "contract", fingerprint: "contract" },
        { name: "request_controls", fingerprint: "controls" },
        { name: "system", fingerprint: "system" },
        { name: "tools", fingerprint: "tools-changed" },
        { name: "history_prefix", fingerprint: "history" }
      ]
    })
  ]);

  assert.equal(diagnostics.rounds[0].stable_prefix.status, "available");
  assert.deepEqual(diagnostics.rounds[1].stable_prefix_changed_components, ["tools"]);
  assert.equal(diagnostics.rounds[1].cache_break_classification, "client_prefix_changed");
  assert.equal(
    diagnostics.rounds[1].shape_changed_fields.includes("stable_prefix.components.tools"),
    true
  );
  assert.equal(diagnostics.summary.stable_prefix_available_rounds, 2);
  assert.equal(diagnostics.summary.stable_prefix_changed_rounds, 1);
});

test("summarizeHolonTokenOptimization keeps missing stable-prefix diagnostics unavailable", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 18_000 })
  ]);

  assert.equal(diagnostics.rounds[0].stable_prefix.status, "unavailable");
  assert.equal(diagnostics.rounds[0].stable_prefix_changed_components, null);
  assert.equal(diagnostics.summary.stable_prefix_available_rounds, 0);
});

test("summarizeHolonTokenOptimization reports client prefix changes inside a segment", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      breakpointStability: "stable",
      breakpointHash: "breakpoint-hash-b",
      prefixFingerprint: "prefix-b"
    })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "client_prefix_changed");
  assert.equal(diagnostics.rounds[1].request_shape_changed, true);
  assert.deepEqual(diagnostics.rounds[1].shape_changed_fields, ["anthropic_cache.cache_breakpoints"]);
  assert.equal(diagnostics.summary.client_shape_changed_cache_break_rounds, 1);
});

test("summarizeHolonTokenOptimization classifies compression epoch cache drops as expected", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 18_000, compressionEpoch: 1 }),
    anthropicProviderRound({ round: 2, cacheRead: 0, compressionEpoch: 2, systemHash: "system-after-compact" })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "expected_after_compaction");
  assert.equal(diagnostics.rounds[1].request_shape_changed, true);
  assert.equal(diagnostics.rounds[1].shape_changed_fields.includes("compression_epoch"), true);
  assert.equal(diagnostics.summary.expected_after_compaction_cache_break_rounds, 1);
});

test("summarizeHolonTokenOptimization classifies elapsed cache drops as TTL possible", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 18_000, createdAt: "2026-04-28T00:00:00Z" }),
    anthropicProviderRound({ round: 2, cacheRead: 0, createdAt: "2026-04-28T00:06:00Z" })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "ttl_possible");
  assert.equal(diagnostics.rounds[1].previous_round_elapsed_ms, 360_000);
  assert.equal(diagnostics.summary.ttl_possible_cache_break_rounds, 1);
});

test("summarizeHolonTokenOptimization tracks positive-read to zero-read to continued miss", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    }),
    anthropicProviderRound({
      round: 3,
      cacheRead: 0,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "likely_server_side_drop");
  assert.equal(diagnostics.rounds[2].cache_break_classification, "continued_cache_miss");
  assert.equal(diagnostics.rounds[2].last_positive_cache_read_round, 1);
  assert.equal(diagnostics.summary.continued_cache_miss_rounds, 1);
});

test("summarizeHolonTokenOptimization classifies moving tail breakpoint non-reuse", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({ round: 1, cacheRead: 18_000, prefixFingerprint: "tail-a" }),
    anthropicProviderRound({ round: 2, cacheRead: 0, prefixFingerprint: "tail-b" })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "moving_breakpoint_non_reuse");
  assert.equal(diagnostics.rounds[1].contains_prior_known_cacheable_prefix, false);
  assert.equal(diagnostics.summary.moving_breakpoint_non_reuse_rounds, 1);
});

test("summarizeHolonTokenOptimization classifies context-management-applied cache invalidation", () => {
  const diagnostics = summarizeHolonTokenOptimization([
    anthropicProviderRound({
      round: 1,
      cacheRead: 18_000,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    }),
    anthropicProviderRound({
      round: 2,
      cacheRead: 0,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a",
      appliedEdits: [
        {
          type: "clear_tool_uses_20250919",
          cleared_tool_uses: 7,
          cleared_input_tokens: 4096,
          beta_field: "preserved"
        }
      ]
    }),
    anthropicProviderRound({
      round: 3,
      cacheRead: 12_000,
      breakpointStability: "stable",
      prefixFingerprint: "prefix-a"
    })
  ]);

  assert.equal(diagnostics.rounds[1].cache_break_classification, "context_management_applied");
  assert.equal(diagnostics.rounds[1].context_management.applied_edit_count, 1);
  assert.equal(diagnostics.rounds[1].context_management.applied_edits[0].beta_field, "preserved");
  assert.equal(diagnostics.summary.context_management_applied_rounds, 1);
  assert.equal(diagnostics.summary.context_management_cleared_tool_uses, 7);
  assert.equal(diagnostics.summary.context_management_cleared_input_tokens, 4096);
  assert.equal(diagnostics.summary.cache_miss_with_context_management_applied_rounds, 1);
  assert.equal(diagnostics.summary.cache_recovered_after_context_management_applied_rounds, 1);
});

function anthropicProviderRound({
  round,
  cacheRead,
  createdAt = "2026-04-28T00:00:00Z",
  systemHash = "system-hash",
  toolsHash = "tools-hash",
  contextHash = "context-hash",
  breakpointHash = "breakpoint-hash",
  prefixFingerprint = "prefix-fingerprint",
  breakpointStability = "conversation_tail",
  workingMemoryRevision = 4,
  compressionEpoch = 1,
  appliedEdits = [],
  stablePrefixFingerprint = null,
  stablePrefixComponents = null
}) {
  return {
    kind: "provider_round_completed",
    created_at: createdAt,
    data: {
      round,
      input_tokens: 25_000,
      output_tokens: 100,
      provider_cache_usage: {
        read_input_tokens: cacheRead,
        creation_input_tokens: 100
      },
      working_memory_revision: workingMemoryRevision,
      compression_epoch: compressionEpoch,
      provider_request_diagnostics: {
        request_lowering_mode: "prompt_cache_blocks",
        ...(stablePrefixFingerprint
          ? {
              stable_prefix: {
                schema_version: 1,
                algorithm: "sha256",
                full_request_fingerprint: `full-${round}`,
                stable_prefix_fingerprint: stablePrefixFingerprint,
                history_prefix_items: 3,
                dynamic_tail_items: 1,
                components:
                  stablePrefixComponents ?? [
                    { name: "contract", fingerprint: "contract" },
                    { name: "request_controls", fingerprint: "controls" },
                    { name: "system", fingerprint: "system" },
                    { name: "tools", fingerprint: "tools" },
                    { name: "history_prefix", fingerprint: "history" }
                  ]
              }
            }
          : {}),
        anthropic_cache: {
          tools_count: 3,
          tools_hash: toolsHash,
          system_hash: systemHash,
          system_block_count: 2,
          estimated_system_tokens: 500,
          context_hash_by_stability: {
            stable: contextHash
          },
          conversation_message_count: 4,
          conversation_content_block_count: 6,
          cache_breakpoints: [
            {
              location: "messages[3].content[0]",
              provider_payload_path: "messages[3].content[0]",
              block_kind: "tool_result",
              stability: breakpointStability,
              estimated_prefix_tokens: 22_000,
              content_hash: breakpointHash,
              canonical_prefix_fingerprint: prefixFingerprint
            }
          ],
          tokens_before_last_breakpoint: 22_000,
          tokens_after_last_breakpoint: 0,
          automatic_cache_control_requested: false
        },
        anthropic_context_management: {
          applied_edits: appliedEdits
        }
      },
      provider_attempt_timeline: {
        attempts: [
          {
            provider: "anthropic",
            model_ref: "anthropic/claude-sonnet-4-6",
            outcome: "succeeded"
          }
        ]
      }
    }
  };
}

test("ensureBaseShaExists verifies commits in a git repo", async () => {
  const repoDir = await fs.mkdtemp(path.join(os.tmpdir(), "holon-bench-manifest-"));
  await run("git", ["init"], repoDir);
  await run("git", ["config", "user.name", "Holon Test"], repoDir);
  await run("git", ["config", "user.email", "holon@example.com"], repoDir);
  await fs.writeFile(path.join(repoDir, "README.md"), "hello\n", "utf8");
  await run("git", ["add", "README.md"], repoDir);
  await run("git", ["commit", "-m", "init"], repoDir);
  const sha = (await run("git", ["rev-parse", "HEAD"], repoDir)).trim();

  const resolved = await ensureBaseShaExists(repoDir, sha, execCommand);
  assert.equal(resolved, sha);

  await fs.rm(repoDir, { recursive: true, force: true });
});

test("checked-in suites reference valid tasks with reachable base commits", async () => {
  const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const suiteDir = path.join(repoRoot, "benchmarks", "suites");
  const suiteFiles = (await fs.readdir(suiteDir))
    .filter((entry) => entry.endsWith(".yaml"))
    .sort();

  assert.ok(suiteFiles.length > 0);

  for (const suiteFile of suiteFiles) {
    const suitePath = path.join(suiteDir, suiteFile);
    const suite = await loadBenchmarkSuite(suitePath);

    for (const taskEntry of suite.tasks) {
      const taskPath = path.resolve(path.dirname(suitePath), taskEntry);
      const manifest = await loadRealTaskManifest(taskPath);
      const taskRepoPath = resolveRepoPath(manifest.repo.local_path, path.dirname(taskPath));
      const resolved = await ensureBaseShaExists(
        taskRepoPath,
        manifest.base.sha,
        execCommand
      );

      assert.equal(resolved, manifest.base.sha);
    }
  }
});

async function execCommand(command, args, cwd, env) {
  const stdout = await run(command, args, cwd, env);
  return { stdout };
}

async function run(command, args, cwd, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
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

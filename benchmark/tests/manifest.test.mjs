import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";

import {
  ensureBaseShaExists,
  validateBenchmarkSuite,
  validateRealTaskManifest
} from "../lib/manifest.mjs";
import {
  buildHolonBenchmarkEnv,
  buildOperatorPrompt,
  classifyVerificationResult,
  collectChangedFilesFromGitOutputs,
  codexBenchmarkConfigToml,
  detectScopeViolation,
  evaluateRealTaskSuccess,
  parseClaudeCliJsonl,
  parseCodexJsonl,
  selectHolonFinalMessage,
  summarizeHolonTokenOptimization,
  tokenOptimizationEvents
} from "../run.mjs";
import {
  benchmarkLabelsForTask,
  branchNameForTask,
  prTitleForTask,
  worktreeNameForTask
} from "../lib/naming.mjs";

test("validateRealTaskManifest accepts a phase-1 manifest", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-0015-tool-guidance-registry",
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

  assert.equal(manifest.task_id, "holon-0015-tool-guidance-registry");
});

test("validateRealTaskManifest allows issue-driven tasks without operator_prompt", () => {
  const manifest = validateRealTaskManifest({
    schema_version: 1,
    task_id: "holon-0015-tool-guidance-registry",
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
        task_id: "holon-0015-tool-guidance-registry",
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
        task_id: "holon-0015-tool-guidance-registry",
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
    task_id: "holon-0015-tool-guidance-registry",
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

test("buildOperatorPrompt uses an issue-driven template with PR policy", () => {
  const prompt = buildOperatorPrompt(
    validateRealTaskManifest({
      schema_version: 1,
      task_id: "holon-0015-tool-guidance-registry",
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

test("buildOperatorPrompt preserves legacy push-only PR policy", () => {
  const prompt = buildOperatorPrompt(
    validateRealTaskManifest({
      schema_version: 1,
      task_id: "holon-0015-tool-guidance-registry",
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

test("validateBenchmarkSuite rejects unknown runner ids", () => {
  assert.throws(
    () =>
      validateBenchmarkSuite({
        suite_id: "openai-phase1",
        label_prefix: "openai-phase1",
        tasks: ["benchmarks/tasks/task.yaml"],
        runners: [{ runner_id: "not-a-runner", driver: "holon", model_ref: "openai-codex/gpt-5.3-codex-spark" }],
        pr: { submit_pr: true, draft_pr: true, push_branch: true },
        timeouts: { ci_poll_minutes: 30 }
      }),
    /runner_id must be one of/
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
    branchNameForTask("holon-0015-tool-guidance-registry", "holon-openai"),
    "bench/holon-0015-tool-guidance-registry/holon-openai"
  );
  assert.equal(worktreeNameForTask(15, "codex-openai"), "bench-0015-codex-openai");
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
                outcome: "succeeded"
              }
            ],
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
  assert.equal(diagnostics.rounds[0].request_lowering_mode, "prompt_cache_blocks");
  assert.equal(diagnostics.rounds[0].previous_tool.name, "ExecCommand");
  assert.equal(typeof diagnostics.rounds[0].previous_tool.input_bytes, "number");
  assert.equal(JSON.stringify(diagnostics).includes("secret-ish payload omitted"), false);
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
  appliedEdits = []
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

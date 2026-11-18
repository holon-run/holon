---
title: "Can Local LLMs Complete Multi-Turn Coding Agent Tasks? Benchmarking GGUF, MoE Turn Bloat, and Prompt Caching"
summary: "Models that score well on single-turn coding tests often time out inside a multi-turn coding agent with a 30k-token system contract. We tested local Ollama configurations across 8 real repository tasks on an Apple M5 Pro 64GB to measure model selection, inference backends, MoE turn bloat, prompt caching, and KV quantization."
order: 65
date: 2026-11-18
socialImage: "/assets/local-model-agent-benchmark-cover.webp"
---

# Can Local LLMs Complete Multi-Turn Coding Agent Tasks? Benchmarking GGUF, MoE Turn Bloat, and Prompt Caching

<img src="/assets/local-model-agent-benchmark-cover.webp" width="1280" height="720" alt="A coral-accented robot caught in a tangled loop of gears and hourglasses on the left, contrasted with a calm robot on the right stepping cleanly along a blue path powered by a silver desktop workstation." decoding="async" fetchpriority="high">

Ask a local Ollama model in your terminal to write quicksort or explain a stack trace, and you get an answer in seconds. Open-weight models also score well on single-turn coding benchmarks—in our own 72-task blind single-turn coding suite, `qwen3-coder:30b` scored 58/72.

Put that same model inside a multi-turn coding agent—where turn one alone ingests roughly 30k tokens of system and tool contracts, and the model has to read files, patch code, and run verification on its own—and the picture changes quickly.

To see what a 64GB unified-memory desktop can actually handle today, we ran controlled multi-turn benchmarks on an Apple M5 Pro (64GB, macOS 26.6.2) using Holon's built-in isolated benchmark suite, comparing local Ollama configurations against a cloud baseline (`deepseek-v4-flash`).

Up front, several common pieces of local tuning advice did not hold up:

- **For the same 27B model, GGUF outperforms MLX on long-prompt agents**: `qwen3.8:27b` timed out repeatedly under Ollama's MLX runner because cold first-turn prefill on long prompts took too long. Switching the same model to `llama.cpp`/GGUF raised end-to-end passes from 3/8 to 6/8–7/8, reaching 8/8 on underlying unit verification.
- **Single-turn speed does not mean multi-turn speed**: Among 3B-active MoE models, `gemma4:26b` (~3.8B active) wrapped up in 8.5 turns on average (159s/task, 0 timeouts). Meanwhile, `qwen3-coder:30b` (30.5B total, 3.3B active)—which scored 58/72 on single-turn coding—generated tokens fast per step, yet stretched across 23.2–36.9 turns and roughly 1M+ cumulative input tokens per task, timing out on half the suite (3/8–4/8 pass).
- **Daemon keep-alive settings do not fix first-turn prefill, and cross-task prompt caching depends on architecture**: In-turn dialogue appends already hit a 93% cache rate by default, so `OLLAMA_KEEP_ALIVE=-1` only saves a few seconds of model loading. Across different tasks, full-attention models (`qwen3moe`) hit 11k–14k of shared system prefix but still compute 21k–24k of task-specific context fresh, while `qwen3.8:27b` (`qwen35` hybrid linear/full attention) triggers full prompt reprocessing across tasks in current `llama.cpp`.
- **Skip `q8_0` KV cache quantization on Metal**: Ollama 0.32.15 already enables FlashAttention on Metal via `--flash-attn auto`, so setting `OLLAMA_FLASH_ATTENTION=1` changes nothing. Adding `OLLAMA_KV_CACHE_TYPE=q8_0` adds per-token dequantization overhead and slowed `qwen3-coder:30b` decode by 28% (27 → 19.5 tok/s).

---

## Overall Results and Model Selection

### 8-Task Benchmark Summary

| Model Configuration | Final Pass | Unit Verify (`verify`) | Avg Duration / Task | Avg Turns | Avg Cumulative Input Tokens | Timeouts / Errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `deepseek-v4-flash` (cloud baseline) | 8/8 | 8/8 | 21s | 5.6 | 30k | 0 |
| **`qwen3.8:27b-gguf` (27B Dense, `Q4_K_M`)** | **6/8–7/8** | **8/8** | **422s–437s** | **5.9–6.6** | **218k–248k** | **1–2** |
| `gemma4:26b` (MoE-A3.8B) | 5/8 | 5/8 | 159s | 8.5 | 325k | 0 |
| `qwen3-coder:30b` (MoE-A3B) | 3/8–4/8 | 7/8 | 561s–583s | 23.2–36.9 | 962k–1,588k | 4–5* |
| `qwen3.8:27b-mlx` | 3/8 | 4/8 | 552s | 6.5 | 245k | 5 |
| `qwen3.5:0.8b-mlx` | 0/8 | 0/8 | 28s | 8.5 | 327k | 0 |

> *Note: Ranges reflect variance across repeated runs of the same configuration. In `qwen3-coder:30b`'s timeouts, 3 tasks (`callback-refactor`, `fix-multi-file-config-merger`, and `task-inspection-subagent-status`) actually passed underlying unit tests (`verify passed`), then burned through their budget re-checking files instead of finishing.*

### What to Pick on a 64GB Desktop

| Use Case | Recommended Configuration | Observed Behavior |
| --- | --- | --- |
| Primary coding work (first-try accuracy and clean wrap-up) | `qwen3.8:27b` (`ggml-org` `Q4_K_M` GGUF, ~19GB) | 6/8–7/8 pass, ~6 turns to converge, 5–10 min per task |
| Everyday quick edits and fast responses | `gemma4:26b` (MoE, ~3.8B active) | 5/8 pass with 0 timeouts, 159s/task average, fastest turnaround |
| Long asynchronous background tasks | `qwen3-coder:30b` (17GB GGUF) | Strong code repair capability (`verify` 7/8), but high turn counts; allow 10–15+ min budgets |
| Avoid for coding agents | `qwen3.8:27b-mlx`, `qwen3.5:0.8b` | MLX suffers severe long-prompt prefill latency; 0.8B scored 0/8 and cannot hold tool or patch contracts |

---

## Test Environment and Methodology

- **Hardware and runtime**: Apple M5 Pro (64GB unified memory, Metal GPU backend), macOS 26.6.2, Ollama 0.32.15 (default `http://127.0.0.1:11434/v1`). Every configuration ran serially to avoid memory-bandwidth contention.
- **Isolated harness**: Executed via `benchmark/run.mjs --runner holon`, with each task assigned a clean `holon-home` and temporary workspace. Test exit codes and `success_criteria` were scored mechanically by the outer harness, with expected answers hidden from the agent. Tool execution traces were verified to rule out false passes from idle runs.
- **8 real repository tasks**: Covered code-path tracing (`analyze-pipeline`), multi-file config merging (`fix-multi-file-config-merger`) and cross-turn follow-up (`followup-after-multifile-fix`), retry after failed verification (`failed-verification-retry`), constrained modifications (`coupon-constrained-fix`, `callback-refactor`), invoice pipeline debugging (`invoice-pipeline-bug`), and parent/subagent status inspection (`task-inspection-subagent-status`). Task budgets were set to **300s / 600s / 900s** by complexity.

---

## Inference Backend Comparison: Switching to GGUF Lifts 27B Dense from 3/8 to 7/8

Under the MLX runner, `qwen3.8:27b-mlx` passed only 3/8 tasks. The remaining 5 failed on time (4 timeouts, 1 failing to finish turn one). Daemon logs showed that on a ~30k-token system prompt, Ollama's MLX runner spent roughly **235 seconds** on cold first-turn prefill alone—burning nearly half the task budget before the model read a single file.

To isolate backend execution from quantization differences, we downloaded `ggml-org`'s official `Q4_K_M` GGUF weights (~19GB, 27B Dense), imported them into Ollama via a local `Modelfile` using the `llama.cpp` backend, and re-ran the identical suite and budgets.

All 4 tasks that had timed out under MLX (`analyze-pipeline`, `coupon-constrained-fix`, `followup-after-multifile-fix`, and `invoice-pipeline-bug`) passed. Total pass rate climbed from 3/8 to **7/8**, with `verify` hitting 8/8. Across repeated runs, aside from an occasional slow opening turn (`invoice-pipeline-bug` once spiked to 324s on turn one), it held steady at **6/8–7/8**.

Telemetry shows the model's reasoning behavior barely changed:

- Average turns: 6.6 (GGUF) vs. 6.5 (MLX)
- Average input tokens: 248k (GGUF) vs. 245k (MLX)
- Average output tokens: 2.2k (GGUF) vs. 4.2k (MLX)

Turn counts and input volumes matched closely. Switching formats did not make the model smarter—`llama.cpp`/GGUF simply cut first-turn 30k prefill latency enough for capable solutions to finish before the clock ran out.

The only consistent timeout was `task-inspection-subagent-status`: given a tight 300s budget, two turns took 303s, dominated by first-turn 30k prefill. Short-budget tasks remain highly sensitive to initial prefill delay.

---

## Architecture and Turn Count: Why a 58/72 Single-Turn 30B MoE Stalls at 4/8

`qwen3-coder:30b` is a 17GB MoE coding model (30.5B total parameters, ~3.3B active per token). It scored 58/72 on our single-turn coding blind test and streams tokens quickly, yet in the isolated multi-turn benchmark it passed only 3/8–4/8 tasks, ballooning to **23.2–36.9 turns** and 962k–1,588k input tokens per task.

Logs showed a clear failure mode: **fixing the code, then failing to stop**. In one 5-timeout run, `callback-refactor` (72 turns), `fix-multi-file-config-merger` (41 turns), and `task-inspection-subagent-status` (7 turns) had already passed unit verification (`verify passed`). Instead of delivering the result, the model kept issuing read-only shell commands to double-check files until the timeout hit.

The second issue was **micro-stepping**. `qwen3.8:27b-gguf` (27B Dense) inspects the relevant files and applies a direct fix in 5.9–6.6 turns. With 3B active parameters, `qwen3-coder:30b` steps fast, but across multi-file contexts it fragments reads into tiny chunks, probes patches tentatively, and reworks them—multiplying turn count by 4–6×.

This is not an inherent flaw of every MoE model: `gemma4:26b` (also 3B–4B active) averaged 8.5 turns and 159s per task. The gap comes from `qwen3-coder:30b`'s tendency to hesitate and over-verify inside an agent loop. Because every extra turn replays the accumulated conversation history, fast per-step decoding could not offset 4×–6× more turns.

---

## Prompt Caching and Decode Tuning: What Helps and What Hurts

With first-turn 30k prefill dominating runtimes, we tested daemon keep-alive flags, cross-task prefix caching, and low-level quantization switches on the same hardware.

### 1. `OLLAMA_KEEP_ALIVE=-1` Saves Cold Model Loads, Not First-Turn Prefill

Setting `OLLAMA_KEEP_ALIVE=-1 OLLAMA_NUM_PARALLEL=1 OLLAMA_MAX_LOADED_MODELS=1` left average 8-task durations nearly unchanged (`qwen3.8:27b-gguf` 422s vs. 437s; `qwen3-coder:30b` 561s vs. 583s). Breaking down 403 prompt eval calls explained why:

- **Within-task turns already hit 93% cache**: 373 of 403 requests had deltas ≤1k tokens and prefilled in 0.02s–0.5s, confirming Ollama's default daemon already caches appended dialogue properly.
- **Explicit keep-alive only saves 5–12 seconds of cold model loading**: Negligible inside a 400-second multi-turn task.

### 2. Cross-Task Prefix Caching Today: Architecture Split and Task-Context Weight

Even when consecutive tasks share an identical system prompt (~83% of the prompt text, over 56k characters), cross-task prefix caching in current `llama.cpp` (Ollama 0.32.15) hits two practical limits:

- **`qwen3-coder:30b` (`qwen3moe` full attention): Cross-task cache hits, but non-shared context dilutes the gain.**
  After the initial cold start, the next 7 task openings consistently hit `cached n_tokens = 11,378–14,306` in daemon logs (depending on whether adjacent tasks diverged around ~11.4k or ~14.2k tokens), cutting median opening-turn latency to ~121s. Even so, out of ~35.5k opening tokens, **21k–24k are task-specific instructions and workspace state** that must still be computed fresh. Combined with dozens of decode turns afterward, total 8-task provider time stayed at 4,296s—saving ~10s on turn one improved end-to-end runtime by less than 5%.
- **`qwen3.8:27b-gguf` (`qwen35` hybrid linear/full attention): Checkpoints work within a task, but task switches trigger full reprocessing.**
  `Qwen3.8-27B` alternates 48 Gated DeltaNet linear-attention layers with 16 full-attention layers. Between turns of the same task, it restores cleanly from checkpoints (`restored context checkpoint n_past=35.6k+`). Once the harness switches to a different task and the prompt diverges, all 8 task openings in `llama.cpp` logs hit `forcing full prompt re-processing due to lack of cache data`—hybrid/recurrent memory layers cannot yet truncate and resume from a partial shared prefix. Median opening-turn latency stayed at ~156s.

| Model (Inference Architecture) | Cross-Task Opening Cache Hit | Median Opening Turn | 8-Task Opening Sum / Total Provider Time |
| --- | --- | ---: | ---: |
| `qwen3-coder:30b` (`qwen3moe` full attention) | 11,378–14,306 tokens | ~121s | 1,237s / 4,296s (29%) |
| `qwen3.8:27b-gguf` (`qwen35` hybrid linear/full attention) | 0 (full reprocessing) | ~156s | 1,558s / 3,097s (50%) |

### 3. FlashAttention and `q8_0` KV Cache Slowed Decode Down

Many local tuning guides recommend setting `OLLAMA_FLASH_ATTENTION=1` alongside `OLLAMA_KV_CACHE_TYPE=q8_0`. Using a controlled **40k prompt + 512-token decode** probe, we compared default settings (`base`) against enabling both flags (`fakv`):

| Model | Prefill Speed (`base` → `fakv`) | Decode Speed (`base` → `fakv`) |
| --- | ---: | ---: |
| `qwen3-coder:30b` | 266 → 212 tok/s (**-20%**) | 27 → 19.5 tok/s (**-28%**) |
| `qwen3.8:27b-gguf` | 221.6 → 195.8 tok/s (**-12%**) | 11 → 10.2 tok/s (**-7%**) |

The takeaway is straightforward:

1. On Metal in Ollama 0.32.15, `--flash-attn auto` is already on by default for supported architectures, so `OLLAMA_FLASH_ATTENTION=1` adds no benefit.
2. The only change came from `q8_0` KV cache quantization. While it saves memory, it adds per-token dequantization overhead during generation, cutting `qwen3-coder:30b` decode throughput by nearly 30%. On a 64GB machine running a 27B–30B `Q4_K_M` model, memory is not the bottleneck—skip KV quantization.

---

## Local GGUF Import and Configuration Pitfalls

Running `ollama run qwen3.8:27b` on Apple Silicon defaults to the MLX variant. If you import GGUF weights into Ollama 0.32.15 yourself, watch out for three issues we hit directly:

1. **Do not pipe a `Modelfile` to `ollama create`**: On Ollama 0.32.15, `ollama create <name> -f -` reading from stdin fails silently with exit code `0` without creating a usable model. Write an explicit `Modelfile` to disk (e.g., `FROM ./Qwen3.8-27B-Q4_K_M.gguf`) and run `ollama create qwen3.8:27b-gguf -f ./Modelfile`.
2. **Stick to standard `ggml-org` `Q4_K_M` GGUF files for manual imports**: Some third-party dynamic quantization packs (such as the `unsloth` quant we tested) failed Ollama's format validation. Standard `ggml-org` `Q4_K_M` (~19GB) worked reliably; if you prefer pulling from the Ollama library directly, specify `qwen3.8:27b-q4_K_M` (the GGUF tag) rather than the bare tag.
3. **Keep Ollama defaults on 64GB Apple Silicon**: Leave `--flash-attn auto` and FP16 KV cache as-is. Skip `OLLAMA_KV_CACHE_TYPE=q8_0` (7%–28% slower in our probes), and do not expect `OLLAMA_KEEP_ALIVE=-1` to accelerate cross-task first-turn prefill.

Once your local model is ready, follow [Run Private Agents in Offline and Self-Hosted Environments: From Ollama to Private Search and Local Toolchains](../guides/run-offline-private-agent.md) to lock down external network policies, add self-hosted search and local tools, and verify end-to-end private execution. For multi-cloud failover or per-agent routing, see [Configure and Switch Models: From Local Ollama to Multi-Cloud Failover](../guides/configure-models.md), [Work in a Project Workspace or Git Worktree](../guides/work-with-workspaces.md), and [Manage Agent Memory](../guides/manage-agent-memory.md).

---

## Conclusion

On a 64GB unified-memory desktop paired with `llama.cpp`/GGUF, a local 27B Dense model (`qwen3.8:27b-gguf`) already clears 6/8 to 7/8 of realistic multi-turn coding tasks. The remaining bottleneck for local agents is no longer tweaking Ollama environment variables—it comes down to whether models avoid wasted verification loops and wrap up cleanly, and how far we can shrink the 20k+ tokens of unshared per-task context.

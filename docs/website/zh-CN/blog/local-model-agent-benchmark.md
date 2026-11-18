---
title: "本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测"
summary: "单轮盲测能写代码，接进带 30k 系统契约的多轮编码 Agent 却经常超时。我们在 Apple M5 Pro 64GB 上用 8 个真实仓库任务做了一组对照实测，给出模型选型结论与推理后端、MoE 轮次膨胀、Prompt Cache 及 KV 量化的真实表现。"
order: 65
date: 2026-11-18
---

# 本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测

在本地终端里用 Ollama 写一段快排，或者解释一段报错，几秒钟就能出结果。单轮代码榜单上，不少开源模型分数也不低（在我们自己的 72 题单轮代码盲测里，`qwen3-coder:30b` 拿到了 58/72）。

但把同一个模型放进多轮编码 Agent（首轮就要吞下约 30k Token 的系统与工具上下文，并自主完成读代码、改补丁和跑验证），情况很快就不一样了。

为了确认 64GB 统一内存的桌面设备现在到底能跑成什么样，我们在 Apple M5 Pro（64GB，macOS 26.6.2）上用 Holon 内置的隔离基准套件，对几组本地 Ollama 配置和云端对照模型 `deepseek-v4-flash` 做了多轮真实任务实测。

先说结论，有几条常见的本地优化经验并不成立：

- **同款 27B 模型，GGUF 比 MLX 更适合长提示词 Agent**：`qwen3.8:27b` 在 Ollama MLX runner 下频频超时，是长提示词首轮 prefill 太慢导致的。切到 `llama.cpp`/GGUF 路径后，同一款模型的通过率从 3/8 升到了 6/8–7/8，底层单测一度达到 8/8 全过。
- **单轮快不等于多轮快**：同为激活参数 3B 级的 MoE 模型，`gemma4:26b`（约 3.8B 激活）平均 8.5 轮即可收工（159s/任务、0 超时）；而单轮代码测到 58/72 的 `qwen3-coder:30b`（30.5B 总参数、3.3B 激活）虽然单步吐字快，多轮却要走 23.2–36.9 轮，单任务累计输入接近甚至超过百万 Token，约一半任务超时（3/8–4/8）。
- **调常驻参数救不了跨任务首轮 Prefill，跨任务 Prompt Cache 也受架构限制**：轮内对话追加本来就有 93% 的缓存命中率，`OLLAMA_KEEP_ALIVE=-1` 只能省下几秒冷加载。跨任务场景下，全注意力架构（`qwen3moe`）虽能命中 11k–14k 公共系统前缀，但其余 21k–24k 任务上下文仍要现算；而 `qwen3.8:27b`（`qwen35` 混合线性/全注意力架构）在当前 `llama.cpp` 下跨任务仍会触发全量重算。
- **Metal 后端别开 `q8_0` KV 量化**：Ollama 0.32.15 在 Metal 后端默认通过 `--flash-attn auto` 自动开启了 FlashAttention，手动设置 `OLLAMA_FLASH_ATTENTION=1` 不起额外作用；加上 `OLLAMA_KV_CACHE_TYPE=q8_0` 反而因为逐 Token 反量化开销，把 `qwen3-coder:30b` 的解码速度拉低 28%（27 → 19.5 tok/s）。

---

## 整体结果与选型建议

### 8 任务实测汇总

| 模型配置 | 最终通过 | 单测验证 (`verify`) | 平均耗时/任务 | 平均轮次 | 平均累计输入 Token | 超时/报错数 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `deepseek-v4-flash`（云端对照） | 8/8 | 8/8 | 21s | 5.6 | 30k | 0 |
| **`qwen3.8:27b-gguf`（27B Dense，`Q4_K_M`）** | **6/8–7/8** | **8/8** | **422s–437s** | **5.9–6.6** | **218k–248k** | **1–2** |
| `gemma4:26b`（MoE-A3.8B） | 5/8 | 5/8 | 159s | 8.5 | 325k | 0 |
| `qwen3-coder:30b`（MoE-A3B） | 3/8–4/8 | 7/8 | 561s–583s | 23.2–36.9 | 962k–1,588k | 4–5* |
| `qwen3.8:27b-mlx` | 3/8 | 4/8 | 552s | 6.5 | 245k | 5 |
| `qwen3.5:0.8b-mlx` | 0/8 | 0/8 | 28s | 8.5 | 327k | 0 |

> *注：区间来自同一配置的多轮复测波动；`qwen3-coder:30b` 在超时任务中，有 3 个（`callback-refactor`、`fix-multi-file-config-merger`、`task-inspection-subagent-status`）其实已经跑通底层单测（`verify passed`），但因为后续多轮反复复查没及时收尾，最终拖过预算。*

### 64GB 桌面环境怎么选

| 使用场景 | 建议配置 | 实测参考 |
| --- | --- | --- |
| 主力改代码（看重一次做对和收尾） | `qwen3.8:27b`（`ggml-org` `Q4_K_M` GGUF，约 19GB） | 通过 6/8–7/8，平均约 6 轮收敛，单任务 5–10 分钟 |
| 日常小改和快速响应 | `gemma4:26b`（MoE，约 3.8B 激活） | 通过 5/8 且 0 超时，平均 159s/任务，响应最快 |
| 后台异步长任务 | `qwen3-coder:30b`（17GB GGUF） | 代码修复底子不错（`verify` 7/8），但轮次偏多，超时预算建议放宽到 10–15 分钟以上 |
| 不建议用于编码 Agent | `qwen3.8:27b-mlx`、`qwen3.5:0.8b` 等超小模型 | 前者受长提示词 prefill 拖累严重；0.8B 量级在 8 个任务里 0/8，守不住工具和补丁契约 |

---

## 测试环境与评估口径

- **硬件与推理端**：Apple M5 Pro（64GB 统一内存，Metal GPU 后端）、macOS 26.6.2、Ollama 0.32.15（走默认 `http://127.0.0.1:11434/v1`）。所有配置串行单跑，避免并发抢占内存带宽。
- **隔离基准**：跑在仓库自带的 `benchmark/run.mjs --runner holon` 上，每个任务分配独立的 `holon-home` 和临时工作区。测试脚本退出码和交付检查（`success_criteria`）放在外层 Harness 机械判分，受测 Agent 看不到预期答案；同时强制校验工具调用痕迹，防止空转混过只读校验。
- **8 个真实仓库任务**：覆盖代码链路分析（`analyze-pipeline`）、多文件配置合并修复（`fix-multi-file-config-merger`）及跨轮追问（`followup-after-multifile-fix`）、测试失败重试（`failed-verification-retry`）、带约束修改（`coupon-constrained-fix`、`callback-refactor`）、发票流水线排障（`invoice-pipeline-bug`）和主从子 Agent 状态巡检（`task-inspection-subagent-status`）。按复杂度设 **300s / 600s / 900s** 三档超时预算。

---

## 推理后端对照：换到 GGUF 后，27B Dense 从 3/8 升到 7/8

在 MLX runner 下测试时，`qwen3.8:27b-mlx` 只拿了 3/8，剩下 5 个任务全部倒在时间上（4 个超时，1 个首轮没跑完）。查看日志才发现，面对约 30k Token 的系统提示词，Ollama 当时的 MLX runner 首轮冷启动 prefill 就要吃掉约 **235 秒**。模型还没开始读文件，一半预算已经没了。

为了排除量化差异，我们取 `ggml-org` 官方的 `Q4_K_M` GGUF 权重（约 19GB，27B Dense），写本地 `Modelfile` 导进 Ollama 走 `llama.cpp` 后端，在相同任务与预算下重跑。

结果之前在 MLX 下超时的 4 个任务（`analyze-pipeline`、`coupon-constrained-fix`、`followup-after-multifile-fix`、`invoice-pipeline-bug`）全部通过，总分从 3/8 升到 **7/8**，底层单测 `verify` 更是 8/8 全过。复测中除偶发长开场波动（`invoice-pipeline-bug` 单次首轮异常拉长到 324 秒而超时）外，稳定维持在 **6/8–7/8**。

对比两边遥测数据，模型行为其实完全没变：

- 平均轮次：GGUF 版 6.6 轮，MLX 版 6.5 轮
- 平均输入 Token：GGUF 版 248k，MLX 版 245k
- 平均输出 Token：GGUF 版 2.2k，MLX 版 4.2k

轮次和输入量几乎一致，说明换格式并没有让模型变聪明，纯粹是 `llama.cpp`/GGUF 压低了首轮 30k prefill 耗时，原本能做对的题来得及在超时前交卷。

唯一稳定超时的任务是 `task-inspection-subagent-status`：预算只有 300 秒，模型两轮跑了 303 秒，其中首轮 30k prefill 占了绝大部分时间。短预算任务对首轮延迟非常敏感。

---

## 架构与轮次对照：单轮 58/72 的 30B MoE 为什么卡在 4/8

`qwen3-coder:30b` 是个 17GB 的 MoE 代码模型（总参数 30.5B，每 Token 激活约 3.3B）。单轮代码盲测它拿过 58/72，单步吐字也很快，但放进隔离多轮基准后，通过率只有 3/8–4/8，平均轮次飙到 **23.2–36.9 轮**，单任务平均输入 Token 高达 962k–1,588k。

日志里最突出的问题是**代码改对了却不会收尾**。在一轮 5 个超时任务里，`callback-refactor`（跑了 72 轮）、`fix-multi-file-config-merger`（41 轮）和 `task-inspection-subagent-status`（7 轮）其实已经把单测跑通了，但模型没有及时结束交付，而是继续发只读命令反复确认文件，一直拖到预算耗尽。

另一个问题是**单步拆得太碎**。`qwen3.8:27b-gguf`（27B Dense）看清文件后直接动手，平均 5.9–6.6 轮收工；`qwen3-coder:30b` 只有 3B 激活参数，单步推理快，但面对多文件上下文时经常把读取拆成很多小步、反复试探补丁再返工，把总轮次拉长到 4–6 倍。

这并不意味着所有 MoE 模型都会轮次膨胀——同为 3B–4B 级激活参数的 `gemma4:26b` 平均只走 8.5 轮（159s/任务）。差距主要在于 `qwen3-coder:30b` 在多轮 Agent 循环里更容易犹豫和反复验证，而每多一轮，前面累积的历史上下文就得跟着再走一遍，单步吐字快最终并没有换成更快的任务收敛。

---

## Prompt Cache 与解码调优：哪些有效，哪些没用

首轮 30k prefill 占了不少时间，我们在同硬件上针对 Daemon 常驻、跨任务前缀缓存和底层量化开关做了单项实测。

### 1. 常驻参数 `OLLAMA_KEEP_ALIVE=-1` 只省冷加载，不省首轮 Prefill

加上 `OLLAMA_KEEP_ALIVE=-1 OLLAMA_NUM_PARALLEL=1 OLLAMA_MAX_LOADED_MODELS=1` 后，8 任务平均耗时和默认配置几乎持平（`qwen3.8:27b-gguf` 422s vs 437s，`qwen3-coder:30b` 561s vs 583s）。拆开 403 次 Prompt Eval 请求看：

- **同一任务轮内 Cache 原本就有 93% 命中率**：增量 ≤1k Token 的请求占了 373 次（93%），每次 prefill 只要 0.02s–0.5s，说明默认 Ollama Daemon 在对话追加时缓存一直正常。
- **显式常驻只省下 5–12 秒模型冷加载**：对 400 秒级别的多轮任务感知不到。

### 2. 跨任务前缀缓存的现状：架构分化与任务上下文占比

即便不同任务共享同一套系统提示词（公共系统前缀占文本约 83%、超过 5.6 万字符），在当前 `llama.cpp`（Ollama 0.32.15）下连续切换任务时，跨任务前缀缓存依然受限于两点：

- **`qwen3-coder:30b`（`qwen3moe` 全注意力架构）：跨任务缓存能命中，但收益被非共享上下文稀释。**
  除首个任务冷启动外，后续 7 个任务的开轮在 Daemon 日志里能稳定命中 `cached n_tokens = 11,378–14,306`（视相邻任务分叉点落在 ≈11.4k 或 ≈14.2k），单轮开场中位耗时可压到 ~121s。但 35.5k 的开轮上下文里，仍有 **21k–24k 是每个任务独有的指令与工作区状态**，这部分每次仍需现算；再加上后续几十轮 decode 的耗时，8 任务总 Provider 耗时依然高达 4296s，首轮省出的十几秒对端到端总时长改善不到 5%。
- **`qwen3.8:27b-gguf`（`qwen35` 混合线性/全注意力架构）：轮内能续，跨任务仍强制全量重算。**
  `Qwen3.8-27B` 采用 48 层 Gated DeltaNet 线性注意力与 16 层全注意力交替的混合架构。在同一个任务的多轮之间，它可以靠检查点整段恢复（`restored context checkpoint n_past=35.6k+`）；但只要切换到新任务、上下文在分叉点出现差异，全部 8 个跨任务开轮在 `llama.cpp` 日志里都会触发全量重算（`forcing full prompt re-processing due to lack of cache data`，即混合/循环记忆层尚不支持截断到部分公共前缀再续算）。首轮开场中位耗时停在 ~156s。

| 模型（推理架构） | 跨任务开轮缓存命中 | 单轮开场中位耗时 | 8 任务开场合计 / 总 Provider 耗时 |
| --- | --- | ---: | ---: |
| `qwen3-coder:30b`（`qwen3moe` 全注意力） | 11,378–14,306 tokens | ~121s | 1237s / 4296s（29%） |
| `qwen3.8:27b-gguf`（`qwen35` 混合线性/全注意力） | 0（全部强制重算） | ~156s | 1558s / 3097s（50%） |

### 3. FlashAttention 与 KV Cache `q8_0` 实测降速

网上不少部署指南会顺手加上 `OLLAMA_FLASH_ATTENTION=1` 和 `OLLAMA_KV_CACHE_TYPE=q8_0`。我们用 **40k Prompt + 512 Token Decode** 的单变量探针直接测了默认配置（`base`）和同时开启这两项环境变量（`fakv`）的速率：

| 模型 | Prefill 速率 (`base` → `fakv`) | Decode 速率 (`base` → `fakv`) |
| --- | ---: | ---: |
| `qwen3-coder:30b` | 266 → 212 tok/s (**-20%**) | 27 → 19.5 tok/s (**-28%**) |
| `qwen3.8:27b-gguf` | 221.6 → 195.8 tok/s (**-12%**) | 11 → 10.2 tok/s (**-7%**) |

结果很明确：

1. 在 Ollama 0.32.15 的 Metal 后端上，默认传入的 `--flash-attn auto` 本就已经对受支持架构自动启用了 FlashAttention，再加 `OLLAMA_FLASH_ATTENTION=1` 没有任何效果。
2. 唯一产生变化的是 `q8_0` KV 量化。虽然能省下一部分显存，但每次生成都要做逐 Token 反量化。在 M5 Pro 统一内存上，这部分计算开销反而让 `coder30b` 的解码速率掉了将近三成。64GB 内存跑 27B–30B `Q4_K_M` 本来就够装，没必要开 KV 量化换显存。

---

## 本地 GGUF 导入与配置避坑

在 Apple Silicon 上直接运行 `ollama run qwen3.8:27b` 默认会命中 MLX 变体；如果自行拿 GGUF 往 Ollama 0.32.15 里导入，有三个坑我们在测试里都踩过：

1. **别用管道往 `ollama create` 里传 `Modelfile`**：在 Ollama 0.32.15 上实测，`ollama create <name> -f -` 从标准输入读取时会静默失败且返回退出码 `0`，表面看不出报错，实际模型没建成功。先在磁盘写好真实的 `Modelfile`（比如 `FROM ./Qwen3.8-27B-Q4_K_M.gguf`），再跑 `ollama create qwen3.8:27b-gguf -f ./Modelfile`。
2. **手动导入优先用标准 `ggml-org` `Q4_K_M` GGUF**：测试中部分第三方动态量化打包（如本次尝试导入的 `unsloth` 量化版本）未通过当前 Ollama 的格式校验，直接用 `ggml-org` 官方 `Q4_K_M`（约 19GB）最稳；若不想手动导入，也可显式拉取 Ollama 库中的 `qwen3.8:27b-q4_K_M`（GGUF 版）而非默认标签。
3. **64GB Apple Silicon 上保持 Ollama 默认参数即可**：保持默认的 `--flash-attn auto` 和 FP16 KV Cache，别加 `OLLAMA_KV_CACHE_TYPE=q8_0`（实测降速 7%–28%），也不要指望 `OLLAMA_KEEP_ALIVE=-1` 缩短跨任务首轮 prefill。

模型导进 Ollama 之后，如果想在 Holon 里设为默认模型，或者按角色路由（比如日常开发用本地 `qwen3.8:27b-gguf`，复杂评审按需切云端模型），可以参考[《配置与切换大模型：从本地 Ollama 到多云灾备》](../guides/configure-models.md)。关于工作区隔离和记忆管理，详见[《使用工作区与隔离目录》](../guides/work-with-workspaces.md)和[《管理 Agent 记忆》](../guides/manage-agent-memory.md)。

---

## 结论

在 64GB 统一内存的桌面设备上，配合 `llama.cpp`/GGUF 后端，本地 27B Dense 模型（`qwen3.8:27b-gguf`）已经能把多轮编码修复稳定跑在 6/8 到 7/8 的通过率上。目前限制本地 Agent 体验的瓶颈已经不在环境变量调参，而在两件更基础的事：模型在多轮循环里能不能少走弯路、按时收尾，以及任务级上下文（20k+ 的非共享部分）能不能进一步压缩。

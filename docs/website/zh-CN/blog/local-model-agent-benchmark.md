---
title: "本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测"
summary: "单轮盲测能写代码，接进带 30k 系统契约的多轮编码 Agent 却经常超时。我们在 Apple M5 Pro 64GB 上用 8 个真实仓库任务跑了 5 轮对照实验，记录推理后端、MoE 轮次膨胀、共享前缀缓存与 KV 量化的真实表现。"
order: 65
date: 2026-11-18
---

# 本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测

在本地终端里用 Ollama 写一段快排，或者解释一段报错，几秒钟就能出结果。单轮代码榜单上，不少开源模型分数也不低（在我们自己的 72 题单轮代码盲测里，`qwen3-coder:30b` 拿到了 58/72）。

但把同一个模型放进编码 Agent 运行时，情况很快就不一样了。

在 Holon 里做一次普通的代码修复，首轮就要吞下约 30k Token 的系统提示与工具契约（补丁格式、命令规则、工作区边界）。之后模型得在隔离目录里自己读文件、查调用链、用 `ApplyPatch` 改代码、跑 `node test.js`，失败了再看报错接着改，最后整理交付摘要。首轮 prefill 太慢、补丁格式写歪，或者反复试探收不了尾，都会直接拖到任务超时。

为了确认 64GB 统一内存的桌面设备现在到底能跑成什么样，我们在 Apple M5 Pro（64GB，macOS 26.6.2）上用 Holon 仓库内置的隔离基准套件跑了 5 轮实验。测试覆盖了几组本地 Ollama 模型配置，并用云端的 `deepseek-v4-flash` 作为对照。

跑完这 5 轮之后，有几条常见的本地优化经验并不成立：

- `qwen3.8:27b` 在 v2 频频超时是 MLX runner 的长提示词 prefill 太慢导致的。切到 `llama.cpp`/GGUF 路径后，同一款模型的通过率从 3/8 升到了 7/8，底层单测 8/8 全过。
- 单轮代码测到 58/72 的 `qwen3-coder:30b`（MoE-A3B）单步吐字很快，但在多轮任务里平均要走 23.2 到 36.9 轮，单任务输入超过百万 Token，最终有一半任务超时。
- 把 `OLLAMA_KEEP_ALIVE=-1` 配成常驻救不了首轮 prefill。轮内前缀缓存原本就命中了 93%；在 Holon 0.59.0 合入共享系统前缀前置（`SharedPrefix`）后，全注意力模型 `qwen3moe` 跨任务命中了 11k–14k 缓存，但端到端总耗时变化不大，而混合/循环记忆模型 `qwen35` 在当前 `llama.cpp` 下仍然只能整段重算。
- 在 Ollama 0.32.15 Metal 后端上，默认的 `--flash-attn auto` 本身就开了 FlashAttention，`OLLAMA_FLASH_ATTENTION=1` 不起额外作用；开启 `q8_0` KV 量化反而会因为逐 Token 反量化开销，把 `qwen3-coder:30b` 的解码速度拉低 28%（27 → 19.5 tok/s）。

---

## 我们怎么测：基准演进与环境约束

这套基准从 v1 到 v5 改过五版，主要为了解决两个问题：题目太水、或者模型偷看答案。

- **v1（早期玩具题）**：用简单的单文件修改测，几乎所有模型都能满分。翻日志才发现，有些 Agent 直接用 `rg` 在工作区里搜到了带着预期答案的测试文件。
- **v2（隔离 Harness）**：把所有评分规则（测试命令退出码和交付约束 `success_criteria`）移出 Agent 可读的工作区，放进外层 Harness；同时加上 `assertHolonFixtureModel` 检查，确保模型真做了推理和工具调用，防止 0 轮空转混过只读校验。
- **v3（推理后端与架构对照）**：在 v2 隔离环境里对比 MLX 和 GGUF/`llama.cpp`，以及 Dense 与 MoE 模型的差异。
- **v4（Daemon 常驻单变量实验）**：固定加上 `OLLAMA_KEEP_ALIVE=-1 OLLAMA_NUM_PARALLEL=1 OLLAMA_MAX_LOADED_MODELS=1`，测模型常驻到底能不能省下首轮 prefill。
- **v5（共享前缀修复与解码探针）**：在 Holon `0.59.0` 合入共享系统前缀前置（`SharedPrefix`）后重跑一轮（v5a），再用 40k Prompt 探针单独测 FlashAttention 与 `q8_0` KV 量化。

### 测试环境

硬件是 Apple M5 Pro（64GB 统一内存，Metal GPU 后端），系统为 macOS 26.6.2，Ollama 版本 0.32.15，走默认 `ollama serve` 的 OpenAI 兼容端点 `http://127.0.0.1:11434/v1`。所有配置全部串行单跑，避免并发抢占内存带宽。

测试跑在仓库自带的 `benchmark/run.mjs --runner holon` 上，每个任务分配独立的 `holon-home` 和临时工作区。8 个任务全部来自真实仓库场景：代码链路分析（`analyze-pipeline`）、多文件配置合并修复（`fix-multi-file-config-merger`）及跨轮追问（`followup-after-multifile-fix`）、测试失败重试（`failed-verification-retry`）、带约束修改（`coupon-constrained-fix`、`callback-refactor`）、发票流水线排障（`invoice-pipeline-bug`）和主从子 Agent 状态巡检（`task-inspection-subagent-status`）。

按任务复杂度设 **300s / 600s / 900s** 三档超时预算。预算内同时通过测试和交付检查记 `P`（Passed），超时记 `T`（Timeout）。

---

## 整体结果汇总

### 横向基线（v2–v4 同口径统计）

| 模型配置 | 最终通过 | 单测验证 (`verify`) | 平均耗时/任务 | 平均轮次 | 平均累计输入 Token | 超时/报错数 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `deepseek-v4-flash`（云端对照） | 8/8 | 8/8 | 21s | 5.6 | 30k | 0 |
| **`qwen3.8:27b-gguf`（v3 默认）** | **7/8** | **8/8** | **422s** | **6.6** | **248k** | **1** |
| **`qwen3.8:27b-gguf`（v4 Cache 常驻）** | **7/8** | **8/8** | **437s** | **5.9** | **218k** | **1** |
| `gemma4:26b` | 5/8 | 5/8 | 159s | 8.5 | 325k | 0 |
| `qwen3-coder:30b`（v3 默认，MoE） | 4/8 | 7/8 | 561s | 23.2 | 962k | 4* |
| `qwen3-coder:30b`（v4 Cache 常驻，MoE） | 4/8 | 7/8 | 583s | 36.9 | 1,588k | 5* |
| `qwen3.8:27b-mlx`（v2） | 3/8 | 4/8 | 552s | 6.5 | 245k | 5 |
| `qwen3.5:0.8b-mlx`（v2） | 0/8 | 0/8 | 28s | 8.5 | 327k | 0 |

> *注：`qwen3-coder:30b` 在 v4 超时的 5 个任务里，有 3 个（`callback-refactor`、`fix-multi-file-config-merger`、`task-inspection-subagent-status`）其实已经过了底层单测（`verify passed`），但因为后面拖了太多轮次没收尾，最后还是超时了。*

### 8 任务逐项明细（v3 → v4 → v5a）

符号说明：`P` = 通过，`F` = 验证失败，`T` = 超时，`E` = 执行/约束报错。

| 任务名称 | `deepseek` | `gemma4` | `qwen3.8-gguf` (v3/v4) | **`qwen3.8-gguf` (v5a)** | `coder30b` (v3) | `coder30b` (v4) | **`coder30b` (v5a)** |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `analyze-pipeline` | P | P | P / P | **P** | P | T | **E(wait)** |
| `callback-refactor` | P | P | P / P | **P** | E(wait) | T | **T** |
| `coupon-constrained-fix` | P | F | P / P | **P** | T | P | **T** |
| `failed-verification-retry` | P | P | P / P | **P** | P | P | **P** |
| `fix-multi-file-config-merger` | P | F | P / P | **P** | P | P | **P** |
| `followup-after-multifile-fix` | P | P | P / P | **P** | P | T | **P** |
| `invoice-pipeline-bug` | P | F | P / P | **T** | T | P | **T** |
| `task-inspection-subagent-status` | P | P | T / T | **T** | T | T | **T** |
| **总通过数** | **8/8** | **5/8** | **7/8 / 7/8** | **6/8** | **4/8** | **4/8** | **3/8** |

在 v5a 里，`coder30b` 有几个任务来回翻转（`followup` T→P，`coupon` P→T，`invoice` P→T，`analyze` T→E），净变化 -1 属于本地长任务的正常波动。`qwen3.8-gguf` 在 `invoice-pipeline-bug` 上超时，是因为那一轮首轮 opener 异常拉长到了 324 秒（占了整个任务 provider 时长的 93%），也不是前缀修改造成的退化。

---

## 换到 GGUF 后，27B Dense 从 3/8 升到 7/8

v2 测试的时候，`qwen3.8:27b-mlx` 只拿了 3/8，剩下 5 个任务全部倒在时间上（4 个超时，1 个首轮没跑完）。看日志才发现，面对 Holon 约 30k Token 的系统提示词，Ollama 当时的 MLX runner 首轮冷启动 prefill 就要吃掉约 **235 秒**。模型还没开始读文件，一半预算已经没了。

为了排除模型量化本身的影响，我们在 v3 拿了 `ggml-org` 官方的 `Q4_K_M` GGUF 权重（约 19GB，27B Dense），写本地 `Modelfile` 导进 Ollama 走 `llama.cpp` 后端，原样重跑 8 个任务。

结果很直接：之前在 MLX 下超时的 4 个任务（`analyze-pipeline`、`coupon-constrained-fix`、`followup-after-multifile-fix`、`invoice-pipeline-bug`）全部变成了 `P`，总分从 3/8 跳到 **7/8**，底层单测 `verify` 更是 8/8 全过。

两边的过程遥测几乎完全重合：

- 平均轮次：GGUF 版 6.6 轮，MLX 版 6.5 轮
- 平均输入 Token：GGUF 版 248k，MLX 版 245k
- 平均输出 Token：GGUF 版 2.2k，MLX 版 4.2k

轮次和输入量几乎没变，说明换了格式并没有让模型变聪明，纯粹是 `llama.cpp`/GGUF 把首轮 30k prefill 的时间压下来了，模型原本能做出来的题终于来得及在超时前交卷。

唯一没过的是 `task-inspection-subagent-status`。v3 它跑了 2 轮耗时 303 秒，卡在 300s 预算线上；v4 即使开了常驻，首轮 30k prefill 还是花了 243 秒（占总时间 87%）。在 300 秒的短预算下，30k 提示词本身就已经占满了大部分时间。

---

## 单轮高分的 30B MoE 为什么卡在 4/8

`qwen3-coder:30b` 是个 17GB 的 MoE-A3B 代码模型（总参数 30B，每 Token 激活约 3B）。单轮代码盲测它拿过 58/72，平时单步吐字也很快，但放进隔离多轮基准后，v3 和 v4 都只有 4/8，平均轮次飙到 **23.2 ~ 36.9 轮**，v4 单任务平均输入 Token 高达 158.8 万。

日志里最明显的问题是代码改对了却不会收尾。v4 超时的 5 个任务里，`callback-refactor`（跑了 72 轮）、`fix-multi-file-config-merger`（41 轮）和 `task-inspection-subagent-status`（7 轮）其实已经把单测跑通了，但模型没有及时结束交付，而是接着发只读命令复查文件，一直耗到预算耗尽。

另一个问题是单步太碎导致的轮次膨胀。`qwen3.8:27b-gguf`（27B Dense）看清文件后直接动手，平均 5.9 到 6.6 轮收工；`qwen3-coder:30b` 只有 3B 激活参数，单步推理快，但面对多文件上下文时经常把命令拆碎、反复试探补丁再返工，把轮次拉长到 4–6 倍。

在单轮问答里，低激活参数意味着响应快；但在多轮 Agent 循环里，每多犹豫一轮，前面累积的历史上下文就得跟着走一轮。单步吐字快，并没换成更快的任务收敛。

---

## Prompt Cache 调优：哪些有效，哪些没用

首轮 30k prefill 占了大头时间，我们在 v4 和 v5 连续做了两轮单变量测试，看看到底能省下多少。

### v4：Daemon 常驻参数只省冷加载，不省首轮 Prefill

v3 跑完后我们怀疑过：是不是 Ollama 默认在任务之间把 KV Cache 换出内存了？于是 v4 加上了 `OLLAMA_KEEP_ALIVE=-1 OLLAMA_NUM_PARALLEL=1 OLLAMA_MAX_LOADED_MODELS=1`。

重跑结果和 v3 基本持平（`qwen3.8` 422s vs 437s，`coder30b` 561s vs 583s）。拆开 403 次 Prompt Eval 请求来看，原因很清楚：

1. **同一任务轮内 Cache 原本就有 93% 命中率**：增量 ≤1k Token 的请求占了 373 次（93%），每次 prefill 只要 0.02s 到 0.5s，说明默认 Ollama Daemon 在对话追加时缓存一直正常。
2. **显式常驻只省了 5 到 12 秒模型冷加载**：对 400 秒级别的多轮任务来说感知不到。
3. **跨任务全量重算是提示词结构问题**：当时 30 次 >1k Token 的大 prefill（最大 36,329 Token）一共花掉 3,185 秒，占了总 prefill 时长的 83%。原因是运行时把变化的工作区路径、身份信息插在了 Prompt 靠前位置，每个任务一开场前缀就分叉了。

### v5：合入共享前缀前置后，不同模型架构分了叉

顺着 v4 的结果，我们在 Holon `0.59.0` 里把系统契约段落前置固定成了 `SharedPrefix`。从文本上看立刻见效：跨任务文本公共前缀从 ~194 个字符拉到了 **56,510 个字符（占 83.2%）**。

但跑完 8 个连续任务后，两款模型的推理层日志完全是两种情况：

**`qwen3-coder:30b`（`qwen3moe` 全注意力架构）：命中了跨任务缓存，但总耗时只动了一点。**
除了第一个任务冷启动（`cached = 0`），后面 7 个任务的开轮在 Daemon 日志里都出现了 `cached n_tokens = 11,378–14,306`（主要落在 ≈11.4k 和 ≈14.2k 两档，视相邻任务分叉点而定；v4 在同位置全是 0）。首轮 opener 中位耗时从 ~140s 降到了 ~121s（-13%），8 个任务 opener 合计从 1377s 降到 1237s（-10%）。

但 8 个任务的总 Provider 耗时几乎没变（4249s → 4296s）。35.5k 的开轮上下文里只复用了 11k–14k，剩下 21k–24k 任务指令和工作区状态还是要现算；再加上后续几十轮 decode 的耗时，开轮省出来的这十几秒很快被平均掉了。

**`qwen3.8:27b-gguf`（`qwen35` 混合/循环记忆架构）：轮内能续，跨任务依然全部重算。**
查看 `llama.cpp` 日志，它在同一个任务的多轮之间可以靠检查点恢复（`restored context checkpoint n_past=35.6k+`）；但只要切到新任务，全部 8 个 opener 都打出了 `forcing full prompt re-processing due to lack of cache data (likely due to SWA or hybrid/recurrent memory)`。包含 SWA 或循环记忆层的架构在当前 `llama.cpp` 里还做不到截断到部分公共前缀后再续算，只能整段重跑。跨任务开轮中位耗时依然停在 ~156s。

下面是两款模型在 v3、v4、v5a 三轮里首轮开场（`round == 1`）的 Provider 耗时对比：

| 指标（8 任务聚合，Provider 口径） | `coder30b` v3 | `coder30b` v4 | **`coder30b` v5a** | `qwen38` v3 | `qwen38` v4 | **`qwen38` v5a** |
| --- | ---: | ---: | ---: | --- | ---: | ---: |
| 总 Provider 耗时 | 4019s | 4249s | **4296s** | 3289s | 3437s | **3097s** |
| 首轮 Opener 合计（占比） | 1471s (37%) | 1377s (32%) | **1237s (29%)** | 1472s (45%) | 1394s (41%) | **1558s (50%)** |
| 单轮 Opener 中位耗时 | ~144s | ~140s | **~121s** | ~160s | ~156s | **~156s** |

把系统前缀固定在最前面本身是对的（全注意力模型和云端支持 `cache_control` 的模型都能直接受益），但只靠这一项改动还改变不了本地编码 Agent 的大头开销。接下来的瓶颈主要是两点：21k–24k 的任务级提示词能不能继续瘦身，以及本地硬件的 decode 吞吐和模型收敛轮次。

---

## FlashAttention 与 KV Cache 量化实测

网上不少本地部署指南会建议顺手加上 `OLLAMA_FLASH_ATTENTION=1` 和 `OLLAMA_KV_CACHE_TYPE=q8_0`。我们在 v5 用 **40k Prompt + 512 Token Decode** 的单变量探针直接测了默认配置（`base`）和同时开启这两项环境变量（`fakv`）的速率：

| 模型 | Prefill 速率 (`base` → `fakv`) | Decode 速率 (`base` → `fakv`) |
| --- | ---: | ---: |
| `qwen3-coder:30b` | 266 → 212 tok/s (**-20%**) | 27 → 19.5 tok/s (**-28%**) |
| `qwen3.8:27b-gguf` | 221.6 → 195.8 tok/s (**-12%**) | 11 → 10.2 tok/s (**-7%**) |

跑完探针后我们直接取消了原定的全矩阵对比：

1. 在 Ollama 0.32.15 的 Metal 后端上，默认的 `--flash-attn auto` 本来就已经开了 FlashAttention，再加 `OLLAMA_FLASH_ATTENTION=1` 没有任何效果。
2. 唯一产生变化的是 `q8_0` KV 量化。虽然能省下一部分显存，但每次生成都要做逐 Token 反量化。在 M5 Pro 统一内存上，这部分计算开销反而让 `coder30b` 的解码速率掉了将近三成。64GB 内存跑 27B–30B Q4_K_M 本来就够装，没必要开 KV 量化换带宽。

---

## 私有环境选型与避坑建议

如果你要在开发机或离线工作站上跑本地编码 Agent，可以按下面这组实测结论选配置：

| 使用场景 | 建议配置 | 实测参考 |
| --- | --- | --- |
| 主力改代码（看重一次做对和收尾） | `qwen3.8:27b`（`ggml-org` `Q4_K_M` GGUF，约 19GB） | 三轮通过 6/8–7/8，v3/v4 单测验证 8/8，平均约 6 轮收敛，单任务 5–10 分钟 |
| 日常小改和快速响应 | `gemma4:26b` | 通过 5/8 且 0 超时，平均 159s/任务，响应最快 |
| 后台异步长任务 | `qwen3-coder:30b`（17GB GGUF） | 代码修复底子不错（`verify` 7/8），但轮次偏多，超时预算建议放宽到 10–15 分钟以上 |
| 不建议用于编码 Agent | `qwen3.8:27b-mlx`、`qwen3.5:0.8b` 等超小模型 | 前者受长提示词 prefill 拖累严重；0.8B 量级在 8 个任务里 0/8，守不住工具和补丁契约 |

自己拿 `qwen3.8:27b` GGUF 往 Ollama 里导时，有三个坑我们在测试里都踩过：

1. **别用管道往 `ollama create` 里传 `Modelfile`**：目前 `ollama create <name> -f -` 从标准输入读取时会静默失败且返回退出码 `0`，表面看不出报错，实际模型没建成功。先在磁盘写好真实的 `Modelfile`（比如 `FROM ./Qwen3.8-27B-Q4_K_M.gguf`），再跑 `ollama create qwen3.8:27b-gguf -f ./Modelfile`。
2. **优先用 `ggml-org` 官方 `Q4_K_M` GGUF**：测试中个别第三方打包版本（如 `unsloth` 同款）在导入 Ollama 时卡在格式校验上，官方 GGUF 最稳。
3. **64GB Apple Silicon 上保持 Ollama 默认参数即可**：保持默认的 `--flash-attn auto` 和 FP16 KV Cache，别加 `OLLAMA_KV_CACHE_TYPE=q8_0`（实测降速 7%–28%），也不要指望 `OLLAMA_KEEP_ALIVE=-1` 缩短跨任务首轮 prefill。

模型导进 Ollama 之后，如果想在 Holon 里设为默认模型，或者按角色路由（比如日常开发用本地 `qwen3.8:27b-gguf`，复杂评审按需切云端模型），可以参考[《配置与切换大模型：从本地 Ollama 到多云灾备》](../guides/configure-models.md)。关于工作区隔离和记忆管理，详见[《使用工作区与隔离目录》](../guides/work-with-workspaces.md)和[《管理 Agent 记忆》](../guides/manage-agent-memory.md)。

---

## 结论

从 5 轮实测来看，64GB 统一内存的桌面设备已经能把本地 27B Dense 模型（`qwen3.8:27b-gguf`）稳定跑在 6/8 到 7/8 的多轮修复通过率上。但再往上的瓶颈不在环境变量调参，而在两件更基础的事：模型在多轮循环里能不能少走弯路、按时收尾，以及运行时能不能把二三十 k 的任务提示词再压下来。

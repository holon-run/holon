---
title: Supported Models
description: Complete reference of all built-in models and providers supported by Holon.
generated: auto-generated from holon source — do not edit directly
---

# Supported Models

Holon includes built-in configuration for **33 provider accounts**
across **42 endpoints** and **253 models**.

This page is auto-generated from the Holon source code (`src/model_catalog.rs` and `src/config.rs`).
Run `cargo run --bin holon-docgen -- models > docs/website/reference/models.md` to regenerate.

Note: subscription-scoped providers such as `dashscope-token-plan` and
`dashscope-coding-plan` are intended for interactive AI coding/agent tool usage
under the upstream service terms, not backend automation or generic scripts.

## Provider Setup

Each provider account endpoint requires an API key or credential to use. Set the listed
environment variable before running Holon. `Legacy Provider Ref` is the user-visible provider id
used in existing `provider/model` refs and config shortcuts.

| Provider Account | Endpoint | Legacy Provider Ref | Transport | Base URL | Auth Env Variable(s) |
|------------------|----------|---------------------|-----------|----------|----------------------|
| `anthropic` | `default` | `anthropic` | Anthropic Messages | `https://api.anthropic.com` | `ANTHROPIC_AUTH_TOKEN` |
| `arcee` | `default` | `arcee` | OpenAI Chat Completions | `https://api.arcee.ai/v1` | `ARCEE_API_KEY` |
| `bigmodel` | `default` | `bigmodel` | Anthropic Messages | `https://open.bigmodel.cn/api/anthropic` | `BIGMODEL_API_KEY` |
| `byteplus` | `default` | `byteplus` | OpenAI Chat Completions | `https://ark.ap-southeast.bytepluses.com/api/v3` | `BYTEPLUS_API_KEY` |
| `byteplus` | `coding` | `byteplus-coding` | OpenAI Chat Completions | `https://ark.ap-southeast.bytepluses.com/api/coding/v3` | `BYTEPLUS_CODING_API_KEY or BYTEPLUS_API_KEY` |
| `chutes` | `default` | `chutes` | OpenAI Chat Completions | `https://llm.chutes.ai/v1` | `CHUTES_API_KEY` |
| `dashscope` | `default` | `dashscope` | Anthropic Messages | `https://dashscope.aliyuncs.com/apps/anthropic` | `DASHSCOPE_API_KEY or QWEN_API_KEY` |
| `dashscope` | `coding-plan` | `dashscope-coding-plan` | Anthropic Messages | `https://coding.dashscope.aliyuncs.com/apps/anthropic` | `DASHSCOPE_CODING_PLAN_API_KEY` |
| `dashscope` | `token-plan` | `dashscope-token-plan` | Anthropic Messages | `https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic` | `DASHSCOPE_TOKEN_PLAN_API_KEY` |
| `deepseek` | `default` | `deepseek` | Anthropic Messages | `https://api.deepseek.com/anthropic` | `DEEPSEEK_API_KEY` |
| `fireworks` | `default` | `fireworks` | OpenAI Chat Completions | `https://api.fireworks.ai/inference/v1` | `FIREWORKS_API_KEY` |
| `gemini` | `default` | `gemini` | Gemini Generate Content | `https://generativelanguage.googleapis.com/v1beta` | `GEMINI_API_KEY` |
| `huggingface` | `default` | `huggingface` | OpenAI Chat Completions | `https://router.huggingface.co/v1` | `HUGGINGFACE_API_KEY or HF_TOKEN` |
| `kilocode` | `default` | `kilocode` | OpenAI Chat Completions | `https://api.kilo.ai/api/gateway` | `KILOCODE_API_KEY` |
| `litellm` | `default` | `litellm` | OpenAI Chat Completions | `http://localhost:4000` | `LITELLM_API_KEY` |
| `minimax` | `default` | `minimax` | Anthropic Messages | `https://api.minimax.io/anthropic` | `MINIMAX_API_KEY` |
| `mistral` | `default` | `mistral` | OpenAI Chat Completions | `https://api.mistral.ai/v1` | `MISTRAL_API_KEY` |
| `moonshot` | `default` | `moonshot` | OpenAI Chat Completions | `https://api.moonshot.ai/v1` | `MOONSHOT_API_KEY` |
| `nearai` | `default` | `nearai` | OpenAI Chat Completions | `https://cloud-api.near.ai/v1` | `NEARAI_API_KEY` |
| `nvidia` | `default` | `nvidia` | OpenAI Chat Completions | `https://integrate.api.nvidia.com/v1` | `NVIDIA_API_KEY` |
| `openai` | `default` | `openai` | OpenAI Responses | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| `openai-codex` | `default` | `openai-codex` | OpenAI Codex | `https://chatgpt.com/backend-api/codex` | `—` |
| `opencode-go` | `default` | `opencode-go` | OpenAI Chat Completions | `https://opencode.ai/zen/go/v1` | `OPENCODE_GO_API_KEY` |
| `opencode-go` | `messages` | `opencode-go-messages` | Anthropic Messages | `https://opencode.ai/zen/go/v1` | `OPENCODE_GO_API_KEY` |
| `openrouter` | `default` | `openrouter` | OpenAI Chat Completions | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| `qianfan` | `default` | `qianfan` | OpenAI Chat Completions | `https://qianfan.baidubce.com/v2` | `QIANFAN_API_KEY` |
| `stepfun` | `default` | `stepfun` | OpenAI Chat Completions | `https://api.stepfun.com/v1` | `STEPFUN_API_KEY` |
| `stepfun` | `plan` | `stepfun-plan` | OpenAI Chat Completions | `https://api.stepfun.com/step_plan/v1` | `STEPFUN_PLAN_API_KEY or STEPFUN_API_KEY` |
| `synthetic` | `default` | `synthetic` | Anthropic Messages | `https://api.synthetic.new/anthropic` | `SYNTHETIC_API_KEY` |
| `tencent-tokenhub` | `default` | `tencent-tokenhub` | OpenAI Chat Completions | `https://tokenhub.tencentmaas.com/v1` | `TOKENHUB_API_KEY` |
| `tencent-tokenhub` | `messages` | `tencent-tokenhub-messages` | Anthropic Messages | `https://tokenhub.tencentmaas.com` | `TOKENHUB_API_KEY` |
| `together` | `default` | `together` | OpenAI Chat Completions | `https://api.together.xyz/v1` | `TOGETHER_API_KEY` |
| `venice` | `default` | `venice` | OpenAI Chat Completions | `https://api.venice.ai/api/v1` | `VENICE_API_KEY` |
| `vercel-ai-gateway` | `default` | `vercel-ai-gateway` | Anthropic Messages | `https://ai-gateway.vercel.sh` | `VERCEL_OIDC_TOKEN or AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY` |
| `vllm` | `default` | `vllm` | OpenAI Chat Completions | `http://127.0.0.1:8000/v1` | `—` |
| `volcengine` | `default` | `volcengine` | OpenAI Responses | `https://ark.cn-beijing.volces.com/api/v3` | `VOLCENGINE_API_KEY` |
| `volcengine` | `plan` | `volcengine-agent` | OpenAI Responses | `https://ark.cn-beijing.volces.com/api/plan/v3` | `VOLCENGINE_AGENT_API_KEY or VOLCENGINE_IMAGE_OPENAI_API_KEY` |
| `volcengine` | `coding` | `volcengine-coding` | OpenAI Responses | `https://ark.cn-beijing.volces.com/api/coding/v3` | `VOLCENGINE_CODING_API_KEY` |
| `xai` | `default` | `xai` | OpenAI Responses | `https://api.x.ai/v1` | `XAI_API_KEY` |
| `xiaomi` | `default` | `xiaomi` | OpenAI Responses | `https://api.xiaomimimo.com/v1` | `XIAOMI_API_KEY` |
| `xiaomi` | `token-plan` | `xiaomi-token-plan` | OpenAI Responses | `https://token-plan-cn.xiaomimimo.com/v1` | `XIAOMI_TOKEN_PLAN_API_KEY` |
| `zai` | `default` | `zai` | Anthropic Messages | `https://api.z.ai/api/anthropic` | `ZAI_API_KEY` |

## Model Catalog

The table below lists every built-in model with its context window, max output tokens,
and capabilities.

| Provider | Model | Usage | Context Window | Max Output | Reasoning | Image |
|----------|-------|-------|----------------|------------|-----------|-------|
| `anthropic` | `claude-fable-5` | `anthropic/claude-fable-5` | 1000000 | 128000 | ✅ | ✅ |
| `anthropic` | `claude-haiku-4-5` | `anthropic/claude-haiku-4-5` | 200000 | 64000 | ✅ | ✅ |
| `anthropic` | `claude-opus-4-8` | `anthropic/claude-opus-4-8` | 1000000 | 128000 | ✅ | ✅ |
| `anthropic` | `claude-sonnet-5` | `anthropic/claude-sonnet-5` | 1000000 | 128000 | ✅ | ✅ |
| `arcee` | `trinity-large-preview` | `arcee/trinity-large-preview` | 131072 | — | — | — |
| `arcee` | `trinity-mini` | `arcee/trinity-mini` | 131072 | — | — | — |
| `bigmodel` | `glm-4-flash-250414` | `bigmodel/glm-4-flash-250414` | 131072 | 16384 | — | — |
| `bigmodel` | `glm-4-flashx-250414` | `bigmodel/glm-4-flashx-250414` | 131072 | 16384 | — | — |
| `bigmodel` | `glm-4-long` | `bigmodel/glm-4-long` | 1000000 | 4096 | — | — |
| `bigmodel` | `glm-4.1v-thinking-flash` | `bigmodel/glm-4.1v-thinking-flash` | 65536 | 16384 | ✅ | ✅ |
| `bigmodel` | `glm-4.1v-thinking-flashx` | `bigmodel/glm-4.1v-thinking-flashx` | 65536 | 16384 | ✅ | ✅ |
| `bigmodel` | `glm-4.5-air` | `bigmodel/glm-4.5-air` | 131072 | 98304 | ✅ | — |
| `bigmodel` | `glm-4.5-airx` | `bigmodel/glm-4.5-airx` | 131072 | 98304 | ✅ | — |
| `bigmodel` | `glm-4.5-flash` | `bigmodel/glm-4.5-flash` | 131072 | 98304 | ✅ | — |
| `bigmodel` | `glm-4.6` | `bigmodel/glm-4.6` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-4.6v` | `bigmodel/glm-4.6v` | 131072 | 32768 | ✅ | ✅ |
| `bigmodel` | `glm-4.6v-flash` | `bigmodel/glm-4.6v-flash` | 131072 | 32768 | ✅ | ✅ |
| `bigmodel` | `glm-4.7` | `bigmodel/glm-4.7` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-4.7-flash` | `bigmodel/glm-4.7-flash` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-4.7-flashx` | `bigmodel/glm-4.7-flashx` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-4v-flash` | `bigmodel/glm-4v-flash` | 16384 | 1024 | — | ✅ |
| `bigmodel` | `glm-5` | `bigmodel/glm-5` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-5-turbo` | `bigmodel/glm-5-turbo` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-5.1` | `bigmodel/glm-5.1` | 204800 | 131072 | ✅ | — |
| `bigmodel` | `glm-5.2` | `bigmodel/glm-5.2` | 1000000 | 131072 | ✅ | — |
| `bigmodel` | `glm-5v-turbo` | `bigmodel/glm-5v-turbo` | 204800 | 131072 | ✅ | ✅ |
| `chutes` | `MiniMaxAI/MiniMax-M2.5-TEE` | `chutes/MiniMaxAI/MiniMax-M2.5-TEE` | 196608 | 65536 | ✅ | — |
| `chutes` | `Qwen/Qwen3-235B-A22B-Thinking-2507-TEE` | `chutes/Qwen/Qwen3-235B-A22B-Thinking-2507-TEE` | 262144 | 262144 | ✅ | — |
| `chutes` | `Qwen/Qwen3-32B-TEE` | `chutes/Qwen/Qwen3-32B-TEE` | 40960 | 40960 | ✅ | — |
| `chutes` | `Qwen/Qwen3.5-397B-A17B-TEE` | `chutes/Qwen/Qwen3.5-397B-A17B-TEE` | 262144 | 65536 | ✅ | ✅ |
| `chutes` | `Qwen/Qwen3.6-27B-TEE` | `chutes/Qwen/Qwen3.6-27B-TEE` | 262144 | 65536 | ✅ | ✅ |
| `chutes` | `deepseek-ai/DeepSeek-V3.2-TEE` | `chutes/deepseek-ai/DeepSeek-V3.2-TEE` | 131072 | 65536 | ✅ | — |
| `chutes` | `google/gemma-4-31B-turbo-TEE` | `chutes/google/gemma-4-31B-turbo-TEE` | 131072 | 65536 | ✅ | ✅ |
| `chutes` | `moonshotai/Kimi-K2.5-TEE` | `chutes/moonshotai/Kimi-K2.5-TEE` | 262144 | 65535 | ✅ | ✅ |
| `chutes` | `moonshotai/Kimi-K2.6-TEE` | `chutes/moonshotai/Kimi-K2.6-TEE` | 262144 | 65535 | ✅ | ✅ |
| `chutes` | `unsloth/Mistral-Nemo-Instruct-2407-TEE` | `chutes/unsloth/Mistral-Nemo-Instruct-2407-TEE` | 131072 | — | — | — |
| `chutes` | `zai-org/GLM-5-TEE` | `chutes/zai-org/GLM-5-TEE` | 202752 | 65535 | ✅ | — |
| `chutes` | `zai-org/GLM-5.1-TEE` | `chutes/zai-org/GLM-5.1-TEE` | 202752 | 65535 | ✅ | — |
| `chutes` | `zai-org/GLM-5.2-TEE` | `chutes/zai-org/GLM-5.2-TEE` | 1048576 | 65535 | ✅ | — |
| `dashscope` | `MiniMax-M2.5` | `dashscope/MiniMax-M2.5` | 196608 | 32768 | ✅ | — |
| `dashscope` | `MiniMax/MiniMax-M3` | `dashscope/MiniMax/MiniMax-M3` | 196608 | 32768 | ✅ | — |
| `dashscope` | `ZHIPU/GLM-5.2` | `dashscope/ZHIPU/GLM-5.2` | 1000000 | 131072 | ✅ | — |
| `dashscope` | `deepseek-v3.2` | `dashscope/deepseek-v3.2` | 128000 | 32768 | ✅ | — |
| `dashscope` | `deepseek-v4-flash` | `dashscope/deepseek-v4-flash` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `deepseek-v4-pro` | `dashscope/deepseek-v4-pro` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `glm-4.7` | `dashscope/glm-4.7` | 202752 | 16384 | ✅ | — |
| `dashscope` | `glm-5` | `dashscope/glm-5` | 202752 | 16384 | ✅ | — |
| `dashscope` | `glm-5.1` | `dashscope/glm-5.1` | 202752 | 65536 | ✅ | — |
| `dashscope` | `glm-5.2` | `dashscope/glm-5.2` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `kimi-k2.5` | `dashscope/kimi-k2.5` | 262144 | 32768 | ✅ | ✅ |
| `dashscope` | `kimi-k2.6` | `dashscope/kimi-k2.6` | 262144 | 65536 | ✅ | ✅ |
| `dashscope` | `kimi-k2.7-code` | `dashscope/kimi-k2.7-code` | 262144 | 65536 | ✅ | ✅ |
| `dashscope` | `mimo-v2.5-pro` | `dashscope/mimo-v2.5-pro` | 1000000 | 131072 | ✅ | — |
| `dashscope` | `qwen3-coder-flash` | `dashscope/qwen3-coder-flash` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `qwen3-coder-next` | `dashscope/qwen3-coder-next` | 262144 | 65536 | ✅ | — |
| `dashscope` | `qwen3-coder-plus` | `dashscope/qwen3-coder-plus` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `qwen3-max-2026-01-23` | `dashscope/qwen3-max-2026-01-23` | 262144 | 65536 | — | — |
| `dashscope` | `qwen3.5-flash` | `dashscope/qwen3.5-flash` | 1000000 | 65536 | ✅ | ✅ |
| `dashscope` | `qwen3.5-plus` | `dashscope/qwen3.5-plus` | 1000000 | 65536 | ✅ | ✅ |
| `dashscope` | `qwen3.6-flash` | `dashscope/qwen3.6-flash` | 1000000 | 65536 | ✅ | ✅ |
| `dashscope` | `qwen3.6-plus` | `dashscope/qwen3.6-plus` | 1000000 | 65536 | ✅ | ✅ |
| `dashscope` | `qwen3.7-max` | `dashscope/qwen3.7-max` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `qwen3.7-max-2026-05-20` | `dashscope/qwen3.7-max-2026-05-20` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `qwen3.7-max-2026-06-08` | `dashscope/qwen3.7-max-2026-06-08` | 1000000 | 65536 | ✅ | — |
| `dashscope` | `qwen3.7-plus` | `dashscope/qwen3.7-plus` | 1000000 | 65536 | ✅ | ✅ |
| `dashscope` | `qwen3.7-plus-2026-05-26` | `dashscope/qwen3.7-plus-2026-05-26` | 1000000 | 65536 | ✅ | ✅ |
| `deepseek` | `deepseek-v4-flash` | `deepseek/deepseek-v4-flash` | 1000000 | 384000 | ✅ | — |
| `deepseek` | `deepseek-v4-pro` | `deepseek/deepseek-v4-pro` | 1000000 | 384000 | ✅ | — |
| `fireworks` | `accounts/fireworks/models/deepseek-v4-flash` | `fireworks/accounts/fireworks/models/deepseek-v4-flash` | 1048576 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/deepseek-v4-pro` | `fireworks/accounts/fireworks/models/deepseek-v4-pro` | 1048576 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/glm-5p1` | `fireworks/accounts/fireworks/models/glm-5p1` | 202752 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/glm-5p2` | `fireworks/accounts/fireworks/models/glm-5p2` | 1048576 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/gpt-oss-120b` | `fireworks/accounts/fireworks/models/gpt-oss-120b` | 131072 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/kimi-k2p6` | `fireworks/accounts/fireworks/models/kimi-k2p6` | 262144 | — | ✅ | ✅ |
| `fireworks` | `accounts/fireworks/models/kimi-k2p7-code` | `fireworks/accounts/fireworks/models/kimi-k2p7-code` | 262144 | — | ✅ | ✅ |
| `fireworks` | `accounts/fireworks/models/minimax-m2p7` | `fireworks/accounts/fireworks/models/minimax-m2p7` | 196608 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/minimax-m3` | `fireworks/accounts/fireworks/models/minimax-m3` | 524288 | — | ✅ | ✅ |
| `fireworks` | `accounts/fireworks/models/nemotron-3-ultra-nvfp4` | `fireworks/accounts/fireworks/models/nemotron-3-ultra-nvfp4` | 262144 | — | ✅ | — |
| `fireworks` | `accounts/fireworks/models/qwen3p6-plus` | `fireworks/accounts/fireworks/models/qwen3p6-plus` | — | — | ✅ | ✅ |
| `fireworks` | `accounts/fireworks/models/qwen3p7-plus` | `fireworks/accounts/fireworks/models/qwen3p7-plus` | 262144 | — | ✅ | ✅ |
| `gemini` | `gemini-2.5-flash` | `gemini/gemini-2.5-flash` | 1048576 | 65536 | ✅ | ✅ |
| `gemini` | `gemini-2.5-flash-lite` | `gemini/gemini-2.5-flash-lite` | 1048576 | 65536 | ✅ | ✅ |
| `gemini` | `gemini-2.5-pro` | `gemini/gemini-2.5-pro` | 1048576 | 65536 | ✅ | ✅ |
| `gemini` | `gemini-3.1-flash-lite` | `gemini/gemini-3.1-flash-lite` | 1048576 | 65536 | ✅ | ✅ |
| `gemini` | `gemini-3.1-pro-preview` | `gemini/gemini-3.1-pro-preview` | 1048576 | 65536 | ✅ | ✅ |
| `gemini` | `gemini-3.5-flash` | `gemini/gemini-3.5-flash` | 1048576 | 65536 | ✅ | ✅ |
| `huggingface` | `openai/gpt-oss-120b` | `huggingface/openai/gpt-oss-120b` | 131072 | — | ✅ | — |
| `kilocode` | `kilo-auto/balanced` | `kilocode/kilo-auto/balanced` | 1000000 | — | ✅ | ✅ |
| `kilocode` | `kilo-auto/efficient` | `kilocode/kilo-auto/efficient` | 1000000 | — | ✅ | ✅ |
| `kilocode` | `kilo-auto/free` | `kilocode/kilo-auto/free` | 256000 | — | ✅ | — |
| `kilocode` | `kilo-auto/frontier` | `kilocode/kilo-auto/frontier` | 1000000 | — | ✅ | ✅ |
| `litellm` | `claude-opus-4-6` | `litellm/claude-opus-4-6` | 200000 | 128000 | ✅ | ✅ |
| `minimax` | `MiniMax-M2` | `minimax/MiniMax-M2` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.1` | `minimax/MiniMax-M2.1` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.1-highspeed` | `minimax/MiniMax-M2.1-highspeed` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.5` | `minimax/MiniMax-M2.5` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.5-highspeed` | `minimax/MiniMax-M2.5-highspeed` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.7` | `minimax/MiniMax-M2.7` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M2.7-highspeed` | `minimax/MiniMax-M2.7-highspeed` | 204800 | 128000 | ✅ | — |
| `minimax` | `MiniMax-M3` | `minimax/MiniMax-M3` | 1000000 | 32768 | ✅ | ✅ |
| `mistral` | `codestral-latest` | `mistral/codestral-latest` | 128000 | 4096 | — | — |
| `mistral` | `mistral-large-latest` | `mistral/mistral-large-latest` | 256000 | 16384 | — | ✅ |
| `mistral` | `mistral-medium-latest` | `mistral/mistral-medium-latest` | 256000 | 8192 | ✅ | ✅ |
| `mistral` | `mistral-small-latest` | `mistral/mistral-small-latest` | 256000 | 16384 | — | ✅ |
| `moonshot` | `kimi-k2.5` | `moonshot/kimi-k2.5` | 262144 | 262144 | ✅ | ✅ |
| `moonshot` | `kimi-k2.6` | `moonshot/kimi-k2.6` | 262144 | 262144 | ✅ | ✅ |
| `moonshot` | `kimi-k2.7-code` | `moonshot/kimi-k2.7-code` | 262144 | 262144 | ✅ | ✅ |
| `moonshot` | `kimi-k2.7-code-highspeed` | `moonshot/kimi-k2.7-code-highspeed` | 262144 | 262144 | ✅ | ✅ |
| `moonshot` | `moonshot-v1-128k` | `moonshot/moonshot-v1-128k` | 131072 | 131072 | — | — |
| `moonshot` | `moonshot-v1-128k-vision-preview` | `moonshot/moonshot-v1-128k-vision-preview` | 131072 | 131072 | — | ✅ |
| `moonshot` | `moonshot-v1-32k` | `moonshot/moonshot-v1-32k` | 32768 | 32768 | — | — |
| `moonshot` | `moonshot-v1-32k-vision-preview` | `moonshot/moonshot-v1-32k-vision-preview` | 32768 | 32768 | — | ✅ |
| `moonshot` | `moonshot-v1-8k` | `moonshot/moonshot-v1-8k` | 8192 | 8192 | — | — |
| `moonshot` | `moonshot-v1-8k-vision-preview` | `moonshot/moonshot-v1-8k-vision-preview` | 8192 | 8192 | — | ✅ |
| `moonshot` | `moonshot-v1-auto` | `moonshot/moonshot-v1-auto` | 131072 | 131072 | — | — |
| `nearai` | `Qwen/Qwen3-VL-30B-A3B-Instruct` | `nearai/Qwen/Qwen3-VL-30B-A3B-Instruct` | 16384 | 8192 | — | ✅ |
| `nearai` | `Qwen/Qwen3.5-122B-A10B` | `nearai/Qwen/Qwen3.5-122B-A10B` | 262144 | 16384 | ✅ | ✅ |
| `nearai` | `Qwen/Qwen3.6-35B-A3B-FP8` | `nearai/Qwen/Qwen3.6-35B-A3B-FP8` | 262144 | 8192 | ✅ | — |
| `nearai` | `google/gemma-4-31B-it` | `nearai/google/gemma-4-31B-it` | 262144 | 8192 | ✅ | ✅ |
| `nearai` | `zai-org/GLM-5.1-FP8` | `nearai/zai-org/GLM-5.1-FP8` | 202752 | 16384 | ✅ | — |
| `nvidia` | `minimaxai/minimax-m2.7` | `nvidia/minimaxai/minimax-m2.7` | 204800 | — | ✅ | — |
| `nvidia` | `minimaxai/minimax-m3` | `nvidia/minimaxai/minimax-m3` | 1000000 | — | ✅ | ✅ |
| `nvidia` | `moonshotai/kimi-k2.6` | `nvidia/moonshotai/kimi-k2.6` | 262144 | — | ✅ | ✅ |
| `nvidia` | `nvidia/nemotron-3-super-120b-a12b` | `nvidia/nvidia/nemotron-3-super-120b-a12b` | 1000000 | — | ✅ | — |
| `nvidia` | `z-ai/glm-5.2` | `nvidia/z-ai/glm-5.2` | 1000000 | — | ✅ | — |
| `openai` | `gpt-5.3` | `openai/gpt-5.3` | 128000 | — | ✅ | ✅ |
| `openai` | `gpt-5.4` | `openai/gpt-5.4` | 272000 | — | ✅ | ✅ |
| `openai` | `gpt-5.4-mini` | `openai/gpt-5.4-mini` | 128000 | — | ✅ | ✅ |
| `openai` | `gpt-5.6-luna` | `openai/gpt-5.6-luna` | 372000 | 128000 | ✅ | ✅ |
| `openai` | `gpt-5.6-sol` | `openai/gpt-5.6-sol` | 372000 | 128000 | ✅ | ✅ |
| `openai` | `gpt-5.6-terra` | `openai/gpt-5.6-terra` | 372000 | 128000 | ✅ | ✅ |
| `openai` | `gpt-image-2` | `openai/gpt-image-2` | — | — | — | — |
| `openai-codex` | `gpt-5.3-codex-spark` | `openai-codex/gpt-5.3-codex-spark` | 128000 | — | ✅ | — |
| `openai-codex` | `gpt-5.4` | `openai-codex/gpt-5.4` | 272000 | — | ✅ | ✅ |
| `openai-codex` | `gpt-5.4-mini` | `openai-codex/gpt-5.4-mini` | 272000 | — | ✅ | ✅ |
| `openai-codex` | `gpt-5.5` | `openai-codex/gpt-5.5` | 272000 | — | ✅ | ✅ |
| `openai-codex` | `gpt-5.6-luna` | `openai-codex/gpt-5.6-luna` | 372000 | — | ✅ | ✅ |
| `openai-codex` | `gpt-5.6-sol` | `openai-codex/gpt-5.6-sol` | 372000 | — | ✅ | ✅ |
| `openai-codex` | `gpt-5.6-terra` | `openai-codex/gpt-5.6-terra` | 372000 | — | ✅ | ✅ |
| `opencode-go` | `deepseek-v4-flash` | `opencode-go/deepseek-v4-flash` | 1000000 | 384000 | ✅ | — |
| `opencode-go` | `deepseek-v4-pro` | `opencode-go/deepseek-v4-pro` | 1000000 | 384000 | ✅ | — |
| `opencode-go` | `glm-5.1` | `opencode-go/glm-5.1` | 202800 | 131072 | ✅ | — |
| `opencode-go` | `glm-5.2` | `opencode-go/glm-5.2` | 1000000 | 131072 | ✅ | — |
| `opencode-go` | `kimi-k2.6` | `opencode-go/kimi-k2.6` | 262144 | 262144 | ✅ | ✅ |
| `opencode-go` | `kimi-k2.7-code` | `opencode-go/kimi-k2.7-code` | 262144 | 262144 | ✅ | ✅ |
| `opencode-go` | `mimo-v2.5` | `opencode-go/mimo-v2.5` | 1048576 | 131072 | ✅ | ✅ |
| `opencode-go` | `mimo-v2.5-pro` | `opencode-go/mimo-v2.5-pro` | 1048576 | 131072 | ✅ | — |
| `opencode-go` | `minimax-m2.5` | `opencode-go/minimax-m2.5` | 196608 | 32768 | ✅ | — |
| `opencode-go` | `minimax-m2.7` | `opencode-go/minimax-m2.7` | 204800 | — | ✅ | — |
| `opencode-go` | `minimax-m3` | `opencode-go/minimax-m3` | 1000000 | — | ✅ | ✅ |
| `opencode-go` | `qwen3.6-plus` | `opencode-go/qwen3.6-plus` | 1000000 | 65536 | ✅ | ✅ |
| `opencode-go` | `qwen3.7-max` | `opencode-go/qwen3.7-max` | 1000000 | 65536 | ✅ | — |
| `opencode-go` | `qwen3.7-plus` | `opencode-go/qwen3.7-plus` | 1000000 | 65536 | ✅ | ✅ |
| `openrouter` | `auto` | `openrouter/auto` | 2000000 | — | ✅ | ✅ |
| `qianfan` | `deepseek-v3.2` | `qianfan/deepseek-v3.2` | 131072 | 32768 | — | — |
| `qianfan` | `deepseek-v3.2-think` | `qianfan/deepseek-v3.2-think` | 163840 | 65536 | ✅ | — |
| `qianfan` | `ernie-5.0` | `qianfan/ernie-5.0` | 248832 | 65536 | — | ✅ |
| `qianfan` | `ernie-5.0-thinking-preview` | `qianfan/ernie-5.0-thinking-preview` | 248832 | 65536 | ✅ | ✅ |
| `qianfan` | `ernie-5.1` | `qianfan/ernie-5.1` | 248832 | 65536 | — | — |
| `qianfan` | `ernie-x1.1` | `qianfan/ernie-x1.1` | 121856 | 65536 | ✅ | — |
| `stepfun` | `step-3.5-flash` | `stepfun/step-3.5-flash` | 262144 | — | ✅ | — |
| `stepfun` | `step-3.5-flash-2603` | `stepfun/step-3.5-flash-2603` | 262144 | — | ✅ | — |
| `stepfun` | `step-3.7-flash` | `stepfun/step-3.7-flash` | 262144 | — | ✅ | ✅ |
| `synthetic` | `hf:MiniMaxAI/MiniMax-M3` | `synthetic/hf:MiniMaxAI/MiniMax-M3` | 262144 | 65536 | ✅ | ✅ |
| `synthetic` | `hf:Qwen/Qwen3.6-27B` | `synthetic/hf:Qwen/Qwen3.6-27B` | 262144 | 65536 | ✅ | ✅ |
| `synthetic` | `hf:moonshotai/Kimi-K2.7-Code` | `synthetic/hf:moonshotai/Kimi-K2.7-Code` | 262144 | 65536 | ✅ | ✅ |
| `synthetic` | `hf:nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4` | `synthetic/hf:nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-NVFP4` | 262144 | 65536 | ✅ | — |
| `synthetic` | `hf:openai/gpt-oss-120b` | `synthetic/hf:openai/gpt-oss-120b` | 131072 | 65536 | ✅ | — |
| `synthetic` | `hf:zai-org/GLM-4.7-Flash` | `synthetic/hf:zai-org/GLM-4.7-Flash` | 196608 | 65536 | ✅ | — |
| `synthetic` | `hf:zai-org/GLM-5.2` | `synthetic/hf:zai-org/GLM-5.2` | 524288 | 65536 | ✅ | — |
| `synthetic` | `syn:large:text` | `synthetic/syn:large:text` | 524288 | 65536 | ✅ | — |
| `synthetic` | `syn:large:vision` | `synthetic/syn:large:vision` | 262144 | 65536 | ✅ | ✅ |
| `synthetic` | `syn:small:text` | `synthetic/syn:small:text` | 196608 | 65536 | ✅ | — |
| `synthetic` | `syn:small:vision` | `synthetic/syn:small:vision` | 262144 | 65536 | ✅ | ✅ |
| `tencent-tokenhub` | `deepseek-v3.2` | `tencent-tokenhub/deepseek-v3.2` | 128000 | 32768 | ✅ | — |
| `tencent-tokenhub` | `deepseek-v4-flash` | `tencent-tokenhub/deepseek-v4-flash` | 1000000 | 384000 | ✅ | — |
| `tencent-tokenhub` | `deepseek-v4-pro` | `tencent-tokenhub/deepseek-v4-pro` | 1000000 | 384000 | ✅ | — |
| `tencent-tokenhub` | `glm-5` | `tencent-tokenhub/glm-5` | 202800 | 131072 | ✅ | — |
| `tencent-tokenhub` | `glm-5-turbo` | `tencent-tokenhub/glm-5-turbo` | 202800 | 131072 | ✅ | — |
| `tencent-tokenhub` | `glm-5.1` | `tencent-tokenhub/glm-5.1` | 202800 | 131072 | ✅ | — |
| `tencent-tokenhub` | `glm-5.2` | `tencent-tokenhub/glm-5.2` | 1000000 | 131072 | ✅ | — |
| `tencent-tokenhub` | `glm-5v-turbo` | `tencent-tokenhub/glm-5v-turbo` | 202800 | 131072 | ✅ | ✅ |
| `tencent-tokenhub` | `hunyuan-role-latest` | `tencent-tokenhub/hunyuan-role-latest` | — | — | — | — |
| `tencent-tokenhub` | `hunyuan-t1-vision-20250916` | `tencent-tokenhub/hunyuan-t1-vision-20250916` | — | — | ✅ | ✅ |
| `tencent-tokenhub` | `hy-mt2-lite` | `tencent-tokenhub/hy-mt2-lite` | — | — | — | — |
| `tencent-tokenhub` | `hy-mt2-plus` | `tencent-tokenhub/hy-mt2-plus` | — | — | — | — |
| `tencent-tokenhub` | `hy-mt2-pro` | `tencent-tokenhub/hy-mt2-pro` | — | — | — | — |
| `tencent-tokenhub` | `hy-role` | `tencent-tokenhub/hy-role` | — | — | — | — |
| `tencent-tokenhub` | `hy-vision-2.0-instruct` | `tencent-tokenhub/hy-vision-2.0-instruct` | — | — | — | ✅ |
| `tencent-tokenhub` | `hy3` | `tencent-tokenhub/hy3` | 256000 | 128000 | ✅ | — |
| `tencent-tokenhub` | `hy3-preview` | `tencent-tokenhub/hy3-preview` | 256000 | 128000 | ✅ | — |
| `tencent-tokenhub` | `kimi-k2.5` | `tencent-tokenhub/kimi-k2.5` | 262144 | 262144 | ✅ | ✅ |
| `tencent-tokenhub` | `kimi-k2.6` | `tencent-tokenhub/kimi-k2.6` | 262144 | 262144 | ✅ | ✅ |
| `tencent-tokenhub` | `kimi-k2.7-code` | `tencent-tokenhub/kimi-k2.7-code` | 262144 | 262144 | ✅ | ✅ |
| `tencent-tokenhub` | `kimi-k2.7-code-highspeed` | `tencent-tokenhub/kimi-k2.7-code-highspeed` | 262144 | 262144 | ✅ | ✅ |
| `tencent-tokenhub` | `minimax-m2.5` | `tencent-tokenhub/minimax-m2.5` | 196608 | 32768 | ✅ | — |
| `tencent-tokenhub` | `minimax-m2.7` | `tencent-tokenhub/minimax-m2.7` | 204800 | — | ✅ | — |
| `tencent-tokenhub` | `minimax-m3` | `tencent-tokenhub/minimax-m3` | 1000000 | — | ✅ | ✅ |
| `tencent-tokenhub` | `qwen3.5-flash` | `tencent-tokenhub/qwen3.5-flash` | 1000000 | 65536 | ✅ | ✅ |
| `tencent-tokenhub` | `qwen3.5-plus` | `tencent-tokenhub/qwen3.5-plus` | 1000000 | 65536 | ✅ | ✅ |
| `tencent-tokenhub` | `youtu-vita` | `tencent-tokenhub/youtu-vita` | — | — | — | ✅ |
| `together` | `MiniMaxAI/MiniMax-M2.7` | `together/MiniMaxAI/MiniMax-M2.7` | 202752 | — | ✅ | — |
| `together` | `MiniMaxAI/MiniMax-M3` | `together/MiniMaxAI/MiniMax-M3` | 524288 | — | — | ✅ |
| `together` | `Qwen/Qwen3.5-9B` | `together/Qwen/Qwen3.5-9B` | 262144 | — | ✅ | ✅ |
| `together` | `deepseek-ai/DeepSeek-V4-Pro` | `together/deepseek-ai/DeepSeek-V4-Pro` | 512000 | — | ✅ | — |
| `together` | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | `together/meta-llama/Llama-3.3-70B-Instruct-Turbo` | 131072 | — | — | — |
| `together` | `moonshotai/Kimi-K2.6` | `together/moonshotai/Kimi-K2.6` | 262144 | — | ✅ | ✅ |
| `together` | `moonshotai/Kimi-K2.7-Code` | `together/moonshotai/Kimi-K2.7-Code` | 262144 | — | — | ✅ |
| `together` | `nvidia/nemotron-3-ultra-550b-a55b` | `together/nvidia/nemotron-3-ultra-550b-a55b` | 512300 | — | ✅ | — |
| `together` | `openai/gpt-oss-120b` | `together/openai/gpt-oss-120b` | 128000 | — | ✅ | — |
| `together` | `zai-org/GLM-5.2` | `together/zai-org/GLM-5.2` | 262144 | — | — | — |
| `venice` | `qwen3-235b-a22b-thinking-2507` | `venice/qwen3-235b-a22b-thinking-2507` | 128000 | 16384 | ✅ | — |
| `venice` | `qwen3-coder-480b-a35b-instruct-turbo` | `venice/qwen3-coder-480b-a35b-instruct-turbo` | 256000 | 65536 | — | — |
| `venice` | `qwen3-vl-235b-a22b` | `venice/qwen3-vl-235b-a22b` | 128000 | 16384 | — | ✅ |
| `venice` | `venice-uncensored-1-2` | `venice/venice-uncensored-1-2` | 128000 | 8192 | — | ✅ |
| `venice` | `zai-org-glm-4.7` | `venice/zai-org-glm-4.7` | 198000 | 16384 | ✅ | — |
| `vercel-ai-gateway` | `anthropic/claude-opus-4.6` | `vercel-ai-gateway/anthropic/claude-opus-4.6` | 1000000 | 128000 | ✅ | ✅ |
| `vercel-ai-gateway` | `moonshotai/kimi-k2.6` | `vercel-ai-gateway/moonshotai/kimi-k2.6` | 262000 | 262000 | ✅ | ✅ |
| `vercel-ai-gateway` | `openai/gpt-5.4` | `vercel-ai-gateway/openai/gpt-5.4` | 1050000 | 128000 | ✅ | ✅ |
| `vercel-ai-gateway` | `openai/gpt-5.4-pro` | `vercel-ai-gateway/openai/gpt-5.4-pro` | 1050000 | 128000 | ✅ | ✅ |
| `vllm` | `meta-llama/Meta-Llama-3-8B-Instruct` | `vllm/meta-llama/Meta-Llama-3-8B-Instruct` | 131072 | 8192 | — | — |
| `volcengine` | `ark-code-latest` | `volcengine/ark-code-latest` | 256000 | 65536 | ✅ | — |
| `volcengine` | `deepseek-v3-2-251201` | `volcengine/deepseek-v3-2-251201` | 128000 | 4096 | — | — |
| `volcengine` | `deepseek-v4-flash` | `volcengine/deepseek-v4-flash` | 1000000 | 8192 | ✅ | — |
| `volcengine` | `deepseek-v4-pro` | `volcengine/deepseek-v4-pro` | 1000000 | 8192 | ✅ | — |
| `volcengine` | `doubao-seed-1-8-251228` | `volcengine/doubao-seed-1-8-251228` | 256000 | 4096 | — | ✅ |
| `volcengine` | `doubao-seed-2-0-code-preview-260215` | `volcengine/doubao-seed-2-0-code-preview-260215` | 256000 | 4096 | — | ✅ |
| `volcengine` | `doubao-seed-2-0-lite-260215` | `volcengine/doubao-seed-2-0-lite-260215` | 256000 | 4096 | — | — |
| `volcengine` | `doubao-seed-2-0-pro-260215` | `volcengine/doubao-seed-2-0-pro-260215` | 256000 | 4096 | ✅ | ✅ |
| `volcengine` | `doubao-seedream-5.0-lite` | `volcengine/doubao-seedream-5.0-lite` | — | — | — | — |
| `volcengine` | `glm-5.2` | `volcengine/glm-5.2` | 204800 | 128000 | ✅ | — |
| `volcengine` | `kimi-k2.6` | `volcengine/kimi-k2.6` | 262144 | 32768 | ✅ | — |
| `volcengine` | `kimi-k2.7-code` | `volcengine/kimi-k2.7-code` | 262144 | 32768 | ✅ | — |
| `xai` | `grok-4.3` | `xai/grok-4.3` | 1000000 | — | ✅ | ✅ |
| `xai` | `grok-4.5` | `xai/grok-4.5` | 500000 | — | ✅ | ✅ |
| `xiaomi` | `mimo-v2.5` | `xiaomi/mimo-v2.5` | 1048576 | 131072 | ✅ | ✅ |
| `xiaomi` | `mimo-v2.5-pro` | `xiaomi/mimo-v2.5-pro` | 1048576 | 131072 | ✅ | — |
| `zai` | `glm-4-32b-0414-128k` | `zai/glm-4-32b-0414-128k` | 131072 | 16384 | — | — |
| `zai` | `glm-4.5` | `zai/glm-4.5` | 131072 | 98304 | ✅ | — |
| `zai` | `glm-4.5-air` | `zai/glm-4.5-air` | 131072 | 98304 | ✅ | — |
| `zai` | `glm-4.5-airx` | `zai/glm-4.5-airx` | 131072 | 98304 | ✅ | — |
| `zai` | `glm-4.5-flash` | `zai/glm-4.5-flash` | 131072 | 98304 | ✅ | — |
| `zai` | `glm-4.5-x` | `zai/glm-4.5-x` | 131072 | 98304 | ✅ | — |
| `zai` | `glm-4.5v` | `zai/glm-4.5v` | 64000 | 16384 | ✅ | ✅ |
| `zai` | `glm-4.6` | `zai/glm-4.6` | 204800 | 131072 | ✅ | — |
| `zai` | `glm-4.6v` | `zai/glm-4.6v` | 128000 | 32768 | ✅ | ✅ |
| `zai` | `glm-4.6v-flash` | `zai/glm-4.6v-flash` | 128000 | 32768 | ✅ | ✅ |
| `zai` | `glm-4.6v-flashx` | `zai/glm-4.6v-flashx` | 128000 | 32768 | ✅ | ✅ |
| `zai` | `glm-4.7` | `zai/glm-4.7` | 204800 | 131072 | ✅ | — |
| `zai` | `glm-4.7-flash` | `zai/glm-4.7-flash` | 200000 | 131072 | ✅ | — |
| `zai` | `glm-4.7-flashx` | `zai/glm-4.7-flashx` | 200000 | 128000 | ✅ | — |
| `zai` | `glm-5` | `zai/glm-5` | 202800 | 131072 | ✅ | — |
| `zai` | `glm-5-turbo` | `zai/glm-5-turbo` | 202800 | 131072 | ✅ | — |
| `zai` | `glm-5.1` | `zai/glm-5.1` | 202800 | 131072 | ✅ | — |
| `zai` | `glm-5.2` | `zai/glm-5.2` | 1000000 | 131072 | ✅ | — |
| `zai` | `glm-5v-turbo` | `zai/glm-5v-turbo` | 202800 | 131072 | ✅ | ✅ |

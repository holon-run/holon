import type { RuntimeModelOption, RuntimeProviderSummary } from "../runtime/types";

// Explicit presentation metadata only: never use these labels as route identities.
const brands: Record<string, [string, string]> = {
  openai: ["OpenAI", "GPT"], "openai-codex": ["OpenAI Codex", "ChatGPT OAuth 订阅 登录"],
  anthropic: ["Anthropic", "Claude"], gemini: ["Google Gemini", "谷歌"],
  deepseek: ["DeepSeek", "深度求索"], dashscope: ["阿里云百炼 · DashScope", "阿里 通义 千问 Qwen Alibaba"],
  bigmodel: ["智谱 · BigModel", "GLM Zhipu"], zai: ["Z.AI", "智谱 GLM"],
  moonshot: ["Moonshot · Kimi", "月之暗面"], minimax: ["MiniMax", "海螺"],
  volcengine: ["火山引擎 · Volcengine", "豆包 字节 Doubao ByteDance"],
  byteplus: ["BytePlus", "字节 火山"], xiaomi: ["小米 · Xiaomi", "MiMo"],
  "tencent-tokenhub": ["腾讯 · TokenHub", "Tencent 混元 Hunyuan"],
  stepfun: ["阶跃星辰 · StepFun", "Step"], qianfan: ["百度千帆 · Qianfan", "Baidu 文心"],
  openrouter: ["OpenRouter", "gateway 聚合"], ollama: ["Ollama", "local 本地"],
  vllm: ["vLLM", "local 本地 self hosted"], litellm: ["LiteLLM", "gateway 代理"],
  xai: ["xAI", "Grok"], mistral: ["Mistral", ""], nvidia: ["NVIDIA", "英伟达"],
  huggingface: ["Hugging Face", "HF"], fireworks: ["Fireworks", ""],
  together: ["Together AI", ""], "vercel-ai-gateway": ["Vercel AI Gateway", ""],
};
const variants: Record<string, [string, string]> = {
  "dashscope-coding-plan": ["dashscope", "Coding Plan"],
  "dashscope-token-plan": ["dashscope", "Token Plan"],
  "volcengine-coding": ["volcengine", "Coding Plan"],
  "volcengine-agent": ["volcengine", "Agent"],
  "byteplus-coding": ["byteplus", "Coding Plan"],
  "xiaomi-token-plan": ["xiaomi", "Token Plan"],
  "stepfun-plan": ["stepfun", "Plan"],
  "tencent-tokenhub-messages": ["tencent-tokenhub", "Messages API"],
  "opencode-go-messages": ["opencode-go", "Messages API"],
};

export function providerPresentation(id: string) {
  const [brand, plan] = (Object.hasOwn(variants, id) ? variants[id] : undefined) ?? [id, ""];
  const [name, aliases] = (Object.hasOwn(brands, brand) ? brands[brand] : undefined) ?? [brand, ""];
  return { name, plan, label: plan ? `${name} · ${plan}` : name, search: `${id} ${name} ${aliases} ${plan}`.toLowerCase() };
}

export function modelSourceLabel(model: RuntimeModelOption): string {
  const label = providerPresentation(model.routeProvider).label;
  return model.endpoint !== "default"
    ? `${label} · ${model.endpoint}` : label;
}

export function modelMatches(model: RuntimeModelOption, query: string): boolean {
  const text = `${model.displayName} ${model.model} ${model.routeRef} ${modelSourceLabel(model)} ${providerPresentation(model.routeProvider).search} ${providerPresentation(model.providerFamily).search}`.toLowerCase();
  return query.toLowerCase().trim().split(/\s+/).every((word) => text.includes(word));
}

export function providerIsConnected(provider: RuntimeProviderSummary, usedProviders: ReadonlySet<string>): boolean {
  return provider.credentialConfigured || provider.configuredInConfig || usedProviders.has(provider.id);
}

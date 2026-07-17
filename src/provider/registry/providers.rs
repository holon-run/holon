use crate::config::ProviderTransportKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderMaterializer {
    OpenAiCodex,
    OpenAi,
    Anthropic,
    Gemini,
    Generic,
    VercelAiGateway,
    AliasOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderContextManagement {
    Default,
    Anthropic,
    AnthropicCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderWebSearch {
    None,
    OpenAi,
    OpenAiCodex,
    Anthropic,
    Zai,
    BigModel,
    DeepSeek,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelDiscoveryAuth {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelDiscoveryRoute {
    Fixed(&'static str),
    OpenAiCompatible,
    OpenAiV1,
    Venice,
    VercelAiGateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelDiscoveryDecoder {
    Arcee,
    HuggingFace,
    Kilo,
    OpenAiCompatible,
    NearAi,
    OpenCodeGo,
    OpenRouter,
    Synthetic,
    TencentTokenHub,
    Venice,
    VercelAiGateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderCatalogPolicy {
    StaticOnly,
    StaticAndDiscovery,
    DiscoveryOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ProviderCatalogRegistration {
    Core,
    HostedEarly,
    KiloCode,
    HostedMiddle,
    OpenCodeGo,
    ChinaEarly,
    HostedLate,
    ChinaLate,
    TencentTokenHub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelDiscoveryDefinition {
    pub(crate) auth: ModelDiscoveryAuth,
    pub(crate) route: ModelDiscoveryRoute,
    pub(crate) decoder: ModelDiscoveryDecoder,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderDefinition {
    pub(crate) legacy_provider: &'static str,
    pub(crate) route_provider: &'static str,
    pub(crate) route_endpoint: &'static str,
    pub(crate) transport: ProviderTransportKind,
    pub(crate) default_base_url: &'static str,
    pub(crate) credential_envs: &'static [&'static str],
    pub(crate) materializer: ProviderMaterializer,
    pub(crate) context_management: ProviderContextManagement,
    pub(crate) web_search: ProviderWebSearch,
    pub(crate) default_reasoning_effort: Option<&'static str>,
    pub(crate) discovery: Option<ModelDiscoveryDefinition>,
    pub(crate) catalog_policy: ProviderCatalogPolicy,
    pub(crate) catalog_registration: Option<ProviderCatalogRegistration>,
}

macro_rules! discovery {
    ($auth:ident, $route:expr, $decoder:ident) => {
        Some(ModelDiscoveryDefinition {
            auth: ModelDiscoveryAuth::$auth,
            route: $route,
            decoder: ModelDiscoveryDecoder::$decoder,
        })
    };
}

macro_rules! provider {
    (
        $legacy:literal => $route_provider:literal @ $route_endpoint:literal,
        $transport:ident, $base_url:literal, [$($env:literal),* $(,)?],
        $materializer:ident, $context:ident, $web_search:ident,
        $reasoning:expr, $discovery:expr, $catalog:ident, $registration:ident
    ) => {
        ProviderDefinition {
            legacy_provider: $legacy,
            route_provider: $route_provider,
            route_endpoint: $route_endpoint,
            transport: ProviderTransportKind::$transport,
            default_base_url: $base_url,
            credential_envs: &[$($env),*],
            materializer: ProviderMaterializer::$materializer,
            context_management: ProviderContextManagement::$context,
            web_search: ProviderWebSearch::$web_search,
            default_reasoning_effort: $reasoning,
            discovery: $discovery,
            catalog_policy: ProviderCatalogPolicy::$catalog,
            catalog_registration: Some(
                ProviderCatalogRegistration::$registration,
            ),
        }
    };
    (
        $legacy:literal => $route_provider:literal @ $route_endpoint:literal,
        $transport:ident, $base_url:literal, [$($env:literal),* $(,)?],
        $materializer:ident, $context:ident, $web_search:ident,
        $reasoning:expr, $discovery:expr, $catalog:ident
    ) => {
        ProviderDefinition {
            legacy_provider: $legacy,
            route_provider: $route_provider,
            route_endpoint: $route_endpoint,
            transport: ProviderTransportKind::$transport,
            default_base_url: $base_url,
            credential_envs: &[$($env),*],
            materializer: ProviderMaterializer::$materializer,
            context_management: ProviderContextManagement::$context,
            web_search: ProviderWebSearch::$web_search,
            default_reasoning_effort: $reasoning,
            discovery: $discovery,
            catalog_policy: ProviderCatalogPolicy::$catalog,
            catalog_registration: None,
        }
    };
}

const PROVIDER_DEFINITIONS: &[ProviderDefinition] = &[
    provider!(
        "openai-codex" => "openai-codex" @ "default",
        OpenAiCodexResponses, "https://chatgpt.com/backend-api/codex", [],
        OpenAiCodex, Default, OpenAiCodex, None, None, StaticOnly
    ),
    provider!(
        "openai" => "openai" @ "default",
        OpenAiResponses, "https://api.openai.com/v1", ["OPENAI_API_KEY"],
        OpenAi, Default, OpenAi, None, None, StaticOnly, HostedEarly
    ),
    provider!(
        "anthropic" => "anthropic" @ "default",
        AnthropicMessages, "https://api.anthropic.com", ["ANTHROPIC_AUTH_TOKEN"],
        Anthropic, Anthropic, Anthropic, None, None, StaticOnly, Core
    ),
    provider!(
        "gemini" => "gemini" @ "default",
        GeminiGenerateContent, "https://generativelanguage.googleapis.com/v1beta",
        ["GEMINI_API_KEY"], Gemini, Default, None, None, None, StaticOnly
    ),
    provider!(
        "arcee" => "arcee" @ "default",
        OpenAiChatCompletions, "https://api.arcee.ai/v1", ["ARCEE_API_KEY"],
        Generic, Default, None, None,
        discovery!(
            Required,
            ModelDiscoveryRoute::Fixed("https://api.arcee.ai/api/v1/models"),
            Arcee
        ),
        StaticAndDiscovery
    ),
    provider!(
        "byteplus" => "byteplus" @ "default",
        OpenAiChatCompletions, "https://ark.ap-southeast.bytepluses.com/api/v3",
        ["BYTEPLUS_API_KEY"], Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "byteplus-coding" => "byteplus" @ "coding",
        OpenAiChatCompletions, "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
        ["BYTEPLUS_CODING_API_KEY", "BYTEPLUS_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "chutes" => "chutes" @ "default",
        OpenAiChatCompletions, "https://llm.chutes.ai/v1", ["CHUTES_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "dashscope" => "dashscope" @ "default",
        AnthropicMessages, "https://dashscope.aliyuncs.com/apps/anthropic",
        ["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly
    ),
    provider!(
        "dashscope-token-plan" => "dashscope" @ "token-plan",
        AnthropicMessages, "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        ["DASHSCOPE_TOKEN_PLAN_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly
    ),
    provider!(
        "dashscope-coding-plan" => "dashscope" @ "coding-plan",
        AnthropicMessages, "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        ["DASHSCOPE_CODING_PLAN_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly
    ),
    provider!(
        "deepseek" => "deepseek" @ "default",
        AnthropicMessages, "https://api.deepseek.com/anthropic", ["DEEPSEEK_API_KEY"],
        Generic, AnthropicCompatible, DeepSeek, None, None, StaticOnly
    ),
    provider!(
        "fireworks" => "fireworks" @ "default",
        OpenAiChatCompletions, "https://api.fireworks.ai/inference/v1",
        ["FIREWORKS_API_KEY"], Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "huggingface" => "huggingface" @ "default",
        OpenAiChatCompletions, "https://router.huggingface.co/v1",
        ["HUGGINGFACE_API_KEY", "HF_TOKEN"], Generic, Default, None, None,
        discovery!(Optional, ModelDiscoveryRoute::OpenAiCompatible, HuggingFace),
        StaticAndDiscovery
    ),
    provider!(
        "kilocode" => "kilocode" @ "default",
        OpenAiChatCompletions, "https://api.kilo.ai/api/gateway", ["KILOCODE_API_KEY"],
        Generic, Default, None, None,
        discovery!(Optional, ModelDiscoveryRoute::OpenAiCompatible, Kilo),
        StaticAndDiscovery, KiloCode
    ),
    provider!(
        "litellm" => "litellm" @ "default",
        OpenAiChatCompletions, "http://localhost:4000", ["LITELLM_API_KEY"],
        Generic, Default, None, None,
        discovery!(Required, ModelDiscoveryRoute::OpenAiV1, OpenAiCompatible),
        DiscoveryOnly
    ),
    provider!(
        "mistral" => "mistral" @ "default",
        OpenAiChatCompletions, "https://api.mistral.ai/v1", ["MISTRAL_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "moonshot" => "moonshot" @ "default",
        OpenAiChatCompletions, "https://api.moonshot.ai/v1", ["MOONSHOT_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "nearai" => "nearai" @ "default",
        OpenAiChatCompletions, "https://cloud-api.near.ai/v1", ["NEARAI_API_KEY"],
        Generic, Default, None, None,
        discovery!(Optional, ModelDiscoveryRoute::OpenAiCompatible, NearAi),
        StaticAndDiscovery
    ),
    provider!(
        "nvidia" => "nvidia" @ "default",
        OpenAiChatCompletions, "https://integrate.api.nvidia.com/v1", ["NVIDIA_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "opencode-go" => "opencode-go" @ "default",
        OpenAiChatCompletions, "https://opencode.ai/zen/go/v1", ["OPENCODE_GO_API_KEY"],
        Generic, Default, None, None,
        discovery!(Optional, ModelDiscoveryRoute::OpenAiCompatible, OpenCodeGo),
        StaticAndDiscovery, OpenCodeGo
    ),
    provider!(
        "opencode-go-messages" => "opencode-go" @ "messages",
        AnthropicMessages, "https://opencode.ai/zen/go/v1", ["OPENCODE_GO_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly
    ),
    provider!(
        "openrouter" => "openrouter" @ "default",
        OpenAiChatCompletions, "https://openrouter.ai/api/v1", ["OPENROUTER_API_KEY"],
        Generic, Default, None, None,
        discovery!(Required, ModelDiscoveryRoute::OpenAiCompatible, OpenRouter),
        StaticAndDiscovery
    ),
    provider!(
        "qianfan" => "qianfan" @ "default",
        OpenAiChatCompletions, "https://qianfan.baidubce.com/v2", ["QIANFAN_API_KEY"],
        Generic, Default, None, None, None, StaticOnly, ChinaEarly
    ),
    provider!(
        "stepfun" => "stepfun" @ "default",
        OpenAiChatCompletions, "https://api.stepfun.com/v1", ["STEPFUN_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "stepfun-plan" => "stepfun" @ "plan",
        OpenAiChatCompletions, "https://api.stepfun.com/step_plan/v1",
        ["STEPFUN_PLAN_API_KEY", "STEPFUN_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "synthetic" => "synthetic" @ "default",
        AnthropicMessages, "https://api.synthetic.new/anthropic", ["SYNTHETIC_API_KEY"],
        Generic, AnthropicCompatible, None, None,
        discovery!(
            Optional,
            ModelDiscoveryRoute::Fixed("https://api.synthetic.new/openai/v1/models"),
            Synthetic
        ),
        StaticAndDiscovery, HostedLate
    ),
    provider!(
        "tencent-tokenhub" => "tencent-tokenhub" @ "default",
        OpenAiChatCompletions, "https://tokenhub.tencentmaas.com/v1", ["TOKENHUB_API_KEY"],
        Generic, Default, None, None,
        discovery!(
            Required,
            ModelDiscoveryRoute::OpenAiCompatible,
            TencentTokenHub
        ),
        StaticAndDiscovery, TencentTokenHub
    ),
    provider!(
        "tencent-tokenhub-messages" => "tencent-tokenhub" @ "messages",
        AnthropicMessages, "https://tokenhub.tencentmaas.com", ["TOKENHUB_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly
    ),
    provider!(
        "together" => "together" @ "default",
        OpenAiChatCompletions, "https://api.together.xyz/v1", ["TOGETHER_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "venice" => "venice" @ "default",
        OpenAiChatCompletions, "https://api.venice.ai/api/v1", ["VENICE_API_KEY"],
        Generic, Default, None, None,
        discovery!(Optional, ModelDiscoveryRoute::Venice, Venice),
        StaticAndDiscovery
    ),
    provider!(
        "vllm" => "vllm" @ "default",
        OpenAiChatCompletions, "http://127.0.0.1:8000/v1", [],
        Generic, Default, None, None,
        discovery!(
            Optional,
            ModelDiscoveryRoute::OpenAiCompatible,
            OpenAiCompatible
        ),
        DiscoveryOnly
    ),
    provider!(
        "volcengine" => "volcengine" @ "default",
        OpenAiResponses, "https://ark.cn-beijing.volces.com/api/v3",
        ["VOLCENGINE_API_KEY", "VOLCENGINE_IMAGE_OPENAI_API_KEY"],
        Generic, Default, None, None, None, StaticOnly, ChinaLate
    ),
    provider!(
        "volcengine-coding" => "volcengine" @ "coding",
        OpenAiResponses, "https://ark.cn-beijing.volces.com/api/coding/v3",
        ["VOLCENGINE_CODING_API_KEY"], Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "volcengine-agent" => "volcengine" @ "plan",
        OpenAiResponses, "https://ark.cn-beijing.volces.com/api/plan/v3",
        ["VOLCENGINE_AGENT_API_KEY"], Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "volcengine-image-openai" => "volcengine" @ "default",
        OpenAiResponses, "https://ark.cn-beijing.volces.com/api/v3", [],
        AliasOnly, Default, None, None, None, StaticOnly
    ),
    provider!(
        "xiaomi" => "xiaomi" @ "default",
        OpenAiResponses, "https://api.xiaomimimo.com/v1", ["XIAOMI_API_KEY"],
        Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "xiaomi-token-plan" => "xiaomi" @ "token-plan",
        OpenAiResponses, "https://token-plan-cn.xiaomimimo.com/v1",
        ["XIAOMI_TOKEN_PLAN_API_KEY"], Generic, Default, None, None, None, StaticOnly
    ),
    provider!(
        "xai" => "xai" @ "default",
        OpenAiResponses, "https://api.x.ai/v1", ["XAI_API_KEY"],
        Generic, Default, None, Some("medium"), None, StaticOnly
    ),
    provider!(
        "zai" => "zai" @ "default",
        AnthropicMessages, "https://api.z.ai/api/anthropic", ["ZAI_API_KEY"],
        Generic, AnthropicCompatible, Zai, None, None, StaticOnly
    ),
    provider!(
        "bigmodel" => "bigmodel" @ "default",
        AnthropicMessages, "https://open.bigmodel.cn/api/anthropic", ["BIGMODEL_API_KEY"],
        Generic, AnthropicCompatible, BigModel, None, None, StaticOnly
    ),
    provider!(
        "minimax" => "minimax" @ "default",
        AnthropicMessages, "https://api.minimax.io/anthropic", ["MINIMAX_API_KEY"],
        Generic, AnthropicCompatible, None, None, None, StaticOnly, HostedMiddle
    ),
    provider!(
        "vercel-ai-gateway" => "vercel-ai-gateway" @ "default",
        AnthropicMessages, "https://ai-gateway.vercel.sh",
        ["VERCEL_OIDC_TOKEN", "AI_GATEWAY_API_KEY", "VERCEL_AI_GATEWAY_API_KEY"],
        VercelAiGateway, AnthropicCompatible, None, None,
        discovery!(
            Optional,
            ModelDiscoveryRoute::VercelAiGateway,
            VercelAiGateway
        ),
        StaticAndDiscovery
    ),
];

pub(crate) fn provider_definitions() -> &'static [ProviderDefinition] {
    PROVIDER_DEFINITIONS
}

pub(crate) fn provider_definition(legacy_provider: &str) -> Option<&'static ProviderDefinition> {
    PROVIDER_DEFINITIONS
        .iter()
        .find(|definition| definition.legacy_provider == legacy_provider)
}

//! Reviewed TokenHub inventory used to classify discovered model IDs.

pub type TencentTokenHubModelSpec = (
    &'static str,
    &'static str,
    Option<usize>,
    Option<u32>,
    bool,
    bool,
);

pub const TENCENT_TOKENHUB_MODELS: &[TencentTokenHubModelSpec] = &[
    ("hy3", "Hy3", Some(256_000), Some(128_000), true, false),
    ("hy4", "Hy4", None, None, true, true),
    ("hy4-flash", "Hy4 Flash", None, None, true, true),
    ("hy-mt2-pro", "Hy-MT2-Pro", None, None, false, false),
    ("hy-mt2-plus", "Hy-MT2-Plus", None, None, false, false),
    ("hy-mt2-lite", "Hy-MT2-Lite", None, None, false, false),
    (
        "hunyuan-role-latest",
        "Hy-Role-Latest",
        None,
        None,
        false,
        false,
    ),
    ("hy-role", "Hy-Role", None, None, false, false),
    (
        "deepseek-v4-pro",
        "DeepSeek V4 Pro",
        Some(1_000_000),
        Some(384_000),
        true,
        false,
    ),
    (
        "glm-5.2",
        "GLM-5.2",
        Some(1_000_000),
        Some(131_072),
        true,
        false,
    ),
    (
        "kimi-k2.7-code-highspeed",
        "Kimi K2.7 Code HighSpeed",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "kimi-k2.6",
        "Kimi K2.6",
        Some(262_144),
        Some(262_144),
        true,
        true,
    ),
    (
        "minimax-m3",
        "MiniMax M3",
        Some(1_000_000),
        None,
        true,
        true,
    ),
    (
        "minimax-m2.7",
        "MiniMax M2.7",
        Some(204_800),
        None,
        true,
        false,
    ),
    (
        "hy-vision-2.0-instruct",
        "HY-Vision-2.0-Instruct",
        None,
        None,
        false,
        true,
    ),
    (
        "hunyuan-t1-vision-20250916",
        "HY-Vision-1.5-Thinking",
        None,
        None,
        true,
        true,
    ),
];

// The TokenHub protocol matrix confirms Chat and Messages for these IDs, but
// does not itself establish exact limits or capabilities. Separate Chat
// guides below refine only the reviewed Chat metadata.
// https://cloud.tencent.com/document/product/1823/130079
pub const TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS: &[&str] = &[
    "hy4-preview",
    "deepseek/deepseek-flash",
    "deepseek-v4-flash-202605",
    "deepseek/deepseek-v4-flash-0731",
    "deepseek/deepseek-v4-flash",
    "deepseek-v4-pro-202606",
    "deepseek/deepseek-v4-pro-0813",
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash-vision-exp",
    "deepseek-v4-pro-0813",
    "deepseek-v4.1-flash",
    "deepseek-v4.1-flash-1017",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.3-flashx",
    "kimi-k3",
    "kimi-k3.1-pro",
    "kimi-k3.1-pro-highspeed",
    "kimi-k2.8-preview",
    "mimo-v2.6-pro",
    "mimo-v2.6-flash",
    "minimax-m3.1-flash",
    "step-5-preview",
];

pub fn is_tencent_tokenhub_model_id(id: &str) -> bool {
    TENCENT_TOKENHUB_MODELS.iter().any(|model| model.0 == id)
        || TENCENT_TOKENHUB_ADDITIONAL_MODEL_IDS.contains(&id)
}

//! Opt-in native Gemini image acceptance; never run by the default test suite.
//!
//! HOLON_LIVE_GEMINI_IMAGE_MODEL='gemini@default/<image-capable-model>' \
//!   cargo test --test live_gemini_image -- --ignored --nocapture --test-threads=1
//!
//! HOLON_LIVE_GEMINI_IMAGE_MAX_OUTPUT_TOKENS defaults to 1024 (64..=4096).
//! Every probe caps its own model HTTP dispatches: one for the single-shot
//! probes and two for the multi-turn session probes.
//! ViewImage uses its production validation retry policy, without an extra
//! model call to select the tool. No external images or image generation.
//! Gemini spends part of its output budget on reasoning, so the multi-turn
//! history probes need at least 1024 output tokens to finish their answer.
//! The session probes drive the public RuntimeHost/RuntimeHandle path, so the
//! runtime assembles current and historical image attachments itself.

mod support;

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use holon::{
    config::{
        AppConfig, ModelRouteCapability, ModelRouteRef, ProviderId, ProviderTransportKind,
        RuntimeModelCatalog,
    },
    context::ContextConfig,
    host::RuntimeHost,
    provider::{
        build_provider_from_model_chain, AgentProvider, ConversationMessage, ModelBlock,
        ProviderContextManagementPolicy, ProviderDispatchGuard, ProviderPromptCapability,
        ProviderTurnRequest, ProviderTurnResponse,
    },
    run_once::{run_once_with_host, RunFinalStatus, RunOnceRequest},
    runtime::RuntimeHandle,
    tool::{ToolCall, ToolRegistry},
    types::{
        AgentState, AuthorityClass, MessageBody, MessageEnvelope, MessageKind, MessageOrigin,
        Priority,
    },
};
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    DynamicImage, ImageEncoder, ImageFormat, Rgb, RgbImage,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

const MODEL_ENV: &str = "HOLON_LIVE_GEMINI_IMAGE_MODEL";
const OUTPUT_ENV: &str = "HOLON_LIVE_GEMINI_IMAGE_MAX_OUTPUT_TOKENS";
const PAIR_INSTRUCTIONS: &str = r#"Inspect the actual attached image pixels.
Return only JSON: {"first":{"color":"COLOR","shape":"SHAPE","count":0},"second":{"color":"COLOR","shape":"SHAPE","count":0}}.
Use red, green, blue, or yellow; circle or square; and the visible integer count.
The placeholders and zero counts are not answers. Never infer content from paths.
Use the image order and the surrounding text to assign first and second.
Repeated references are the same image, not additional objects to add.
Do not call tools, create work items, or provide prose."#;

struct LiveConfig {
    config: AppConfig,
    route: ModelRouteRef,
    max_output_tokens: u32,
    workspace: TempDir,
    _data: TempDir,
}

fn live_config() -> Result<LiveConfig> {
    let raw = std::env::var(MODEL_ENV)
        .with_context(|| format!("set {MODEL_ENV} to an explicit native Gemini route"))?;
    let requested = ModelRouteRef::parse(&raw)
        .map_err(|_| anyhow!("invalid {MODEL_ENV}; expected provider@endpoint/model"))?;
    ensure!(
        requested.provider == ProviderId::gemini(),
        "this probe requires the native Gemini provider, not a compatible proxy"
    );
    let max_output = std::env::var(OUTPUT_ENV)
        .unwrap_or_else(|_| "1024".into())
        .parse::<u32>()
        .with_context(|| format!("{OUTPUT_ENV} must be an integer"))?;
    ensure!(
        (64..=4096).contains(&max_output),
        "{OUTPUT_ENV} must be 64..=4096"
    );
    let mut config =
        AppConfig::load().map_err(|_| anyhow!("failed to load configured live Holon routes"))?;
    let route = RuntimeModelCatalog::from_config(&config)
        .resolve_explicit_model_route(
            &ContextConfig::default(),
            &requested,
            ModelRouteCapability::VisionObservation,
        )
        .context("explicit Gemini route must support native image observation")?;
    ensure!(
        route.provider_config().transport == ProviderTransportKind::GeminiGenerateContent
            && route.capabilities.agent_turn
            && route.capabilities.image_input,
        "explicit route must support agent turns and native Gemini image input"
    );
    ensure!(
        route.provider_config().has_configured_credential(),
        "explicit Gemini route has no configured credential"
    );
    let policy = config
        .validated_model_overrides
        .entry(route.model_ref.clone())
        .or_default();
    policy.runtime_max_output_tokens = Some(max_output);
    // A multi-MiB image must fit by visual reserve, not base64 text length.
    policy.context_window_tokens = Some(65_536);
    config.runtime_max_output_tokens = max_output;
    config.default_model = route.route_ref.clone();
    config.vision_model = Some(route.route_ref.clone());
    config.vision_candidate_models.clear();
    config.fallback_models.clear();
    config.disable_provider_fallback = true;
    config.default_agent_id = "default".into();
    let data = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    support::init_git_repo(workspace.path())?;
    config.home_dir = data.path().to_path_buf();
    config.data_dir = data.path().to_path_buf();
    config.config_file_path = data.path().join("config.json");
    config.socket_path = data.path().join("run/holon.sock");
    config.workspace_dir = workspace.path().to_path_buf();
    Ok(LiveConfig {
        config,
        route: route.route_ref,
        max_output_tokens: max_output,
        workspace,
        _data: data,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Observation {
    color: String,
    shape: String,
    count: u8,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pair {
    first: Observation,
    second: Observation,
}

struct Fixture {
    bytes: Vec<u8>,
    expected: Observation,
}

fn fixture(seed: u8, width: u32, height: u32, uncompressed: bool) -> Result<Fixture> {
    let colors = [
        ("red", [235, 20, 20]),
        ("green", [15, 170, 30]),
        ("blue", [20, 45, 235]),
        ("yellow", [245, 220, 10]),
    ];
    let (color, rgb) = colors[usize::from(seed % 4)];
    let square = seed & 4 == 0;
    let count = 1 + (seed / 8) % 3;
    let mut pixels = RgbImage::from_pixel(width, height, Rgb([255, 255, 255]));
    let radius = height.min(width / 2) / 6;
    for index in 0..u32::from(count) {
        let cx = width * (index + 1) / (u32::from(count) + 1);
        let cy = height / 2;
        for y in (cy - radius)..=(cy + radius) {
            for x in (cx - radius)..=(cx + radius) {
                let dx = i64::from(x) - i64::from(cx);
                let dy = i64::from(y) - i64::from(cy);
                if square || dx * dx + dy * dy <= i64::from(radius).pow(2) {
                    pixels.put_pixel(x, y, Rgb(rgb));
                }
            }
        }
    }
    let mut bytes = Cursor::new(Vec::new());
    if uncompressed {
        PngEncoder::new_with_quality(
            &mut bytes,
            CompressionType::Uncompressed,
            FilterType::NoFilter,
        )
        .write_image(
            pixels.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )?;
    } else {
        DynamicImage::ImageRgb8(pixels).write_to(&mut bytes, ImageFormat::Png)?;
    }
    Ok(Fixture {
        bytes: bytes.into_inner(),
        expected: Observation {
            color: color.into(),
            shape: if square { "square" } else { "circle" }.into(),
            count,
        },
    })
}

fn image_message(image: &Fixture) -> ConversationMessage {
    ConversationMessage::UserImage {
        prompt: "Inspect this image; its surrounding text assigns its output key.".into(),
        media_type: "image/png".into(),
        data_base64: BASE64_STANDARD.encode(&image.bytes).into(),
        optional_history: false,
    }
}

fn response_text(blocks: &[ModelBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_pair(text: &str) -> Result<Pair> {
    let trimmed = text.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str(json).map_err(|_| {
        anyhow!("Gemini did not return the requested visual JSON; raw response omitted")
    })
}

/// Caps how many real provider HTTP dispatches a live probe may spend.
#[derive(Debug)]
struct DispatchBudget {
    limit: usize,
    count: AtomicUsize,
}

impl DispatchBudget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            count: AtomicUsize::new(0),
        })
    }

    fn used(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl ProviderDispatchGuard for DispatchBudget {
    fn before_dispatch(&self) -> Result<()> {
        let used = self.count.fetch_add(1, Ordering::SeqCst);
        if used >= self.limit {
            return Err(anyhow!(
                "live Gemini probe allows only {} model HTTP dispatch(es)",
                self.limit
            ));
        }
        Ok(())
    }

    fn allows_auxiliary_dispatch(&self) -> Result<bool> {
        Ok(false)
    }
}

#[tokio::test]
#[ignore = "paid native Gemini ViewImage; explicit HOLON_LIVE_GEMINI_IMAGE_MODEL required"]
async fn live_gemini_view_image_observes_random_pixels() -> Result<()> {
    let live = live_config()?;
    let seed = Uuid::new_v4().as_bytes()[0];
    let image = fixture(seed, 640, 384, false)?;
    let image_path = live.workspace.path().join("reference.png");
    std::fs::write(&image_path, &image.bytes)?;
    let host = RuntimeHost::new(live.config)?;
    support::attach_default_workspace(&host).await?;
    let runtime = host.default_runtime().await?;
    let call = ToolCall {
        id: "gemini-image-acceptance".into(),
        name: "ViewImage".into(),
        input: json!({
            "path": image_path,
            "model": live.route.as_string(),
            "prompt": "Inspect the pixels. In the required visual_observation JSON, set summary \
                to exactly 'COLOR SHAPE COUNT', using red/green/blue/yellow, singular circle/square, \
                and the visible integer count. For example, the format has three space-separated \
                words, not a filename. No speculation or extra summary words."
        }),
    };
    let executed = tokio::time::timeout(
        Duration::from_secs(120),
        ToolRegistry::new(live.workspace.path().to_path_buf()).execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
        ),
    )
    .await;
    host.shutdown().await?;
    let (output, _) = executed.context("Gemini ViewImage timed out")??;
    ensure!(
        output.envelope.error.is_none(),
        "production ViewImage failed; provider response omitted"
    );
    let result = output.envelope.result.context("missing ViewImage result")?;
    ensure!(
        result["cache_hit"] == false,
        "fresh image unexpectedly cached"
    );
    ensure!(
        result["vision_selection"]["resolved_model"] == live.route.as_string()
            && result["observation"]["generated_by"]["resolved_model"] == live.route.as_string(),
        "ViewImage did not use the explicit native Gemini route"
    );
    ensure!(
        result["observation"]["type"] == "visual_observation"
            && result["observation"]["schema"] == "visual_observation.v1",
        "ViewImage did not return a real visual observation"
    );
    let expected = format!(
        "{} {} {}",
        image.expected.color, image.expected.shape, image.expected.count
    );
    ensure!(
        result["observation"]["summary"]
            .as_str()
            .is_some_and(|summary| summary.trim().eq_ignore_ascii_case(&expected)),
        "ViewImage summary did not identify the random visible shapes"
    );
    eprintln!(
        "live_gemini_image scenario=view_image route={} verified={expected}",
        live.route.as_string()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "paid native Gemini ordered multi-image probe; one model HTTP dispatch"]
async fn live_gemini_turn_preserves_image_and_text_associations() -> Result<()> {
    let live = live_config()?;
    let seed = Uuid::new_v4().as_bytes()[0];
    let first = fixture(seed, 640, 384, false)?;
    let second = fixture(seed ^ 7, 640, 384, false)?;
    let provider =
        build_provider_from_model_chain(&live.config, std::slice::from_ref(&live.route))?;
    let mut request = ProviderTurnRequest::plain(
        PAIR_INSTRUCTIONS,
        vec![
            ConversationMessage::UserText("The next image belongs to key second.".into()),
            image_message(&second),
            ConversationMessage::UserText(
                "The preceding image was second. The next image belongs to key first.".into(),
            ),
            image_message(&first),
            ConversationMessage::UserText(
                "Return first and second as assigned by the surrounding text, not arrival order."
                    .into(),
            ),
        ],
        vec![],
    );
    let guard = DispatchBudget::new(1);
    request.dispatch_guard = Some(guard.clone());
    let response = tokio::time::timeout(Duration::from_secs(120), provider.complete_turn(request))
        .await
        .context("ordered native Gemini image request timed out")?
        .map_err(|_| anyhow!("ordered native Gemini image request failed; response omitted"))?;
    let observed = parse_pair(&response_text(&response.blocks))?;
    ensure!(
        observed
            == Pair {
                first: first.expected,
                second: second.expected,
            },
        "native Gemini lost image content or surrounding-text association"
    );
    ensure!(guard.used() == 1, "unexpected dispatch count");
    eprintln!(
        "live_gemini_image scenario=ordered_pair route={} input_tokens={} output_tokens={} verified={}",
        live.route.as_string(),
        response.input_tokens,
        response.output_tokens,
        serde_json::to_string(&observed)?
    );
    Ok(())
}

#[derive(Debug)]
struct BoundedDispatch {
    limit: Arc<DispatchBudget>,
    runtime_guard: Option<Arc<dyn ProviderDispatchGuard>>,
}

impl ProviderDispatchGuard for BoundedDispatch {
    fn before_dispatch(&self) -> Result<()> {
        if let Some(guard) = &self.runtime_guard {
            guard.before_dispatch()?;
        }
        self.limit.before_dispatch()
    }

    fn allows_auxiliary_dispatch(&self) -> Result<bool> {
        Ok(false)
    }
}

#[derive(Debug, Clone)]
struct Attachment {
    label: String,
    decoded_bytes: usize,
    optional_history: bool,
}

// This observer forwards production requests to the real native Gemini provider.
// It never supplies canned responses or calls private image-budget helpers.
struct ObservedProvider {
    inner: Arc<dyn AgentProvider>,
    dispatches: Arc<DispatchBudget>,
    calls: AtomicUsize,
    completed: AtomicUsize,
    errors: Mutex<Vec<String>>,
    /// One entry per provider call, in call order.
    attachments: Mutex<Vec<Vec<Attachment>>>,
    /// One entry per provider call: the conversation text blocks it received.
    texts: Mutex<Vec<Vec<String>>>,
    /// One entry per provider call: the assistant text the model returned.
    responses: Mutex<Vec<String>>,
}

#[async_trait]
impl AgentProvider for ObservedProvider {
    async fn complete_turn(
        &self,
        mut request: ProviderTurnRequest,
    ) -> Result<ProviderTurnResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = async {
            let attachments = request
                .conversation
                .iter()
                .filter_map(|message| match message {
                    ConversationMessage::UserImage {
                        prompt,
                        data_base64,
                        optional_history,
                        ..
                    } => Some((prompt.clone(), data_base64.clone(), *optional_history)),
                    _ => None,
                })
                .map(|(label, data, optional_history)| {
                    Ok(Attachment {
                        label,
                        decoded_bytes: BASE64_STANDARD.decode(data.as_bytes())?.len(),
                        optional_history,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let texts = request
                .conversation
                .iter()
                .filter_map(|message| match message {
                    ConversationMessage::UserText(text) => Some(text.clone()),
                    ConversationMessage::UserBlocks(blocks) => Some(
                        blocks
                            .iter()
                            .map(|block| block.text.clone())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>();
            self.attachments.lock().unwrap().push(attachments);
            self.texts.lock().unwrap().push(texts);
            request.dispatch_guard = Some(Arc::new(BoundedDispatch {
                limit: self.dispatches.clone(),
                runtime_guard: request.dispatch_guard.take(),
            }));
            let response = self.inner.complete_turn(request).await?;
            ensure!(
                response
                    .blocks
                    .iter()
                    .all(|block| !matches!(block, ModelBlock::ToolUse { .. })),
                "the visual acceptance request must not execute tools"
            );
            let text = response_text(&response.blocks);
            ensure!(!text.trim().is_empty(), "the visual response was empty");
            self.responses.lock().unwrap().push(text);
            Ok(response)
        }
        .await;
        if let Err(error) = &result {
            self.errors
                .lock()
                .unwrap()
                .push(format!("the observed provider call failed: {error:#}"));
        }
        self.completed.fetch_add(1, Ordering::SeqCst);
        result
    }

    fn resolved_image_input_support(&self) -> Option<bool> {
        self.inner.resolved_image_input_support()
    }

    fn prompt_capabilities(&self) -> Vec<ProviderPromptCapability> {
        self.inner.prompt_capabilities()
    }

    fn context_management_policy(&self) -> Option<ProviderContextManagementPolicy> {
        self.inner.context_management_policy()
    }
}

async fn runtime_case(
    live: &LiveConfig,
    provider: Arc<ObservedProvider>,
    text: String,
) -> Result<holon::run_once::RunOnceResponse> {
    let host = RuntimeHost::new_with_provider(live.config.clone(), provider)?;
    let request = RunOnceRequest {
        text,
        authority_class: AuthorityClass::OperatorInstruction,
        agent_id: None,
        create_agent: false,
        template: None,
        max_turns: Some(1),
        timeout_seconds: Some(120),
        wait_for_tasks: false,
        workspace_root: Some(live.workspace.path().to_path_buf()),
        cwd: None,
    };
    let result = tokio::time::timeout(
        Duration::from_secs(150),
        run_once_with_host(host.clone(), request),
    )
    .await;
    host.shutdown().await?;
    result.context("native Gemini runtime acceptance timed out")?
}

fn write_reference(workspace: &Path, filename: &str, image: &Fixture) -> Result<String> {
    let path = workspace.join(filename);
    std::fs::write(&path, &image.bytes)?;
    Ok(format!("![reference]({})", path.display()))
}

#[tokio::test]
#[ignore = "paid native Gemini full-runtime budget probe; one model HTTP dispatch, two local rejections"]
async fn live_gemini_runtime_deduplicates_images_and_enforces_attachment_budget() -> Result<()> {
    let live = live_config()?;
    let seed = Uuid::new_v4().as_bytes()[0];
    let first = fixture(seed, 2048, 1024, true)?;
    let second = fixture(seed ^ 7, 640, 384, false)?;
    ensure!(
        first.bytes.len() > 6 * 1024 * 1024,
        "large fixture must exceed the bounded context when miscounted as base64 text"
    );
    let first_path = live.workspace.path().join("a.png");
    let alias_path = live.workspace.path().join("copy.png");
    let second_path = live.workspace.path().join("b.png");
    std::fs::write(&first_path, &first.bytes)?;
    std::fs::write(&alias_path, &first.bytes)?;
    std::fs::write(&second_path, &second.bytes)?;
    let first_reference = format!("![first]({})", first_path.display());
    let repeated = std::iter::repeat_n(first_reference, 9)
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!(
        "{PAIR_INSTRUCTIONS}\nThe following repeated references are first:\n{repeated}\n\
         Same first image under another path: ![first]({})\n\
         The distinct second image is ![second]({})\n\
         Return the required first/second JSON from their pixels only.",
        alias_path.display(),
        second_path.display()
    );
    let provider = Arc::new(ObservedProvider {
        inner: build_provider_from_model_chain(&live.config, std::slice::from_ref(&live.route))?,
        dispatches: DispatchBudget::new(1),
        calls: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
        errors: Mutex::new(Vec::new()),
        attachments: Mutex::new(Vec::new()),
        texts: Mutex::new(Vec::new()),
        responses: Mutex::new(Vec::new()),
    });
    let response = runtime_case(&live, provider.clone(), text).await?;
    ensure!(
        response.final_status == RunFinalStatus::Completed,
        "runtime did not complete the large-image visual probe"
    );
    ensure!(
        parse_pair(&response.final_text)?
            == Pair {
                first: first.expected,
                second: second.expected,
            },
        "runtime lost the large image or returned incorrect visual content"
    );
    {
        let calls = provider.attachments.lock().unwrap();
        let attachments = calls.last().context("runtime did not reach the provider")?;
        ensure!(
            attachments.len() == 2
                && attachments[0].label == "first"
                && attachments[1].label == "second"
                && attachments[0].decoded_bytes == first.bytes.len()
                && attachments[1].decoded_bytes == second.bytes.len(),
            "runtime must deduplicate both repeated paths and identical file contents"
        );
    }
    ensure!(
        provider.calls.load(Ordering::SeqCst) == 1 && provider.dispatches.used() == 1,
        "expected one successful real Gemini dispatch"
    );
    for scenario in ["count_overflow", "decoded_bytes_overflow"] {
        let rejected = live_config()?;
        let references = if scenario == "count_overflow" {
            (0..9)
                .map(|index| {
                    let image = fixture(index, 320, 192, false)?;
                    write_reference(rejected.workspace.path(), &format!("{index}.png"), &image)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            // Each image is valid and individually below ViewImage's 20 MiB limit.
            let a = fixture(seed, 3072, 1280, true)?;
            let b = fixture(seed ^ 7, 3072, 1280, true)?;
            ensure!(
                a.bytes.len() < 20 * 1024 * 1024
                    && b.bytes.len() < 20 * 1024 * 1024
                    && a.bytes.len() + b.bytes.len() > 20 * 1024 * 1024,
                "fixtures must cross only the aggregate decoded-byte budget"
            );
            vec![
                write_reference(rejected.workspace.path(), "a.png", &a)?,
                write_reference(rejected.workspace.path(), "b.png", &b)?,
            ]
        };
        let result = runtime_case(
            &rejected,
            provider.clone(),
            format!(
                "Inspect all current required images. Do not call tools.\n{}",
                references.join("\n")
            ),
        )
        .await?;
        let artifact = result
            .failure_artifact
            .as_ref()
            .context("the runtime did not record a failure artifact")?;
        ensure!(
            result.final_status == RunFinalStatus::Failed
                && artifact.kind == "image_budget_exceeded",
            "runtime did not report the attachment-budget rejection for {scenario}: \
             status={:?} final_text={:?} artifact_kind={:?}",
            result.final_status,
            result.final_text,
            artifact.kind
        );
        ensure!(
            provider.calls.load(Ordering::SeqCst) == 1 && provider.dispatches.used() == 1,
            "over-budget current images reached the provider for {scenario}"
        );
        eprintln!(
            "live_gemini_image scenario={scenario} runtime_rejected=true additional_dispatches=0"
        );
    }
    eprintln!(
        "live_gemini_image scenario=runtime_budget route={} unique_images=2 decoded_bytes={} \
         input_tokens={} output_tokens={} runtime_builder=exercised",
        live.route.as_string(),
        first.bytes.len() + second.bytes.len(),
        response.input_tokens,
        response.output_tokens
    );
    Ok(())
}

const HISTORY_INSTRUCTIONS: &str = r#"Inspect the actual attached image pixels.
Return only JSON: {"alpha":{"color":"COLOR","shape":"SHAPE","count":0},"beta":{"color":"COLOR","shape":"SHAPE","count":0}}.
Use red, green, blue, or yellow; circle or square; and the visible integer count.
Every attached image carries its label as the accompanying text; use that label as the JSON key.
The placeholders and zero counts are not answers. Never infer content from paths.
Do not call tools, create work items, or provide prose."#;

const BUDGET_INSTRUCTIONS: &str = r#"List the labels of the attached images, one label per line.
Do not call tools, create work items, or provide any other text."#;

fn parse_labeled(text: &str) -> Result<BTreeMap<String, Observation>> {
    let trimmed = text.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| {
        anyhow!(
            "Gemini did not return the requested visual JSON; raw response: {}",
            json.chars().take(400).collect::<String>()
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("the visual JSON was not an object"))?;
    let mut labeled = BTreeMap::new();
    for (label, entry) in object {
        let observation: Observation = serde_json::from_value(entry.clone())
            .map_err(|_| anyhow!("the visual JSON entry {label} was malformed"))?;
        labeled.insert(label.clone(), observation);
    }
    Ok(labeled)
}

/// Gemini spends part of its output budget on reasoning, so the multi-turn
/// probes need enough room to finish the requested answer.
fn ensure_output_budget(live: &LiveConfig) -> Result<()> {
    ensure!(
        live.max_output_tokens >= 1024,
        "{OUTPUT_ENV} must be at least 1024 so Gemini can finish its answer; got {}",
        live.max_output_tokens
    );
    Ok(())
}

/// Drives one persistent runtime session across several operator turns.
struct RuntimeSession {
    host: RuntimeHost,
    runtime: RuntimeHandle,
    agent_id: String,
    provider: Arc<ObservedProvider>,
}

fn runtime_turn_completed(state: &AgentState, previous_turn_index: u64) -> Result<bool> {
    let Some(terminal) = state
        .last_turn_terminal
        .as_ref()
        .filter(|terminal| terminal.turn_index > previous_turn_index)
    else {
        return Ok(false);
    };
    ensure!(
        !terminal.kind.is_failure(),
        "the runtime turn failed: kind={:?} reason={:?}",
        terminal.kind,
        terminal.reason
    );
    // A settled turn keeps `current_turn_id` pointing at its terminal turn, so
    // the settled signal is a fresh successful terminal record plus an empty
    // queue and no active run.
    Ok(state.pending == 0 && state.current_run_id.is_none())
}

#[test]
fn session_completion_requires_a_new_successful_settled_terminal() {
    use holon::types::{TurnTerminalKind, TurnTerminalRecord};

    let mut state = AgentState::new("live-test");
    assert!(!runtime_turn_completed(&state, 1).unwrap());
    state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_id: "turn-1".into(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: chrono::Utc::now(),
        duration_ms: 0,
    });
    assert!(!runtime_turn_completed(&state, 1).unwrap());
    state.last_turn_terminal.as_mut().unwrap().turn_index = 2;
    state.current_run_id = Some("run-2".into());
    assert!(!runtime_turn_completed(&state, 1).unwrap());
    state.current_run_id = None;
    state.pending = 1;
    assert!(!runtime_turn_completed(&state, 1).unwrap());
    state.pending = 0;
    // The runtime keeps the terminal turn as the current turn, so a retained
    // `current_turn_id` must not block the settled check.
    state.current_turn_id = Some("turn-2".into());
    assert!(runtime_turn_completed(&state, 1).unwrap());
    state.last_turn_terminal.as_mut().unwrap().kind = TurnTerminalKind::ProviderFailedNeedsRecovery;
    assert!(runtime_turn_completed(&state, 1).is_err());
}

impl RuntimeSession {
    async fn start(live: &LiveConfig, provider: Arc<ObservedProvider>) -> Result<Self> {
        let host = RuntimeHost::new_with_provider(live.config.clone(), provider.clone())?;
        support::attach_default_workspace(&host).await?;
        let runtime = host.default_runtime().await?;
        Ok(Self {
            host,
            runtime,
            agent_id: live.config.default_agent_id.clone(),
            provider,
        })
    }

    /// Sends one operator prompt and waits until that runtime turn finished.
    async fn prompt(&self, text: &str, expected_calls: usize) -> Result<()> {
        let previous_turn_index = self.runtime.agent_state().await?.turn_index;
        self.runtime
            .enqueue(MessageEnvelope::new(
                &self.agent_id,
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: text.to_string(),
                },
            ))
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
        loop {
            let state = self.runtime.agent_state().await?;
            if let Some(error) = self.provider.errors.lock().unwrap().first().cloned() {
                anyhow::bail!("the live Gemini provider call failed: {error}");
            }
            let completed = self.provider.completed.load(Ordering::SeqCst);
            if runtime_turn_completed(&state, previous_turn_index)? && completed >= expected_calls {
                ensure!(
                    completed == expected_calls
                        && self.provider.responses.lock().unwrap().len() == expected_calls,
                    "the runtime turn did not record exactly the expected successful responses"
                );
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for the live Gemini runtime turn \
                     (completed={completed}, pending={}, status={:?})",
                    state.pending,
                    state.status
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn finish(self) -> Result<()> {
        self.host.shutdown().await
    }
}

#[tokio::test]
#[ignore = "paid native Gemini multi-turn history probe; two model HTTP dispatches"]
async fn live_gemini_runtime_deduplicates_images_across_session_history() -> Result<()> {
    let live = live_config()?;
    ensure_output_budget(&live)?;
    let seed = Uuid::new_v4().as_bytes()[0];
    let alpha = fixture(seed, 640, 384, false)?;
    let beta = fixture(seed ^ 7, 640, 384, false)?;
    ensure!(
        alpha.bytes != beta.bytes,
        "the two fixtures must differ so the runtime cannot collapse them"
    );
    let alpha_path = live.workspace.path().join("alpha.png");
    let beta_path = live.workspace.path().join("beta.png");
    std::fs::write(&alpha_path, &alpha.bytes)?;
    std::fs::write(&beta_path, &beta.bytes)?;

    let provider = Arc::new(ObservedProvider {
        inner: build_provider_from_model_chain(&live.config, std::slice::from_ref(&live.route))?,
        dispatches: DispatchBudget::new(2),
        calls: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
        errors: Mutex::new(Vec::new()),
        attachments: Mutex::new(Vec::new()),
        texts: Mutex::new(Vec::new()),
        responses: Mutex::new(Vec::new()),
    });
    let session = RuntimeSession::start(&live, provider.clone()).await?;
    session
        .prompt(
            &format!(
                "![alpha]({})\n![beta]({})\n{HISTORY_INSTRUCTIONS}",
                alpha_path.display(),
                beta_path.display()
            ),
            1,
        )
        .await?;
    session
        .prompt(
            &format!(
                "![alpha]({})\n{HISTORY_INSTRUCTIONS}\n\
                 Report every attached image, including the one carried over from the earlier turn.",
                alpha_path.display()
            ),
            2,
        )
        .await?;
    session.finish().await?;

    let calls = provider.attachments.lock().unwrap().clone();
    ensure!(
        calls.len() == 2,
        "expected exactly two provider calls, got {}",
        calls.len()
    );
    ensure!(
        calls[0].len() == 2
            && calls[0][0].label == "alpha"
            && calls[0][1].label == "beta"
            && calls[0]
                .iter()
                .all(|attachment| !attachment.optional_history),
        "the first turn must attach both referenced images as current input"
    );
    ensure!(
        calls[1].len() == 2,
        "the second turn must attach the current image plus the deduplicated historical \
         image, got {} attachments",
        calls[1].len()
    );
    ensure!(
        calls[1][0].label == "alpha"
            && !calls[1][0].optional_history
            && calls[1][0].decoded_bytes == alpha.bytes.len(),
        "the current image must be attached exactly once"
    );
    ensure!(
        calls[1][1].label == "beta"
            && calls[1][1].optional_history
            && calls[1][1].decoded_bytes == beta.bytes.len(),
        "the historical image must be attached once as optional history"
    );
    ensure!(
        provider.calls.load(Ordering::SeqCst) == 2 && provider.dispatches.used() == 2,
        "expected two successful real Gemini dispatches"
    );

    let responses = provider.responses.lock().unwrap().clone();
    ensure!(
        responses.len() == 2,
        "expected two model responses, got {}",
        responses.len()
    );
    let first = parse_labeled(&responses[0])?;
    ensure!(
        first.get("alpha") == Some(&alpha.expected) && first.get("beta") == Some(&beta.expected),
        "the first turn did not report the real pixels of both current images"
    );
    let second = parse_labeled(&responses[1])?;
    ensure!(
        second.get("alpha") == Some(&alpha.expected),
        "the second turn lost the pixels of the current image"
    );
    ensure!(
        second.get("beta") == Some(&beta.expected),
        "the second turn lost the pixels of the historical image"
    );
    eprintln!(
        "live_gemini_image scenario=history_dedup route={} turn1_attachments={} \
         turn2_attachments={} history_attachments={} verified=alpha+beta",
        live.route.as_string(),
        calls[0].len(),
        calls[1].len(),
        calls[1]
            .iter()
            .filter(|attachment| attachment.optional_history)
            .count()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "paid native Gemini history budget probe; two model HTTP dispatches"]
async fn live_gemini_runtime_history_budget_omits_optional_images() -> Result<()> {
    let live = live_config()?;
    ensure_output_budget(&live)?;
    let seed = Uuid::new_v4().as_bytes()[0];
    // Eight distinct historical images plus one current image exceed the
    // eight-image attachment budget, so the runtime must omit one historical
    // image and report the omission instead of exceeding the budget.
    let mut fixtures = Vec::new();
    for index in 0..9u8 {
        fixtures.push(fixture(seed.wrapping_add(index), 320, 192, false)?);
    }
    for (index, left) in fixtures.iter().enumerate() {
        for right in fixtures.iter().skip(index + 1) {
            ensure!(
                left.bytes != right.bytes,
                "the budget fixtures must be pairwise distinct"
            );
        }
    }
    let mut references = Vec::new();
    for (index, image) in fixtures.iter().enumerate().take(8) {
        let path = live.workspace.path().join(format!("b{index}.png"));
        std::fs::write(&path, &image.bytes)?;
        references.push(format!("![b{index}]({})", path.display()));
    }
    let current_path = live.workspace.path().join("current.png");
    std::fs::write(&current_path, &fixtures[8].bytes)?;

    let provider = Arc::new(ObservedProvider {
        inner: build_provider_from_model_chain(&live.config, std::slice::from_ref(&live.route))?,
        dispatches: DispatchBudget::new(2),
        calls: AtomicUsize::new(0),
        completed: AtomicUsize::new(0),
        errors: Mutex::new(Vec::new()),
        attachments: Mutex::new(Vec::new()),
        texts: Mutex::new(Vec::new()),
        responses: Mutex::new(Vec::new()),
    });
    let session = RuntimeSession::start(&live, provider.clone()).await?;
    session
        .prompt(
            &format!("{}\n{BUDGET_INSTRUCTIONS}", references.join("\n")),
            1,
        )
        .await?;
    session
        .prompt(
            &format!(
                "![current]({})\n{BUDGET_INSTRUCTIONS}",
                current_path.display()
            ),
            2,
        )
        .await?;
    session.finish().await?;

    let calls = provider.attachments.lock().unwrap().clone();
    ensure!(
        calls.len() == 2,
        "expected exactly two provider calls, got {}",
        calls.len()
    );
    ensure!(
        calls[0].len() == 8
            && calls[0]
                .iter()
                .all(|attachment| !attachment.optional_history),
        "the first turn must attach all eight current images"
    );
    ensure!(
        calls[1].len() == 8,
        "the second turn must stop at the eight-image attachment budget, got {} attachments",
        calls[1].len()
    );
    let current = calls[1]
        .iter()
        .filter(|attachment| !attachment.optional_history)
        .collect::<Vec<_>>();
    ensure!(
        current.len() == 1
            && current[0].label == "current"
            && current[0].decoded_bytes == fixtures[8].bytes.len(),
        "the second turn must attach exactly one current image"
    );
    let historical = calls[1]
        .iter()
        .filter(|attachment| attachment.optional_history)
        .map(|attachment| attachment.label.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        historical.len() == 7,
        "expected seven historical attachments inside the budget, got {historical:?}"
    );
    let texts = provider.texts.lock().unwrap().clone();
    let notice = texts
        .get(1)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|text| text.starts_with("[Holon image attachment budget:"))
        })
        .context("the second turn did not report the historical image budget omission")?;
    ensure!(
        notice.contains("omitted 1 optional historical image attachment(s)"),
        "the budget notice did not report the omitted historical image: {notice}"
    );
    ensure!(
        notice.contains("Attached 8 unique image(s)") && notice.contains("limits 8 images"),
        "the budget notice did not describe the bounded attachment set: {notice}"
    );
    ensure!(
        provider.calls.load(Ordering::SeqCst) == 2 && provider.dispatches.used() == 2,
        "expected two successful real Gemini dispatches"
    );
    eprintln!(
        "live_gemini_image scenario=history_budget route={} turn2_attachments={} \
         history_attachments={} omitted=1 verified=budget_notice",
        live.route.as_string(),
        calls[1].len(),
        historical.len()
    );
    Ok(())
}

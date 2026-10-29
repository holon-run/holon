//! Bounded, opt-in visual-content acceptance through the real AgentProvider path.
//!
//! Example (one explicit route, at most two model HTTP dispatches):
//! HOLON_LIVE_IMAGE_MODEL='openai-codex@default/gpt-6.1-sol' \
//!   cargo test --test live_image_budget -- --ignored --nocapture
//!
//! HOLON_LIVE_IMAGE_MAX_OUTPUT_TOKENS defaults to 256 (allowed: 64..=2048).
//! A reasoning model may need a larger allowance for a visible JSON answer.
//! This exercises provider serialization/budgeting, NOT the private runtime
//! conversation builder's historical-image selection or deduplication.

use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use holon::{
    config::{AppConfig, ModelRouteCapability, ModelRouteRef, RuntimeModelCatalog},
    context::ContextConfig,
    provider::{
        build_provider_from_model_chain, provider_attempt_timeline, provider_transport_diagnostics,
        ConversationMessage, ModelBlock, ProviderDispatchGuard, ProviderTurnRequest,
    },
};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const REQUEST_LIMIT: usize = 2;
const RESPONSE_INSTRUCTIONS: &str = r#"Inspect the actual attached images, not their labels.
Return only JSON in this exact format:
{"first":{"color":"COLOR","shape":"SHAPE","count":0},"second":{"color":"COLOR","shape":"SHAPE","count":0}}
The placeholders and zero counts are not answers. Determine every value from the pixels.
Use color red, green, blue, or yellow; shape circle or square; integer count.
first describes image A, second describes image B. Repeated copies of image A
are the same reference, not extra shapes to add together. No tools or prose."#;

#[derive(Debug, Default)]
struct DispatchLimit(AtomicUsize);

impl ProviderDispatchGuard for DispatchLimit {
    fn before_dispatch(&self) -> Result<()> {
        let mut count = self.0.load(Ordering::SeqCst);
        loop {
            ensure!(
                count < REQUEST_LIMIT,
                "live image HTTP request limit reached; no more dispatches"
            );
            match self
                .0
                .compare_exchange(count, count + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Ok(()),
                Err(actual) => count = actual,
            }
        }
    }

    fn allows_auxiliary_dispatch(&self) -> Result<bool> {
        // Server-side compaction is outside this small visual-content probe.
        Ok(false)
    }
}

#[test]
fn dispatch_limit_is_atomic_and_stays_at_the_limit() {
    let guard = DispatchLimit::default();
    let admitted = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| guard.before_dispatch().is_ok()))
            .collect();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(admitted, REQUEST_LIMIT);
    assert_eq!(guard.0.load(Ordering::SeqCst), REQUEST_LIMIT);
    assert!(guard.before_dispatch().is_err());
    assert_eq!(guard.0.load(Ordering::SeqCst), REQUEST_LIMIT);
    assert!(!guard.allows_auxiliary_dispatch().unwrap());
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct Observation {
    color: String,
    shape: String,
    count: u8,
}

#[derive(Debug, Deserialize, Serialize)]
struct ObservationPair {
    first: Observation,
    second: Observation,
}

struct Fixture {
    expected: Observation,
    data_base64: String,
}

fn fixture(color: usize, square: bool, count: u8) -> Result<Fixture> {
    let colors = [
        ("red", [235, 20, 20]),
        ("green", [15, 170, 30]),
        ("blue", [20, 45, 235]),
        ("yellow", [245, 220, 10]),
    ];
    let (name, rgb) = colors[color];
    let mut pixels = RgbImage::from_pixel(320, 192, Rgb([255, 255, 255]));
    for index in 0..count {
        let center_x = 320 * (i32::from(index) + 1) / (i32::from(count) + 1);
        for y in 64..=128 {
            for x in (center_x - 32)..=(center_x + 32) {
                let dx = x - center_x;
                let dy = y - 96;
                if square || dx * dx + dy * dy <= 32 * 32 {
                    pixels.put_pixel(x as u32, y as u32, Rgb(rgb));
                }
            }
        }
    }
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(pixels).write_to(&mut png, ImageFormat::Png)?;
    Ok(Fixture {
        expected: Observation {
            color: name.into(),
            shape: if square { "square" } else { "circle" }.into(),
            count,
        },
        data_base64: BASE64_STANDARD.encode(png.into_inner()),
    })
}

fn random_fixtures() -> Result<(Fixture, Fixture)> {
    let seed = *Uuid::new_v4().as_bytes();
    let first_color = usize::from(seed[0]) % 4;
    let second_color = (first_color + 1 + usize::from(seed[1]) % 3) % 4;
    let first_square = seed[2] & 1 == 0;
    let first_count = 1 + seed[3] % 3;
    let second_count = 1 + (first_count - 1 + 1 + seed[4] % 2) % 3;
    Ok((
        fixture(first_color, first_square, first_count)?,
        fixture(second_color, !first_square, second_count)?,
    ))
}

fn image_message(fixture: &Fixture, label: &str, optional_history: bool) -> ConversationMessage {
    ConversationMessage::UserImage {
        // Never disclose the randomly selected answers in text or filenames.
        prompt: format!("Image {label}. Inspect its visible colored shapes."),
        media_type: "image/png".into(),
        data_base64: fixture.data_base64.clone(),
        optional_history,
    }
}

fn parse_observations(blocks: &[ModelBlock]) -> Result<ObservationPair> {
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ModelBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    // Do not print arbitrary provider responses or reasoning on parse failure.
    serde_json::from_str(json).map_err(|_| {
        anyhow::anyhow!("live image response was not the requested visual-observation JSON")
    })
}

#[tokio::test]
#[ignore = "paid live vision probe; requires explicit HOLON_LIVE_IMAGE_MODEL and real credentials"]
async fn live_image_budget_recognizes_current_and_repeated_history() -> Result<()> {
    let raw_route = std::env::var("HOLON_LIVE_IMAGE_MODEL")
        .context("set HOLON_LIVE_IMAGE_MODEL to an explicit provider@endpoint/model route")?;
    let requested_route = ModelRouteRef::parse(&raw_route)
        .map_err(|_| anyhow::anyhow!("invalid HOLON_LIVE_IMAGE_MODEL route"))?;
    let max_output_tokens = match std::env::var("HOLON_LIVE_IMAGE_MAX_OUTPUT_TOKENS") {
        Ok(value) => value
            .parse::<u32>()
            .context("HOLON_LIVE_IMAGE_MAX_OUTPUT_TOKENS must be an integer")?,
        Err(std::env::VarError::NotPresent) => 256,
        Err(error) => return Err(error.into()),
    };
    ensure!(
        (64..=2048).contains(&max_output_tokens),
        "live image output allowance must be 64..=2048 tokens"
    );
    let mut config = AppConfig::load()
        .map_err(|_| anyhow::anyhow!("failed to load live Holon configuration"))?;
    let context = ContextConfig::default();
    let route = RuntimeModelCatalog::from_config(&config)
        .resolve_explicit_model_route(
            &context,
            &requested_route,
            ModelRouteCapability::VisionObservation,
        )
        .context("explicit route is unavailable or lacks effective image_input")?;
    ensure!(
        route.capabilities.agent_turn && route.capabilities.image_input,
        "explicit route must support both agent turns and image_input"
    );
    ensure!(
        route.provider_config().has_configured_credential(),
        "explicit live image route has no configured credential"
    );
    // Override the resolved model's catalog default as well as the runtime
    // default; changing only AppConfig.runtime_max_output_tokens is insufficient.
    config
        .validated_model_overrides
        .entry(route.model_ref.clone())
        .or_default()
        .runtime_max_output_tokens = Some(max_output_tokens);
    config.runtime_max_output_tokens = max_output_tokens;
    config.disable_provider_fallback = true;
    let bounded_route = RuntimeModelCatalog::from_config(&config)
        .resolve_explicit_model_route(
            &context,
            &route.route_ref,
            ModelRouteCapability::VisionObservation,
        )
        .context("bounded live image route no longer resolves")?;
    ensure!(
        bounded_route.policy.runtime_max_output_tokens <= max_output_tokens,
        "resolved output allowance exceeds live test limit"
    );
    let trace_home = tempfile::tempdir()?;
    config.home_dir = trace_home.path().to_path_buf();
    let provider =
        build_provider_from_model_chain(&config, std::slice::from_ref(&bounded_route.route_ref))
            .map_err(|_| anyhow::anyhow!("failed to build the explicit live image provider"))?;
    let model = bounded_route.route_ref.as_string();
    let transport = bounded_route.provider_config().transport.as_str();
    let (first, second) = random_fixtures()?;
    ensure!(
        first.data_base64 != second.data_base64 && first.expected != second.expected,
        "visual probes must have different pixels and expected content"
    );
    let guard = Arc::new(DispatchLimit::default());
    eprintln!(
        "live_image_budget model={model} transport={transport} credential=configured \
         max_output_tokens={} model_http_limit={REQUEST_LIMIT} runtime_builder=not_exercised",
        bounded_route.policy.runtime_max_output_tokens
    );
    for (scenario, repeat_history) in [("current_pair", false), ("repeated_optional_history", true)]
    {
        let mut conversation = vec![image_message(&first, "A", repeat_history)];
        if repeat_history {
            conversation.push(image_message(&first, "A", true));
        }
        conversation.push(image_message(&second, "B", false));
        conversation.push(ConversationMessage::UserText(
            "Compare image A and image B. Return the required JSON from their pixels.".into(),
        ));
        let mut request = ProviderTurnRequest::plain(RESPONSE_INSTRUCTIONS, conversation, vec![]);
        request.dispatch_guard = Some(guard.clone());
        let result =
            tokio::time::timeout(Duration::from_secs(120), provider.complete_turn(request))
                .await
                .with_context(|| {
                    format!("live image request timed out; model={model} scenario={scenario}")
                })?;
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let failure_kinds = provider_attempt_timeline(&error).map(|timeline| {
                    timeline
                        .attempts
                        .iter()
                        .filter_map(|attempt| attempt.failure_kind.as_deref())
                        .collect::<Vec<_>>()
                });
                let status = provider_transport_diagnostics(&error)
                    .and_then(|diagnostics| diagnostics.status);
                bail!(
                    "live image provider request failed; model={model} transport={transport} \
                     scenario={scenario} http_dispatches={} status={status:?} \
                     failure_kinds={failure_kinds:?}; provider error body omitted",
                    guard.0.load(Ordering::SeqCst)
                )
            }
        };
        // Usage is actual provider-reported usage, never a local estimate.
        eprintln!(
            "live_image_budget model={model} transport={transport} scenario={scenario} \
             input_tokens={} output_tokens={} cache_usage={:?} http_dispatches={}",
            output.input_tokens,
            output.output_tokens,
            output.cache_usage,
            guard.0.load(Ordering::SeqCst)
        );
        let observed = parse_observations(&output.blocks)?;
        ensure!(
            observed.first == first.expected && observed.second == second.expected,
            "incorrect visual content; model={model} scenario={scenario}; \
             expected_first={:?} expected_second={:?} observed={observed:?}",
            first.expected,
            second.expected
        );
        eprintln!(
            "live_image_budget model={model} scenario={scenario} verified_response={}",
            serde_json::to_string(&observed)?
        );
    }
    ensure!(
        guard.0.load(Ordering::SeqCst) == REQUEST_LIMIT,
        "expected exactly two successful model HTTP dispatches"
    );
    Ok(())
}

//! Embedded, zero-egress ONNX decision provider.
//!
//! The provider never resolves model identifiers or URLs. A model directory
//! must already exist on the local filesystem and contain the manifest,
//! tokenizer, ONNX graph, and the ONNX Runtime shared library named by the
//! platform must already exist in the model directory.

use async_trait::async_trait;
use decision_core::{
    DecisionContext, DecisionError, DecisionProvider, DecisionRequest, DecisionResponse,
};
#[cfg(feature = "onnx")]
use decision_core::{DecisionOutcome, Evidence, Provenance};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[cfg(feature = "onnx")]
use std::{sync::Arc, time::Instant};
use thiserror::Error;
#[cfg(feature = "onnx")]
use tokio::sync::Mutex;

pub mod state;

const DEFAULT_MANIFEST: &str = "open_jev_config.json";
const HOLON_MANIFEST: &str = "holon_model_manifest.json";
const DEFAULT_MODEL: &str = "model.onnx";
const DEFAULT_TOKENIZER: &str = "tokenizer.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalOnnxConfig {
    pub model_dir: PathBuf,
    #[serde(default = "default_variant")]
    pub variant: String,
    #[serde(default = "default_num_threads")]
    pub num_threads: usize,
    #[serde(default)]
    pub checksum: Option<String>,
}

fn default_variant() -> String {
    "q4f16".into()
}

fn default_num_threads() -> usize {
    1
}

impl LocalOnnxConfig {
    pub fn validate(&self) -> Result<ModelFiles, LocalOnnxError> {
        if self.model_dir.as_os_str().is_empty() {
            return Err(LocalOnnxError::InvalidConfig("model_dir is empty".into()));
        }
        if self.num_threads == 0 {
            return Err(LocalOnnxError::InvalidConfig(
                "num_threads must be greater than zero".into(),
            ));
        }
        let model_dir = fs::canonicalize(&self.model_dir)
            .map_err(|error| LocalOnnxError::ModelUnavailable(error.to_string()))?;
        if !model_dir.is_dir() {
            return Err(LocalOnnxError::ModelUnavailable(format!(
                "{} is not a directory",
                model_dir.display()
            )));
        }
        let managed_manifest = model_dir.join(HOLON_MANIFEST);
        let manifest_path = if managed_manifest.is_file() {
            managed_manifest
        } else {
            model_dir.join(DEFAULT_MANIFEST)
        };
        let manifest = if manifest_path.is_file() {
            let bytes = fs::read(&manifest_path)
                .map_err(|error| LocalOnnxError::ModelUnavailable(error.to_string()))?;
            serde_json::from_slice::<ModelManifest>(&bytes)
                .map_err(|error| LocalOnnxError::InvalidManifest(error.to_string()))?
        } else {
            ModelManifest::default()
        };
        if manifest.encoding != ENCODING_QUESTION_TAIL {
            return Err(LocalOnnxError::InvalidManifest(format!(
                "unsupported encoding '{}' (expected '{}')",
                manifest.encoding, ENCODING_QUESTION_TAIL
            )));
        }
        let model_path = resolve_model_file(&model_dir, &manifest.model, "model")?;
        let tokenizer_path = resolve_model_file(&model_dir, &manifest.tokenizer, "tokenizer")?;
        if let Some(expected) = self.checksum.as_deref() {
            verify_sha256(&model_path, expected)?;
        }
        Ok(ModelFiles {
            model_dir,
            model_path,
            tokenizer_path,
            manifest,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelManifest {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_tokenizer")]
    pub tokenizer: String,
    #[serde(default = "default_input_ids")]
    pub input_ids: String,
    #[serde(default = "default_attention_mask")]
    pub attention_mask: String,
    #[serde(default = "default_token_type_ids")]
    pub token_type_ids: String,
    #[serde(default = "default_output")]
    pub output: String,
    #[serde(default = "default_max_length")]
    pub max_length: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Text encoding contract. Only `question-tail` (training-corpus format
    /// with tail-preserving truncation) is supported.
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

fn default_model() -> String {
    DEFAULT_MODEL.into()
}

fn default_tokenizer() -> String {
    DEFAULT_TOKENIZER.into()
}

fn default_input_ids() -> String {
    "input_ids".into()
}

fn default_attention_mask() -> String {
    "attention_mask".into()
}

fn default_token_type_ids() -> String {
    "token_type_ids".into()
}

fn default_output() -> String {
    "logits".into()
}

fn default_max_length() -> usize {
    512
}

fn default_temperature() -> f32 {
    1.05
}

const ENCODING_QUESTION_TAIL: &str = "question-tail";

fn default_encoding() -> String {
    ENCODING_QUESTION_TAIL.into()
}

impl Default for ModelManifest {
    fn default() -> Self {
        Self {
            model: default_model(),
            tokenizer: default_tokenizer(),
            input_ids: default_input_ids(),
            attention_mask: default_attention_mask(),
            token_type_ids: default_token_type_ids(),
            output: default_output(),
            max_length: default_max_length(),
            temperature: default_temperature(),
            encoding: default_encoding(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelFiles {
    pub model_dir: PathBuf,
    pub model_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub manifest: ModelManifest,
}

#[derive(Debug, Error)]
pub enum LocalOnnxError {
    #[error("invalid local-onnx configuration: {0}")]
    InvalidConfig(String),
    #[error("local-onnx model unavailable: {0}")]
    ModelUnavailable(String),
    #[error("local-onnx manifest invalid: {0}")]
    InvalidManifest(String),
    #[error("local-onnx checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("local-onnx inference failed: {0}")]
    Inference(String),
    #[error("local-onnx support is not enabled in this build")]
    FeatureDisabled,
}

fn resolve_model_file(
    model_dir: &Path,
    relative: &str,
    kind: &str,
) -> Result<PathBuf, LocalOnnxError> {
    let candidate = model_dir.join(relative);
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        LocalOnnxError::ModelUnavailable(format!(
            "{kind} file {} is missing: {error}",
            candidate.display()
        ))
    })?;
    // Manifest paths must stay inside the configured model directory so an
    // untrusted bundle cannot redirect loading outside the local boundary.
    if !canonical.starts_with(model_dir) {
        return Err(LocalOnnxError::InvalidManifest(format!(
            "{kind} path {} escapes the model directory",
            candidate.display()
        )));
    }
    if !canonical.is_file() {
        return Err(LocalOnnxError::ModelUnavailable(format!(
            "{kind} file {} is missing",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), LocalOnnxError> {
    let expected = expected.trim().trim_start_matches("sha256:");
    let bytes =
        fs::read(path).map_err(|error| LocalOnnxError::ModelUnavailable(error.to_string()))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(LocalOnnxError::ChecksumMismatch {
            expected: expected.into(),
            actual,
        });
    }
    Ok(())
}

#[cfg(feature = "onnx")]
fn runtime_library_path(model_dir: &Path) -> Result<PathBuf, LocalOnnxError> {
    let candidates = [
        "onnxruntime.dll",
        "libonnxruntime.dylib",
        "onnxruntime.dylib",
        "libonnxruntime.so",
        "onnxruntime.so",
    ];
    candidates
        .iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            LocalOnnxError::ModelUnavailable(format!(
                "ONNX Runtime shared library is missing from {}",
                model_dir.display()
            ))
        })
}

pub struct LocalOnnxProvider {
    config: LocalOnnxConfig,
    files: ModelFiles,
    #[cfg(feature = "onnx")]
    runtime: Arc<Mutex<Option<OnnxRuntime>>>,
}

impl std::fmt::Debug for LocalOnnxProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalOnnxProvider")
            .field("model_dir", &self.config.model_dir)
            .field("variant", &self.config.variant)
            .field("num_threads", &self.config.num_threads)
            .field(
                "checksum",
                &self.config.checksum.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

impl LocalOnnxProvider {
    pub fn new(config: LocalOnnxConfig) -> Result<Self, LocalOnnxError> {
        let files = config.validate()?;
        Ok(Self {
            config,
            files,
            #[cfg(feature = "onnx")]
            runtime: Arc::new(Mutex::new(None)),
        })
    }

    pub fn config(&self) -> &LocalOnnxConfig {
        &self.config
    }

    pub fn model_files(&self) -> &ModelFiles {
        &self.files
    }

    #[cfg(feature = "onnx")]
    async fn infer(
        &self,
        request: &DecisionRequest<Value, Value>,
        context: &DecisionContext,
    ) -> Result<InferenceResult, LocalOnnxError> {
        let request = request.clone();
        let context = context.clone();
        let files = self.files.clone();
        let num_threads = self.config.num_threads;
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || {
            context
                .check()
                .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let mut guard = runtime.blocking_lock();
            let runtime = if let Some(runtime) = guard.as_mut() {
                runtime
            } else {
                let created = OnnxRuntime::load(&files, num_threads)?;
                guard.insert(created)
            };
            runtime.decide(&request, &context)
        })
        .await
        .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
    }
}

#[async_trait]
impl DecisionProvider<Value, Value> for LocalOnnxProvider {
    type Output = Value;

    async fn decide(
        &self,
        request: DecisionRequest<Value, Value>,
        context: DecisionContext,
    ) -> Result<DecisionResponse<Self::Output>, DecisionError> {
        request.validate()?;
        context.check()?;
        #[cfg(not(feature = "onnx"))]
        {
            return Err(DecisionError::Provider(
                LocalOnnxError::FeatureDisabled.to_string(),
            ));
        }
        #[cfg(feature = "onnx")]
        {
            let started = Instant::now();
            let result = self
                .infer(&request, &context)
                .await
                .map_err(|error| DecisionError::Provider(error.to_string()))?;
            let mut evidence = vec![Evidence::new(
                "local_onnx",
                format!(
                    "{} variant loaded from local model directory",
                    self.config.variant
                ),
            )];
            evidence[0].metadata.insert(
                "model_dir".into(),
                self.files.model_dir.display().to_string(),
            );
            evidence[0].metadata.insert(
                "probabilities".into(),
                serde_json::to_string(&result.probabilities).unwrap_or_default(),
            );
            Ok(DecisionResponse {
                schema_version: request.schema_version,
                outcome: result.outcome,
                confidence: result.confidence,
                evidence,
                provenance: Provenance::new("local-onnx", Some(self.config.variant.clone())),
                elapsed_ms: Some(started.elapsed().as_millis() as u64),
            })
        }
    }
}

#[cfg(feature = "onnx")]
struct OnnxRuntime {
    tokenizer: tokenizers::Tokenizer,
    session: ort::session::Session,
    manifest: ModelManifest,
}

#[cfg(feature = "onnx")]
struct InferenceResult {
    outcome: DecisionOutcome<Value>,
    confidence: Option<f32>,
    probabilities: Vec<f32>,
}

#[cfg(feature = "onnx")]
impl OnnxRuntime {
    fn load(files: &ModelFiles, num_threads: usize) -> Result<Self, LocalOnnxError> {
        let runtime_library = runtime_library_path(&files.model_dir)?;
        let _ = ort::init_from(runtime_library.display().to_string())
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
            .with_name("holon-decision-local-onnx")
            .commit();
        let session = ort::session::Session::builder()
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
            .with_intra_threads(num_threads)
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
            .commit_from_file(&files.model_path)
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
        let tokenizer = tokenizers::Tokenizer::from_file(&files.tokenizer_path)
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
        Ok(Self {
            tokenizer,
            session,
            manifest: files.manifest.clone(),
        })
    }

    /// Encode one (state, schema, option) decision in the training-corpus
    /// `question-tail` format: `[CLS] + state + "\n[问题] schema\n[选项] option" + [SEP]`.
    /// The tail is always kept; the state head is truncated to fit max_length.
    fn encode_question_tail(
        &self,
        state: &str,
        schema: &str,
        option: &str,
    ) -> Result<Vec<i64>, LocalOnnxError> {
        let tail = format!("\n[问题] {schema}\n[选项] {option}");
        let tail_ids = self
            .tokenizer
            .encode(tail, false)
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
            .get_ids()
            .iter()
            .map(|value| *value as i64)
            .collect::<Vec<_>>();
        let mut state_ids = self
            .tokenizer
            .encode(state, false)
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?
            .get_ids()
            .iter()
            .map(|value| *value as i64)
            .collect::<Vec<_>>();
        let budget = self.manifest.max_length.saturating_sub(tail_ids.len() + 2);
        if budget < 8 {
            return Err(LocalOnnxError::Inference(
                "tail too long for max_length".into(),
            ));
        }
        if state_ids.len() > budget {
            let excess = state_ids.len() - budget;
            state_ids.drain(0..excess);
        }
        let cls = self
            .tokenizer
            .token_to_id("[CLS]")
            .ok_or_else(|| LocalOnnxError::Inference("missing [CLS] token".into()))?
            as i64;
        let sep = self
            .tokenizer
            .token_to_id("[SEP]")
            .ok_or_else(|| LocalOnnxError::Inference("missing [SEP] token".into()))?
            as i64;
        let mut ids = Vec::with_capacity(state_ids.len() + tail_ids.len() + 2);
        ids.push(cls);
        ids.extend(state_ids);
        ids.extend(tail_ids);
        ids.push(sep);
        Ok(ids)
    }

    fn decide(
        &mut self,
        request: &DecisionRequest<Value, Value>,
        context: &DecisionContext,
    ) -> Result<InferenceResult, LocalOnnxError> {
        let state = request
            .input
            .get("baseline")
            .cloned()
            .unwrap_or_else(|| request.input.clone());
        let state_text = value_text(&state);
        let mut sequences = Vec::with_capacity(request.candidates.len());
        for candidate in &request.candidates {
            let option_text = value_text(candidate);
            let sequence = self.encode_question_tail(&state_text, &request.schema, &option_text)?;
            sequences.push(sequence);
        }
        let wants_token_types = self
            .session
            .inputs()
            .iter()
            .any(|input| input.name() == self.manifest.token_type_ids);
        let mut scores = Vec::with_capacity(sequences.len());
        for sequence in sequences {
            context
                .check()
                .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let length = sequence.len();
            let ids = ort::value::Tensor::from_array((vec![1, length], sequence))
                .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let mask = ort::value::Tensor::from_array((vec![1, length], vec![1_i64; length]))
                .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let outputs = if wants_token_types {
                let types = ort::value::Tensor::from_array((vec![1, length], vec![0_i64; length]))
                    .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
                self.session.run(ort::inputs![
                    self.manifest.input_ids.as_str() => &ids,
                    self.manifest.attention_mask.as_str() => &mask,
                    self.manifest.token_type_ids.as_str() => &types
                ])
            } else {
                self.session.run(ort::inputs![
                    self.manifest.input_ids.as_str() => &ids,
                    self.manifest.attention_mask.as_str() => &mask
                ])
            }
            .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let output = outputs.get(self.manifest.output.as_str()).ok_or_else(|| {
                LocalOnnxError::Inference(format!(
                    "model output {} is missing",
                    self.manifest.output
                ))
            })?;
            let (_, values) = output
                .try_extract_tensor::<f32>()
                .map_err(|error| LocalOnnxError::Inference(error.to_string()))?;
            let score = values.first().copied().ok_or_else(|| {
                LocalOnnxError::Inference("model output contains no logits".into())
            })?;
            scores.push(score / self.manifest.temperature.max(f32::EPSILON));
        }
        let (winner, best) = scores
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .ok_or_else(|| LocalOnnxError::Inference("model returned no scores".into()))?;
        let probabilities = softmax(&scores);
        let confidence = probabilities[winner];
        if confidence < 0.8 {
            return Ok(InferenceResult {
                outcome: DecisionOutcome::Abstain {
                    reason: "local-onnx confidence below threshold".into(),
                },
                confidence: Some(confidence),
                probabilities,
            });
        }
        let _ = best;
        Ok(InferenceResult {
            outcome: DecisionOutcome::Select {
                value: request.candidates[winner].clone(),
            },
            confidence: Some(confidence),
            probabilities,
        })
    }
}

#[cfg(feature = "onnx")]
fn softmax(scores: &[f32]) -> Vec<f32> {
    let max = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let values = scores
        .iter()
        .map(|score| (*score - max).exp())
        .collect::<Vec<_>>();
    let total = values.iter().sum::<f32>();
    values.into_iter().map(|value| value / total).collect()
}

#[cfg(feature = "onnx")]
fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_local_model_files_and_checksum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let model = directory.path().join(DEFAULT_MODEL);
        let tokenizer = directory.path().join(DEFAULT_TOKENIZER);
        fs::write(&model, b"model").expect("model");
        fs::write(&tokenizer, b"{}").expect("tokenizer");
        let digest = format!("{:x}", Sha256::digest(b"model"));
        let config = LocalOnnxConfig {
            model_dir: directory.path().into(),
            variant: "q4f16".into(),
            num_threads: 1,
            checksum: Some(digest),
        };
        let files = config.validate().expect("valid files");
        assert_eq!(files.model_path, model.canonicalize().expect("canonical"));
    }

    #[test]
    fn rejects_zero_threads() {
        let config = LocalOnnxConfig {
            model_dir: PathBuf::from("models"),
            variant: "fp16".into(),
            num_threads: 0,
            checksum: None,
        };
        assert!(matches!(
            config.validate(),
            Err(LocalOnnxError::InvalidConfig(message)) if message.contains("num_threads")
        ));
    }

    #[test]
    fn manifest_is_optional_and_defaults_to_open_jev_names() {
        let manifest = ModelManifest::default();
        assert_eq!(manifest.model, DEFAULT_MODEL);
        assert_eq!(manifest.input_ids, "input_ids");
    }

    #[test]
    fn rejects_manifest_paths_outside_model_dir() {
        let directory = tempfile::tempdir().expect("tempdir");
        let model_dir = directory.path().join("models");
        fs::create_dir(&model_dir).expect("model dir");
        let outside = directory.path().join("outside.onnx");
        fs::write(&outside, b"model").expect("outside model");

        let escaped =
            resolve_model_file(&model_dir, "../outside.onnx", "model").expect_err("rejected");
        assert!(matches!(escaped, LocalOnnxError::InvalidManifest(_)));

        let absolute = resolve_model_file(&model_dir, &outside.display().to_string(), "model")
            .expect_err("absolute rejected");
        assert!(matches!(absolute, LocalOnnxError::InvalidManifest(_)));
    }

    #[cfg(feature = "onnx")]
    #[test]
    fn softmax_is_stable_and_normalized() {
        let probabilities = softmax(&[1000.0, 1000.0, 1000.0]);
        assert_eq!(probabilities.len(), 3);
        for value in &probabilities {
            assert!((value - 1.0 / 3.0).abs() < 1e-6);
        }
    }
}

use super::message::AuthorityClass;
use super::serde_helpers::is_false;
use super::task::{CommandTaskStatusSnapshot, TaskHandle, TaskStatusSnapshot};
use super::workspace::{
    ActiveWorkspaceEntry, ExecutionRootEntry, WorkspaceEntry, WorkspaceOccupancyRecord,
};
use crate::execution_types::{WorkspaceAccessMode, WorkspaceProjectionKind};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandCostDiagnostics {
    pub cmd_preview: String,
    pub cmd_char_count: usize,
    pub cmd_estimated_tokens: usize,
    pub contains_heredoc: bool,
    pub contains_inline_script: bool,
    pub exceeds_soft_threshold: bool,
    pub effective_max_output_tokens: u64,
    pub output_char_budget: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum ExecCommandOutcome {
    Completed {
        exit_status: Option<i32>,
        stdout_preview: Option<String>,
        stderr_preview: Option<String>,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<ToolArtifactRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_artifact: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_artifact: Option<usize>,
    },
    PromotedToTask {
        task_handle: TaskHandle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_output_preview: Option<String>,
        initial_output_truncated: bool,
    },
    AlreadyRunning {
        task_handle: TaskHandle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<CommandTaskStatusSnapshot>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandDuplicatePolicy {
    ReuseRunning,
    StartNew,
}

impl Default for ExecCommandDuplicatePolicy {
    fn default() -> Self {
        Self::ReuseRunning
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandResult {
    #[serde(flatten)]
    pub outcome: ExecCommandOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_diagnostics: Option<CommandCostDiagnostics>,
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandBatchItemStatus {
    Completed,
    Failed,
    Rejected,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandBatchItemResult {
    pub index: usize,
    pub cmd: String,
    pub status: ExecCommandBatchItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExecCommandResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommandBatchResult {
    pub item_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub rejected_count: usize,
    pub skipped_count: usize,
    pub stop_on_error: bool,
    pub items: Vec<ExecCommandBatchItemResult>,
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ToolArtifactRef {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskStatusResult {
    pub task: TaskStatusSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskInputResult {
    pub task: TaskStatusSnapshot,
    pub accepted_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchAction {
    Add,
    Modify,
    Delete,
    Move,
}

impl ApplyPatchAction {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Add => "A",
            Self::Modify => "M",
            Self::Delete => "D",
            Self::Move => "R",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchHunkSummary {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub added_count: u32,
    pub removed_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchChangedFile {
    pub action: ApplyPatchAction,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<ApplyPatchHunkSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_preview: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub diff_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchIgnoredMetadata {
    pub path: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchDiagnostic {
    pub path: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyPatchResult {
    #[serde(default)]
    pub changed_files: Vec<ApplyPatchChangedFile>,
    pub changed_paths: Vec<String>,
    pub changed_file_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_metadata: Vec<ApplyPatchIgnoredMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ApplyPatchDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ViewImageSelectedMode {
    Unavailable,
    NativeImageWithObservation,
    VisionAdapter,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageReferenceSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageVisualReference {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub path: PathBuf,
    /// Absolute filesystem path for the image. The historical field name is
    /// retained for serialized compatibility with older tool results.
    #[serde(default)]
    pub workspace_uri: Option<String>,
    pub sha256: String,
    pub mime: String,
    pub byte_count: u64,
    pub size: ViewImageReferenceSize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageGeneratedBy {
    pub mode: ViewImageSelectedMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageObservation {
    #[serde(rename = "type")]
    pub kind: String,
    pub schema: String,
    pub visual_reference_id: String,
    pub prompt: String,
    pub generated_by: ViewImageGeneratedBy,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ocr: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Value>,
    pub uncertainties: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_sources: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageResult {
    pub visual_reference: ViewImageVisualReference,
    #[serde(default)]
    pub cache_hit: bool,
    pub observation: ViewImageObservation,
    pub selected_mode: ViewImageSelectedMode,
    pub vision_selection: ViewImageVisionSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GenerateImageSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GeneratedImageReference {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub workspace_id: String,
    pub path: PathBuf,
    pub uri: String,
    pub sha256: String,
    pub mime: String,
    pub byte_count: u64,
    pub size: GenerateImageSize,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GenerateImageParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GeneratedImageSourceReference {
    pub uri: String,
    pub sha256: String,
    pub byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_reported_mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct GenerateImageResult {
    pub images: Vec<GeneratedImageReference>,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    pub parameters: GenerateImageParameters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_succeeded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_parameters: Option<crate::image_contract::ImageGenerationParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_parameters: Option<crate::image_contract::ImageGenerationParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_effective_parameters: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_defaults: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_mapping: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<crate::image_contract::ImageGenerationContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_images: Vec<GeneratedImageSourceReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversion: Vec<crate::image_contract::ImageConversionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageVisionCandidate {
    pub provider: String,
    pub model: String,
    pub model_ref: String,
    pub image_input: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ViewImageVisionSelection {
    pub selected_mode: ViewImageSelectedMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_model: Option<String>,
    pub selection_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ViewImageVisionCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UseWorkspaceResult {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    pub mode: String,
    pub projection_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceStateResult {
    pub agent_id: String,
    pub attached_workspace_ids: Vec<String>,
    pub workspaces: Vec<WorkspaceEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<ActiveWorkspaceEntry>,
    pub execution_roots: Vec<ExecutionRootEntry>,
    pub occupancies: Vec<WorkspaceOccupancyRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachWorkspaceResult {
    pub workspace: WorkspaceEntry,
    pub already_attached: bool,
    pub active_unchanged: bool,
    pub discovered_projection_kind: WorkspaceProjectionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_execution_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetachWorkspaceResult {
    pub workspace_id: String,
    pub detached: bool,
    pub switched_to_agent_home: bool,
    pub retained_execution_roots: Vec<ExecutionRootEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwitchWorkspaceResult {
    pub disposition: String,
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    pub execution_root_id: String,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    pub projection_kind: WorkspaceProjectionKind,
    pub access_mode: WorkspaceAccessMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWorktreeResult {
    pub disposition: String,
    pub created: bool,
    pub reused: bool,
    pub activated: bool,
    pub base_ref_applied: bool,
    pub requested_base_ref: String,
    pub resolved_base_commit: String,
    pub branch: String,
    pub branch_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_tip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ExecutionRootEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoveWorktreeResult {
    pub execution_root_id: String,
    pub disposition: String,
    pub switched: bool,
    pub removed: bool,
    pub branch_deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_tip: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_retained_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct TaskStopResult {
    pub task: TaskStatusSnapshot,
    pub stop_requested: bool,
    pub force_stop_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Pending,
    Deferred,
    Success,
    Error,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ToolExecutionCallBinding {
    pub incarnation: u64,
    pub assistant_round_id: String,
    pub tool_call_id: String,
    pub model_output_budget: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ToolExecutionRecord {
    pub id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default)]
    pub turn_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub tool_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(alias = "trust")]
    pub authority_class: AuthorityClass,
    pub status: ToolExecutionStatus,
    pub input: Value,
    pub output: Value,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_binding: Option<ToolExecutionCallBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_surface: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLatencyMetrics {
    pub tool_name: String,
    pub total_calls: u64,
    pub total_duration_ms: u64,
}

//! Tool contracts and the runtime-owned state returned from tool execution.

pub(crate) use holon_tools::spec::{
    AwaitCompletionReportDirective, AwaitWaitReportDirective, ExternalCommandAttempt,
    ExternalCommandRegistrationFailed, ToolLoopDirective,
};
pub use holon_tools::spec::{
    CompletionReportCandidate, ToolCall, ToolExecutionContext, ToolFreeformGrammar,
    ToolInputCoercion, ToolResultEnvelope, ToolResultStatus, ToolSpec,
};

/// Deferred commits and resource guards that the runtime must keep alive.
#[derive(Debug, Clone, Default)]
pub struct RuntimeToolState {
    pub(crate) resource_material: Option<crate::diagnostics::resource_io::ResourceReadMaterial>,
    pub(crate) prepared_work_item_completion:
        Option<Box<crate::runtime::PreparedWorkItemCompletion>>,
    pub(crate) prepared_wait_for: Option<Box<crate::runtime::PreparedWaitForSettlement>>,
}

pub type ToolResult = holon_tools::spec::ToolOutcome<RuntimeToolState>;

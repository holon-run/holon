use anyhow::Result;
use async_trait::async_trait;
use tokio::io::AsyncRead;

use super::{
    types::{RunningProcessExitStatus, StopSignal},
    EffectiveExecution, ProcessRequest, ProcessResult,
};

pub trait ProcessOutput: AsyncRead + Unpin + Send {}

impl<T> ProcessOutput for T where T: AsyncRead + Unpin + Send {}

/// A read-only probe retained from an actual running handle, never a restored PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisteredProcessGroupQuiescence {
    Empty,
    NonEmpty,
    Unknown,
}
pub trait RegisteredProcessGroupProbe: Send + Sync {
    fn observe(&self) -> RegisteredProcessGroupQuiescence;
}

#[async_trait]
pub trait RunningProcess: Send {
    fn id(&self) -> String;
    /// Unsupported backends cannot prove that a registered producer group exited.
    fn registered_group_probe(&self) -> Option<std::sync::Arc<dyn RegisteredProcessGroupProbe>> {
        None
    }
    fn take_stdout(&mut self) -> Option<Box<dyn ProcessOutput>>;
    fn take_stderr(&mut self) -> Option<Box<dyn ProcessOutput>>;
    async fn write_stdin(&mut self, data: &[u8]) -> Result<()>;
    async fn wait(&mut self) -> Result<RunningProcessExitStatus>;
    async fn try_status(&mut self) -> Result<Option<RunningProcessExitStatus>>;
    async fn stop(&mut self, signal: StopSignal) -> Result<()>;
}

#[async_trait]
pub trait ProcessHost: Send + Sync {
    async fn run(
        &self,
        execution: &EffectiveExecution,
        req: ProcessRequest,
    ) -> Result<ProcessResult>;
    async fn spawn(
        &self,
        execution: &EffectiveExecution,
        req: ProcessRequest,
    ) -> Result<Box<dyn RunningProcess>>;
}

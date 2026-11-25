//! Agent Client Protocol (ACP, wire v1) client and external harness profiles.
//!
//! The crate has three layers:
//!
//! - [`jsonrpc`] encodes and decodes NDJSON JSON-RPC 2.0 frames for the single
//!   `stdio` stream. Diagnostics never share that stream.
//! - [`connection`] multiplexes outbound requests, peer-initiated requests, and
//!   `session/update` notifications over one stream.
//! - [`client`] exposes the ACP method lifecycle: `initialize`, `session/new`,
//!   `session/resume`, `session/load`, `session/prompt`, and `session/cancel`.
//!
//! [`harness`] holds the declarative [`AcpHarnessSpec`](harness::AcpHarnessSpec)
//! registry; [`antigravity`] is the first built-in profile (`agy acp`).
//!
//! Runtime integration (spawning the process, projecting updates into the
//! ledger, and wiring execution policy) lives in `holon-runtime`, not here.

pub mod antigravity;
pub mod client;
pub mod connection;
pub mod error;
pub mod handler;
pub mod harness;
pub mod jsonrpc;
pub mod protocol;

pub use client::{AcpClient, CancelHandle, Handshake, DEFAULT_CANCEL_SETTLE_TIMEOUT};
pub use connection::AcpConnection;
pub use error::{AcpError, RpcError, AUTHENTICATION_REQUIRED, SESSION_NOT_FOUND};
pub use handler::{
    AcpClientHandler, DenyAllHandler, ElicitationRequest, PermissionOption, PermissionOutcome,
    PermissionRequest,
};
pub use harness::{
    AcpHarnessRegistry, AcpHarnessSpec, KnownAuthMethod, LaunchCommand, LaunchRequest, SandboxMode,
    SessionOptions, ThinkingLevel,
};
pub use jsonrpc::{Frame, RpcId, JSONRPC_VERSION};
pub use protocol::{
    AgentCapabilities, AuthMethod, ClientCapabilities, ContentBlock, InitializeParams,
    InitializeResult, PromptParams, PromptResult, SessionInfo, SessionLoadParams, SessionNewParams,
    SessionNotification, SessionResumeParams, SessionUpdate, StopReason, ToolCall, ToolCallStatus,
    ACP_PROTOCOL_VERSION,
};

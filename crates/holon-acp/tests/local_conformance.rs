//! Optional conformance probe against a locally installed `agy` binary.
//!
//! The test is opt-in and never depends on a real login: it only verifies that
//! the process starts and that the `initialize` handshake has a compatible
//! structure. Enable with `HOLON_ACP_LOCAL_CONFORMANCE=1`; absent `agy` skips.

use std::sync::Arc;
use std::time::Duration;

use holon_acp::{AcpClient, ClientCapabilities, DenyAllHandler, InitializeParams};
use tokio::process::{Child, Command};
use tokio::time::timeout;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

async fn spawn_agy() -> Option<(AcpClient, Child)> {
    if std::env::var("HOLON_ACP_LOCAL_CONFORMANCE").as_deref() != Ok("1") {
        return None;
    }
    let mut child = Command::new("agy")
        .arg("acp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let stream = tokio::io::join(stdout, stdin);
    Some((AcpClient::new(stream, Arc::new(DenyAllHandler)), child))
}

#[tokio::test]
async fn initialize_handshake_is_structurally_compatible() {
    let Some((client, mut child)) = spawn_agy().await else {
        return;
    };

    let handshake = timeout(
        HANDSHAKE_TIMEOUT,
        client.initialize(InitializeParams::new(ClientCapabilities::trimmed())),
    )
    .await
    .expect("initialize timed out")
    .expect("initialize failed");

    assert_eq!(handshake.protocol_version, holon_acp::ACP_PROTOCOL_VERSION);

    let _ = child.start_kill();
    let _ = child.wait().await;
}

use super::*;
use crate::runtime_db::notifications::{self as inbox, NotificationRecipient};

pub(super) const CHANGED_FRAME: &[u8] = b"event: notifications.changed\ndata: {\"v\":1}\n\n";

pub(super) async fn snapshot(
    host: &crate::host::RuntimeHost,
    recipient: NotificationRecipient,
) -> anyhow::Result<(Vec<inbox::InboxNotification>, Vec<inbox::InboxNotification>)> {
    host.runtime_db()
        .with_read_async(move |conn| {
            let now = chrono::Utc::now();
            let view = conn.transaction()?;
            let snapshot = (
                inbox::pending(&view, &recipient, now)?,
                inbox::recent(&view, &recipient, now)?,
            );
            view.commit()?;
            Ok(snapshot)
        })
        .await
}

/// These are post-commit hints, not notification eligibility. A subscriber
/// gets a frame only when its own authoritative snapshot changes.
pub(super) fn could_change(kind: &str) -> bool {
    kind.starts_with("wait_condition_")
        || matches!(
            kind,
            "brief_created"
                | "work_item_written"
                | "work_item_completed"
                | "agent_paused"
                | "agent_stopped"
                | "agent_resumed"
                | "agent_archived"
                | "agent_deleted"
        )
}

pub(super) fn recipient(
    headers: &HeaderMap,
    state: &AppState,
) -> AuthenticationResult<NotificationRecipient> {
    authorize_control(headers, state)?;
    let actor = control_actor(headers, state)?;
    Ok(NotificationRecipient {
        principal_id: actor.principal_id(),
        local_control: actor == ControlActor::LocalControl,
    })
}

pub(super) async fn pending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let recipient = recipient(&headers, &state).map_err(authentication_error_response)?;
    let items = state
        .host
        .runtime_db()
        .with_read_async(move |conn| inbox::pending(conn, &recipient, chrono::Utc::now()))
        .await
        .map_err(error_response)?;
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(inbox::InboxNotificationList { items }),
    ))
}

pub(super) async fn recent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let recipient = recipient(&headers, &state).map_err(authentication_error_response)?;
    let items = state
        .host
        .runtime_db()
        .with_read_async(move |conn| inbox::recent(conn, &recipient, chrono::Utc::now()))
        .await
        .map_err(error_response)?;
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(inbox::InboxNotificationList { items }),
    ))
}

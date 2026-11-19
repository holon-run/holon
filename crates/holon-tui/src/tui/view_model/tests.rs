use super::{ellipsize, shorten_home_path_with_home, workspace_label, StatusbarViewModel};
use crate::{
    client::LocalClient,
    tui::{
        logging::TuiLogWriter,
        overlay::OverlayState,
        runtime::TuiConnectionState,
        tests::{sample_agent_summary, test_config},
        TuiApp,
    },
    types::{ActiveWorkspaceEntry, AGENT_HOME_WORKSPACE_ID},
};
use std::{path::Path, time::Duration};
use unicode_width::UnicodeWidthStr;

fn statusbar_app(execution_root: &str) -> TuiApp {
    let mut app = TuiApp::new(
        LocalClient::new(test_config()).unwrap(),
        TuiLogWriter::new_temp().unwrap(),
    );
    let mut agent = sample_agent_summary("statusbar-agent");
    agent.identity.name = Some("助手-agent👩‍💻".into());
    agent.agent.active_workspace_entry = Some(ActiveWorkspaceEntry {
        workspace_id: "ws-statusbar".into(),
        workspace_anchor: "/tmp/project".into(),
        execution_root_id: "worktree:statusbar".into(),
        execution_root: execution_root.into(),
        projection_kind: crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
        access_mode: crate::system::WorkspaceAccessMode::ExclusiveWrite,
        cwd: execution_root.into(),
        occupancy_id: None,
        projection_metadata: None,
    });
    app.agents = vec![agent];
    app.connection_state = TuiConnectionState::Streaming;
    app.status_line = "Ready".into();
    app
}

#[test]
fn statusbar_reserves_agent_and_model_before_long_paths() {
    let root = format!("/tmp/{}/project", "很长的路径-with-unicode/".repeat(12));
    let mut app = statusbar_app(&root);
    app.status_line = "操作已完成-detail ".repeat(30);

    for width in [80, 84, 120] {
        let view_model = StatusbarViewModel::for_width(&app, false, width);
        assert!(view_model.context_line.width() <= usize::from(width));
        assert!(view_model.status_line.width() <= usize::from(width));
        assert!(view_model.context_line.contains("助手-agent👩‍💻"));
        assert!(view_model
            .context_line
            .contains("anthropic/claude-sonnet-4-6"));
        assert!(view_model.context_line.contains('…'));
        assert!(view_model.status_line.starts_with("streaming · "));
        assert!(view_model.status_line.ends_with('…'));
        assert!(!view_model.context_line.contains('\n'));
        assert!(!view_model.status_line.contains('\n'));
    }
}

#[test]
fn statusbar_keeps_wide_details_and_legacy_view_model() {
    let app = statusbar_app("/tmp/project");
    let legacy = StatusbarViewModel::from_app(&app, false);
    let fitted = StatusbarViewModel::for_width(&app, false, 120);

    assert_eq!(
        fitted.context_line,
        format!("助手-agent👩‍💻 · {}", legacy.context_line)
    );
    assert_eq!(fitted.status_line, legacy.status_line);
    assert!(legacy.context_line.starts_with("project ("));
    assert!(!legacy.context_line.contains("助手-agent👩‍💻"));
    assert!(!fitted.context_line.contains('…'));
}

#[test]
fn statusbar_handles_zero_and_tiny_widths() {
    let app = statusbar_app("/tmp/project");
    for width in 0..=12 {
        let view_model = StatusbarViewModel::for_width(&app, false, width);
        assert!(view_model.context_line.width() <= usize::from(width));
        assert!(view_model.status_line.width() <= usize::from(width));
        if width == 0 {
            assert!(view_model.context_line.is_empty());
            assert!(view_model.status_line.is_empty());
        } else if width == 1 {
            assert_eq!(view_model.context_line, "…");
            assert_eq!(view_model.status_line, "…");
        }
    }
}

#[test]
fn statusbar_keeps_model_visible_with_a_long_agent_name() {
    let mut app = statusbar_app("/tmp/project");
    app.agents[0].identity.name = Some("助手👩‍💻-long-agent-name-".repeat(20));
    for width in [80, 84, 120] {
        let view_model = StatusbarViewModel::for_width(&app, false, width);
        assert!(view_model.context_line.width() <= usize::from(width));
        assert!(view_model
            .context_line
            .ends_with("anthropic/claude-sonnet-4-6"));
        assert!(view_model.context_line.contains('…'));
    }
}

#[test]
fn statusbar_ellipsizes_display_columns_without_splitting_graphemes() {
    assert_eq!(ellipsize("界a👩‍💻e\u{301}Z", 6), "界a👩‍💻…");
    assert_eq!(ellipsize("e\u{301}模型", 2), "e\u{301}…");
    assert_eq!(ellipsize("界", 2), "界");
    assert_eq!(ellipsize("界", 1), "…");
    assert_eq!(ellipsize("界", 0), "");
    assert_eq!(ellipsize("", 1), "");

    for width in 0..=20 {
        assert!(ellipsize("abc中文👩‍💻e\u{301}xyz", width).width() <= width);
    }
}

#[test]
fn statusbar_prioritizes_connection_failures_over_stale_success_and_overlay_hints() {
    let mut app = statusbar_app("/tmp/project");
    app.status_line = "Successfully loaded agents".into();
    app.overlay = OverlayState::Tasks {
        selected: 0,
        detail_scroll: 0,
    };
    for (state, label) in [
        (
            TuiConnectionState::Reconnecting {
                attempt: 3,
                retry_after: Duration::from_secs(2),
                last_error: format!("连接失败: socket closed {}", "detail/".repeat(40)),
            },
            "reconnecting",
        ),
        (
            TuiConnectionState::Disconnected {
                reason: format!("连接失败: daemon stopped {}", "detail/".repeat(40)),
            },
            "disconnected",
        ),
    ] {
        app.connection_state = state;
        let legacy = StatusbarViewModel::from_app(&app, false);
        assert!(legacy.status_line.contains("连接失败:"));
        assert!(!legacy.status_line.contains("Successfully"));
        assert!(!legacy.status_line.contains("Tasks:"));
        for width in [80, 84, 120] {
            let view_model = StatusbarViewModel::for_width(&app, false, width);
            assert!(view_model.status_line.starts_with(label));
            assert!(view_model.status_line.contains("连接失败:"));
            assert!(view_model.status_line.width() <= usize::from(width));
            assert!(view_model.status_line.ends_with('…'));
        }
    }
}

#[test]
fn statusbar_explains_turn_detail_navigation_and_folding() {
    let mut app = statusbar_app("/tmp/project");
    app.overlay = OverlayState::TurnDetails {
        selected_id: None,
        detail_scroll: 0,
    };
    let view_model = StatusbarViewModel::for_width(&app, false, 120);
    assert!(view_model.status_line.contains("Up/Down select"));
    assert!(view_model.status_line.contains("Enter fold/unfold"));
    assert!(view_model.status_line.contains("PgUp/PgDn"));
    assert!(view_model.status_line.contains("Esc"));
}

#[test]
fn statusbar_preserves_healthy_overlay_and_transient_status_precedence() {
    let mut app = statusbar_app("/tmp/project");
    app.status_line = "Successfully loaded agents".into();
    assert!(StatusbarViewModel::from_app(&app, false)
        .status_line
        .contains("Successfully loaded agents"));
    app.overlay = OverlayState::Tasks {
        selected: 0,
        detail_scroll: 0,
    };
    let view_model = StatusbarViewModel::for_width(&app, false, 120);
    assert!(view_model.status_line.contains("Tasks:"));
    assert!(!view_model.status_line.contains("Successfully"));
}

#[test]
fn workspace_label_uses_anchor_name_not_random_workspace_id() {
    assert_eq!(
        workspace_label(
            "ws-123456",
            Path::new("/Users/example/opensource/src/github.com/holon-run/holon")
        ),
        "holon"
    );
    assert_eq!(
        workspace_label(AGENT_HOME_WORKSPACE_ID, Path::new("/tmp/default")),
        "agent_home"
    );
}

#[test]
fn shorten_home_path_uses_tilde_for_home_relative_paths() {
    assert_eq!(
        shorten_home_path_with_home(
            Path::new("/Users/example/opensource/holon"),
            Some(Path::new("/Users/example"))
        ),
        "~/opensource/holon"
    );
}

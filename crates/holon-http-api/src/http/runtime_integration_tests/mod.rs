use super::*;
use crate::runtime::http_test_support as fixture;
use crate::{
    config::{AppConfig, RuntimeModelCatalog},
    host::{
        projection_hydration_references, AgentHydrationReferenceData, AgentStateProjectionSource,
        ObserverSyncRecordKindData,
    },
    types::{
        AgentIdentityRecord, AgentIdentityView, AgentKind, AgentLifecycleHint, AgentListEntry,
        AgentStatus, AgentVisibility, ControlAction, PendingWakeHint,
    },
};
use anyhow::Context;
use fixture::test_host;
use std::time::Duration;

mod agent_summary_scheduling_tests;
mod callback_discovery_tests;
mod child_summary_ownership_tests;
mod cold_scheduling_tests;
mod complete_api_tests;
mod event_read_tests;
mod event_stream_http_tests;
mod observer_sync_http_tests;
mod operator_state_http_tests;
mod projection_brief_preview_tests;
mod projection_hydration_tests;
mod projection_identity_move_tests;
mod projection_lifecycle_material_tests;
mod projection_model_catalog_view_tests;
mod projection_model_material_tests;
mod projection_model_move_tests;
mod projection_model_summary_tests;
mod projection_model_tier_material_tests;
mod projection_posture_material_tests;
mod projection_stopped_state_material_tests;
mod projection_storage_view_tests;
mod quarantine_tests;
mod reader_pool_http_tests;
mod resource_writer_tests;
mod work_planning_http_tests;

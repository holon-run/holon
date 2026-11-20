//! HTTP state DTOs and conversion from the runtime observation.

pub use holon_client::http_dto::*;

impl From<&crate::runtime::LightweightAgentStateProjection> for SlimAgentDto {
    fn from(projection: &crate::runtime::LightweightAgentStateProjection) -> Self {
        Self {
            identity: projection.identity.clone(),
            agent: (&projection.agent).into(),
            scheduling_posture: projection.scheduling_posture.clone(),
            active_task_count: projection.active_task_count,
            lifecycle: projection.lifecycle.clone(),
            model: (&projection.model).into(),
            closure: (&projection.closure).into(),
            active_children: Vec::new(),
        }
    }
}

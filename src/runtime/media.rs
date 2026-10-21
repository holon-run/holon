use anyhow::Result;

use crate::{config::ModelRouteRef, provider::media::MediaToolDefaults};

use super::RuntimeHandle;

impl RuntimeHandle {
    pub(crate) async fn media_tool_defaults(
        &self,
        pending_fallback: Option<&ModelRouteRef>,
    ) -> Result<MediaToolDefaults> {
        let state = self.agent_state().await?;
        let current_fallback = self.inner.turn_fallback_model.read().await.clone();
        let pending_fallback = pending_fallback.or(current_fallback.as_ref());
        let snap = self.inner.config_snapshot.load();
        // Discovery is side-effect free: do not advance the vision health cursor.
        let vision = snap
            .model_catalog
            .select_view_image_vision_model_with_route(
                &snap.base_context_config,
                state.model_override.as_ref(),
                pending_fallback,
            );
        let generation = snap.model_catalog.select_generate_image_route(
            &snap.base_context_config,
            state.model_override.as_ref(),
            pending_fallback,
        );
        Ok(MediaToolDefaults {
            view_image: vision.selected_route.map(|route| route.as_string()),
            generate_image: generation.map(|route| route.route_ref.as_string()),
            output_format: "png".into(),
        })
    }
}

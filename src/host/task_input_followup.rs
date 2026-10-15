//! A TaskInput callback keeps its original child identity across idle replacement.
use super::*;
use crate::runtime::OriginalTaskInputChildBinding;
impl RuntimeHostBridge {
    pub(crate) async fn deliver_original_task_input_followup(
        &self,
        binding: Arc<OriginalTaskInputChildBinding>,
        input: &str,
        authority: AuthorityClass,
    ) -> Result<bool> {
        let child = binding.child_scope().agent_id.as_str();
        if !self.reusable_agent_exists(child).await? {
            return Ok(false);
        }
        let runtime = match self.host()?.get_or_create_agent(child).await {
            Ok(runtime) => runtime,
            Err(error) => {
                if !self.reusable_agent_exists(child).await? {
                    return Ok(false);
                }
                return Err(error);
            }
        };
        let mut message = MessageEnvelope::new(
            child,
            MessageKind::InternalFollowup,
            MessageOrigin::Task {
                task_id: binding.task_id().into(),
            },
            authority,
            Priority::Normal,
            MessageBody::Text { text: input.into() },
        )
        .with_admission(
            MessageDeliverySurface::RuntimeSystem,
            AdmissionContext::RuntimeOwned,
        );
        message.metadata = Some(json!({
            "delegated_task_id":binding.task_id(), "parent_agent_id":binding.parent_id(),
            "child_agent_id":child, "followup_via":"task_input", "delegated_authority_class":authority,
        }));
        self.enqueue_original_task_input_followup(runtime, binding, message)
            .await
    }
    pub(super) async fn enqueue_original_task_input_followup(
        &self,
        mut runtime: RuntimeHandle,
        binding: Arc<OriginalTaskInputChildBinding>,
        message: MessageEnvelope,
    ) -> Result<bool> {
        for _ in 0..3 {
            // A new Runtime epoch for this same durable identity is allowed.
            // A successor incarnation or another creator is never substituted.
            if runtime.instance_incarnation() != binding.child_scope().identity_incarnation {
                return Ok(false);
            }
            let attempt = async {
                // Detect an old retiring handle before attempting its Root.
                // The actual admission lease also fences idle retirement while
                // this accepted callback validates its immutable identity.
                let _admission = runtime.execution_admission_lease()?;
                let (identity, identity_guard) = runtime.capture_active_identity_read().await?;
                let original_identity = binding.validate_identity(&identity).is_ok();
                // The DB phase rechecks its actual identity. Do not retain a
                // selected identity allocation through a separate enqueue phase.
                drop((identity, identity_guard));
                if !original_identity {
                    return Ok(false);
                }
                let material = binding.for_attempt(&runtime, message.authority_class)?;
                runtime
                    .enqueue_original_task_input_child(message.clone(), material)
                    .await?;
                Ok::<_, anyhow::Error>(true)
            }
            .await;
            match attempt {
                Ok(delivered) => return Ok(delivered),
                Err(error)
                    if describe_runtime_error(&error).code == "runtime_instance_retiring" =>
                {
                    runtime = match self.host()?.get_or_create_agent(&message.agent_id).await {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            if !self.reusable_agent_exists(&message.agent_id).await? {
                                return Ok(false);
                            }
                            return Err(error);
                        }
                    };
                }
                Err(error) => return Err(error),
            }
        }
        Err(crate::runtime::execution_admission::closed_error())
    }
}

//! Freeze Control identity and provenance once at the authorized entry.

use super::*;
use crate::runtime_db::control_transition::{ControlInvocation, ControlKey, ControlSource};

impl RuntimeHandle {
    /// Only a Host-owned, already-admitted retirement handoff calls this path.
    pub(crate) async fn apply_host_admitted_control(
        &self,
        key: ControlKey,
        invocation: ControlInvocation,
        admission: super::execution_admission::ExecutionLease,
    ) -> Result<()> {
        let _admission = admission;
        self.apply_frozen_control_command(key, invocation).await
    }

    pub(crate) fn freeze_direct_control(
        &self,
        action: ControlAction,
        authority_class: AuthorityClass,
    ) -> (ControlKey, ControlInvocation) {
        (
            ControlKey {
                agent_id: self.inner.mutation_owner.agent_id().to_owned(),
                identity_incarnation: self.inner.identity_incarnation,
                command_id: crate::ids::audit_event_id(),
            },
            ControlInvocation {
                action,
                source: ControlSource::DirectApi,
                authority_class,
            },
        )
    }

    fn freeze_message_control(
        &self,
        message: &MessageEnvelope,
        action: ControlAction,
    ) -> Result<(ControlKey, ControlInvocation)> {
        anyhow::ensure!(
            message.kind == MessageKind::Control
                && message.agent_id == self.inner.mutation_owner.agent_id()
                && !message.id.is_empty(),
            "Control message has a different canonical scope"
        );
        Ok((
            ControlKey {
                agent_id: message.agent_id.clone(),
                identity_incarnation: self.inner.identity_incarnation,
                command_id: format!("control-message/v1:{}", message.id),
            },
            ControlInvocation {
                action,
                source: ControlSource::ControlMessage {
                    message_id: message.id.clone(),
                },
                authority_class: message.authority_class,
            },
        ))
    }

    pub(super) async fn control_message(
        &self,
        message: &MessageEnvelope,
        action: ControlAction,
    ) -> Result<()> {
        let (key, invocation) = self.freeze_message_control(message, action)?;
        let admission = self.execution_admission_lease()?;
        self.control_command_admitted(key, invocation, admission)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

    #[test]
    fn direct_controls_are_new_intents_and_message_controls_keep_canonical_identity() {
        let home = tempdir().unwrap();
        let runtime = RuntimeHandle::new(
            "control-identity",
            home.path().to_path_buf(),
            InitialWorkspaceBinding::Detached,
            String::new(),
            Arc::new(StubProvider::new("unused")),
            "control-identity".into(),
            context_config(),
        )
        .unwrap();
        let (first, first_invocation) =
            runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::RuntimeInstruction);
        let (second, _) =
            runtime.freeze_direct_control(ControlAction::Stop, AuthorityClass::RuntimeInstruction);
        assert_ne!(first.command_id, second.command_id);
        assert_eq!(
            first_invocation.authority_class,
            AuthorityClass::RuntimeInstruction
        );
        let message = MessageEnvelope::new(
            "control-identity",
            MessageKind::Control,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Next,
            MessageBody::Text {
                text: "stop".into(),
            },
        );
        let (key, invocation) = runtime
            .freeze_message_control(&message, ControlAction::Stop)
            .unwrap();
        let replay = runtime
            .freeze_message_control(&message, ControlAction::Stop)
            .unwrap();
        assert_eq!(key, replay.0);
        assert_eq!(invocation, replay.1);
        assert_eq!(invocation.authority_class, message.authority_class);
        let mut foreign = message;
        foreign.agent_id = "other-agent".into();
        assert!(runtime
            .freeze_message_control(&foreign, ControlAction::Stop)
            .is_err());
    }
}

//! Retired persisted representations used by database cleanup.
#[cfg(all(test, holon_lib_test_shard = "b"))]
pub(super) use holon_storage::legacy_scheduler_wire::{
    ActivationBinding, ActivationCause, ActivationInputAttachment, ActivationOrigin,
    ActivationPriority, ActivationProvenance, ActivationTrust, AgentActivation, PreemptionPolicy,
    WorkDemand, WorkStatus,
};
pub(super) use holon_storage::legacy_scheduler_wire::{
    ActivationLifecycleState, AdmitActivationCommand,
};

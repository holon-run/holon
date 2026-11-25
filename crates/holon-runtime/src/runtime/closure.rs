//! Materialize closure evidence under the runtime's read budget.

use crate::types::ClosureDecision;
use holon_domain::closure;

pub(super) use holon_domain::closure::ClosureFacts;
pub(crate) use holon_domain::closure::{runtime_error_active, runtime_error_active_at};

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
#[path = "closure_material_tests.rs"]
mod material_tests;

/// Admit the exact formatted string and its Vec slot before either allocation.
fn push_evidence(evidence: &mut Vec<String>, value: std::fmt::Arguments<'_>) -> anyhow::Result<()> {
    struct Length(usize);
    impl std::fmt::Write for Length {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            self.0 = self.0.checked_add(value.len()).ok_or(std::fmt::Error)?;
            Ok(())
        }
    }
    let mut length = Length(0);
    std::fmt::write(&mut length, value)?;
    crate::diagnostics::resource_io::charge_owned_metadata(u64::try_from(length.0)?, 1)?;
    crate::runtime_db::repositories::reserve_source_vec_slot(evidence)?;
    let mut string = String::with_capacity(length.0);
    std::fmt::write(&mut string, value)?;
    anyhow::ensure!(string.len() == length.0, "closure evidence size changed");
    evidence.push(string);
    Ok(())
}

pub(super) fn derive_closure_decision(facts: ClosureFacts) -> anyhow::Result<ClosureDecision> {
    crate::diagnostics::resource_io::charge_owned_metadata(
        u64::try_from(std::mem::size_of::<ClosureDecision>())?,
        1,
    )?;
    closure::derive_closure_decision(facts, push_evidence)
}

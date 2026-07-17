mod china;
pub(super) mod common;
mod core;
mod gateways;
mod hosted;
mod tencent_tokenhub;

pub(crate) use tencent_tokenhub::is_tencent_tokenhub_model_id;

pub(super) fn built_in_entries() -> Vec<super::BuiltInModelMetadata> {
    let mut entries = core::entries();
    entries.extend(hosted::early_entries());
    entries.extend(gateways::kilocode_entries());
    entries.extend(hosted::middle_entries());
    entries.extend(gateways::opencode_entries());
    entries.extend(china::early_entries());
    entries.extend(hosted::late_entries());
    entries.extend(china::late_entries());
    entries.extend(tencent_tokenhub::entries());
    entries
}

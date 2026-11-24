//! Render the original ordering and authority of admitted input items.

use holon_domain::IdleInputBatchRecord;

pub const BATCH_HEADER: &str =
    "Current input batch (receive order; each item retains its own authority and reply route):\n";

pub fn render_batch(batch: &IdleInputBatchRecord) -> String {
    let mut text = String::from(BATCH_HEADER);
    for item in &batch.items {
        text.push_str(&item.rendered);
    }
    text
}

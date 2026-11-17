//! SQLite search projection over prepared memory documents.

mod index;
pub mod projection;
mod write_coordinator;

pub use index::*;
pub use write_coordinator::WriterMetrics;

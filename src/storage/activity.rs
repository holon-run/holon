//! Poll activity markers for storage freshness checks.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileActivityMarker {
    pub exists: bool,
    pub len: u64,
    pub modified_unix_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollActivityMarker {
    pub briefs: FileActivityMarker,
    pub tasks: FileActivityMarker,
    pub tools: FileActivityMarker,
    pub events: FileActivityMarker,
    pub transcript: FileActivityMarker,
}

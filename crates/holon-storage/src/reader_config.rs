use anyhow::{ensure, Result};

pub const DEFAULT_ORDINARY_MAX_CONNECTIONS: u32 = 16;
pub const DEFAULT_PREPARATION_MAX_CONNECTIONS: u32 = 4;
pub const MAX_READER_CONNECTIONS: u32 = 64;

/// Limits shared by all clones of one RuntimeDb, fixed at construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDbReaderConfig {
    ordinary_max_connections: u32,
    preparation_max_connections: u32,
}

impl RuntimeDbReaderConfig {
    pub fn new(ordinary_max_connections: u32, preparation_max_connections: u32) -> Result<Self> {
        ensure!(
            ordinary_max_connections > 0,
            "runtime.db_readers.ordinary_max_connections must be positive"
        );
        ensure!(
            preparation_max_connections > 0,
            "runtime.db_readers.preparation_max_connections must be positive"
        );
        ensure!(u64::from(ordinary_max_connections) + u64::from(preparation_max_connections)
            <= u64::from(MAX_READER_CONNECTIONS),
            "runtime.db_readers combined maximum must not exceed {MAX_READER_CONNECTIONS} connections");
        Ok(Self {
            ordinary_max_connections,
            preparation_max_connections,
        })
    }

    pub fn ordinary_max_connections(self) -> u32 {
        self.ordinary_max_connections
    }

    pub fn preparation_max_connections(self) -> u32 {
        self.preparation_max_connections
    }
}

impl Default for RuntimeDbReaderConfig {
    fn default() -> Self {
        Self {
            ordinary_max_connections: DEFAULT_ORDINARY_MAX_CONNECTIONS,
            preparation_max_connections: DEFAULT_PREPARATION_MAX_CONNECTIONS,
        }
    }
}

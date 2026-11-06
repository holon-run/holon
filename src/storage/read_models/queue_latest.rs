use anyhow::{Context, Result};

use crate::types::WorkItemRecord;

type Entry = (String, WorkItemRecord, usize);

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests;

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod state_tests;

type StateEntry = (WorkItemRecord, usize);

/// State's key is the owned record id, not a second owned string.
#[derive(Default)]
pub(super) struct StateQueueWorkBuilder(Vec<StateEntry>);

impl StateQueueWorkBuilder {
    pub(super) fn push(&mut self, record: WorkItemRecord) -> Result<()> {
        if self.0.len() == self.0.capacity() {
            let capacity = self.0.capacity();
            let additional = capacity.max(4);
            let bytes = capacity
                .checked_add(additional)
                .and_then(|_| additional.checked_mul(std::mem::size_of::<StateEntry>()))
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(|| {
                    std::io::Error::other("ResourceExhausted: state queue latest slots")
                })?;
            crate::diagnostics::resource_io::charge_owned_metadata(bytes, additional)
                .context("state queue latest slots admission")?;
            self.0.try_reserve_exact(additional).map_err(|_| {
                std::io::Error::other("ResourceExhausted: state queue latest allocation")
            })?;
        }
        let ordinal = self.0.len();
        self.0.push((record, ordinal));
        Ok(())
    }

    pub(super) fn finish(mut self) -> StateQueueLatestWork {
        self.0.sort_unstable_by(|left, right| {
            left.0
                .id
                .cmp(&right.0.id)
                .then_with(|| right.1.cmp(&left.1))
        });
        self.0.dedup_by(|left, right| left.0.id == right.0.id);
        StateQueueLatestWork(self.0)
    }
}

pub(super) struct StateQueueLatestWork(Vec<StateEntry>);

impl StateQueueLatestWork {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn into_values(self) -> impl Iterator<Item = WorkItemRecord> {
        self.0.into_iter().map(|entry| entry.0)
    }
}

/// Append in source order; sort only after all records have been admitted.
#[derive(Default)]
pub(crate) struct QueueWorkBuilder(Vec<Entry>);

impl QueueWorkBuilder {
    pub(crate) fn push(&mut self, key: String, record: WorkItemRecord) -> Result<()> {
        if self.0.len() == self.0.capacity() {
            let capacity = self.0.capacity();
            let additional = capacity.max(4);
            let bytes = capacity
                .checked_add(additional)
                .and_then(|_| additional.checked_mul(std::mem::size_of::<Entry>()))
                .and_then(|bytes| u64::try_from(bytes).ok())
                .ok_or_else(|| std::io::Error::other("ResourceExhausted: queue latest slots"))?;
            crate::diagnostics::resource_io::charge_owned_metadata(bytes, additional)
                .context("queue latest slots admission")?;
            self.0
                .try_reserve_exact(additional)
                .map_err(|_| std::io::Error::other("ResourceExhausted: queue latest allocation"))?;
        }
        let ordinal = self.0.len();
        self.0.push((key, record, ordinal));
        Ok(())
    }

    pub(crate) fn finish(mut self) -> QueueLatestWork {
        self.0.sort_unstable_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| right.2.cmp(&left.2))
        });
        self.0.dedup_by(|left, right| left.0 == right.0);
        QueueLatestWork(self.0)
    }
}

#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct QueueLatestWork(Vec<Entry>);

impl QueueLatestWork {
    pub(crate) fn get(&self, id: &str) -> Option<&WorkItemRecord> {
        self.0
            .binary_search_by(|entry| entry.0.as_str().cmp(id))
            .ok()
            .map(|index| &self.0[index].1)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &WorkItemRecord> + Clone {
        self.0.iter().map(|entry| &entry.1)
    }

    pub(crate) fn into_values(self) -> impl Iterator<Item = WorkItemRecord> {
        self.0.into_iter().map(|entry| entry.1)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// Fixtures prepare canonical inputs outside the allocation boundary under test.
#[cfg(test)]
impl QueueLatestWork {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn insert(&mut self, key: String, record: WorkItemRecord) {
        match self.0.binary_search_by(|entry| entry.0.cmp(&key)) {
            Ok(index) => self.0[index].1 = record,
            Err(index) => self.0.insert(index, (key, record, 0)),
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &WorkItemRecord)> {
        self.0.iter().map(|entry| (&entry.0, &entry.1))
    }
}

#[cfg(test)]
impl FromIterator<(String, WorkItemRecord)> for QueueLatestWork {
    fn from_iter<T: IntoIterator<Item = (String, WorkItemRecord)>>(records: T) -> Self {
        QueueWorkBuilder(
            records
                .into_iter()
                .enumerate()
                .map(|(ordinal, (key, record))| (key, record, ordinal))
                .collect(),
        )
        .finish()
    }
}

#[cfg(test)]
impl<const N: usize> From<[(String, WorkItemRecord); N]> for QueueLatestWork {
    fn from(records: [(String, WorkItemRecord); N]) -> Self {
        records.into_iter().collect()
    }
}

//! Synchronous refresh-only allocation gate; no request-time filesystem fallback.
use crate::runtime::input_evidence_budget::OwnedCompletionBudget;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{DirEntry, File, ReadDir},
    io::{self, Read},
    marker::PhantomData,
    path::Path,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

thread_local! { static ACTIVE: RefCell<Option<Arc<Mutex<RefreshBudget>>>> = const { RefCell::new(None) }; }
struct RefreshBudget {
    bytes: u64,
    records: usize,
    byte_limit: u64,
    record_limit: usize,
    material: Option<OwnedCompletionBudget>,
    failure: Option<String>,
    versions: BTreeMap<PathBuf, ObservedResourceVersion>,
}
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) enum ObservedResourceVersion {
    Present(String),
    Absent,
    RefreshFailed(String),
}
pub(crate) struct ResourceReadBudget {
    active: Arc<Mutex<RefreshBudget>>,
    previous: Option<Arc<Mutex<RefreshBudget>>>,
    _thread: PhantomData<Rc<()>>,
}
impl ResourceReadBudget {
    pub(crate) fn enter(
        material: OwnedCompletionBudget,
        byte_limit: u64,
        record_limit: usize,
    ) -> io::Result<Self> {
        if byte_limit == 0 || record_limit == 0 {
            return Err(io::Error::other(
                "ResourceExhausted: zero refresh allowance",
            ));
        }
        let active = Arc::new(Mutex::new(RefreshBudget {
            bytes: 0,
            records: 0,
            byte_limit,
            record_limit,
            material: Some(material),
            failure: None,
            versions: BTreeMap::new(),
        }));
        let previous = ACTIVE.with(|slot| slot.replace(Some(active.clone())));
        Ok(Self {
            active,
            previous,
            _thread: PhantomData,
        })
    }
    pub(crate) fn finish(&self) -> io::Result<(u64, usize)> {
        let budget = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(error) = &budget.failure {
            return Err(io::Error::other(error.clone()));
        }
        Ok((budget.bytes, budget.records))
    }
    pub(crate) fn observed_versions(
        &self,
    ) -> io::Result<BTreeMap<PathBuf, ObservedResourceVersion>> {
        let (bytes, records) = {
            let budget = self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let bytes = crate::runtime::input_evidence_budget::serialized_bytes(&budget.versions)
                .map_err(io::Error::other)?;
            (
                bytes,
                budget
                    .versions
                    .len()
                    .checked_mul(4)
                    .ok_or_else(|| io::Error::other("resource version metadata overflow"))?,
            )
        };
        charge(&self.active, bytes, records, false)?;
        Ok(self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .versions
            .clone())
    }
    pub(crate) fn completion_guard(
        &self,
    ) -> io::Result<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        self.finish()?;
        Ok(self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .material
            .as_ref()
            .ok_or_else(|| io::Error::other("resource refresh material already sealed"))?
            .guard())
    }
    /// End the refresh allocation phase and transfer only retained bytes.
    /// Further I/O charges are rejected after this one-shot ownership handoff.
    pub(crate) fn seal_retained(
        &self,
        bytes: u64,
        records: usize,
    ) -> io::Result<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        let mut budget = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if budget.failure.is_some() || bytes > budget.byte_limit || records > budget.record_limit {
            return Err(exhausted(&mut budget));
        }
        let material = budget
            .material
            .take()
            .ok_or_else(|| io::Error::other("resource refresh material already sealed"))?;
        material
            .seal_resource_observation(crate::runtime::input_evidence_budget::OwnedBytesEstimate {
                payload_bytes: bytes,
                metadata_estimate_bytes: 0,
            })
            .map_err(io::Error::other)
    }
    pub(crate) fn charge_metadata(&self, bytes: u64, records: usize) -> io::Result<()> {
        charge(&self.active, bytes, records, false)
    }
}
impl Drop for ResourceReadBudget {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}
fn exhausted(budget: &mut RefreshBudget) -> io::Error {
    let text = "ResourceExhausted: context resource refresh material";
    budget.failure = Some(text.into());
    io::Error::other(text)
}
fn charge(
    active: &Arc<Mutex<RefreshBudget>>,
    bytes: u64,
    records: usize,
    payload: bool,
) -> io::Result<()> {
    let mut budget = active.lock().unwrap_or_else(|error| error.into_inner());
    if budget.failure.is_some() {
        return Err(exhausted(&mut budget));
    }
    let added = bytes.checked_mul(8).ok_or_else(|| exhausted(&mut budget))?;
    let total = budget
        .bytes
        .checked_add(added)
        .ok_or_else(|| exhausted(&mut budget))?;
    let count = records
        .checked_mul(8)
        .and_then(|n| budget.records.checked_add(n))
        .ok_or_else(|| exhausted(&mut budget))?;
    if total > budget.byte_limit || count > budget.record_limit {
        return Err(exhausted(&mut budget));
    }
    // Observe before the caller allocates or extends its owned buffer/collection.
    budget
        .material
        .as_ref()
        .ok_or_else(|| io::Error::other("resource refresh material already sealed"))?
        .observe_material(
            if payload { added } else { 0 },
            if payload { 0 } else { added },
        );
    budget.bytes = total;
    budget.records = count;
    Ok(())
}
fn record_version(
    active: &Arc<Mutex<RefreshBudget>>,
    path: &Path,
    version: ObservedResourceVersion,
) -> io::Result<()> {
    charge(
        active,
        (path.as_os_str().len() as u64)
            .checked_add(512)
            .ok_or_else(|| io::Error::other("resource version path overflow"))?,
        4,
        false,
    )?;
    active
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .versions
        .insert(path.to_path_buf(), version);
    Ok(())
}
pub(crate) fn observe_read_error(path: &Path, error: &io::Error) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let version = if error.kind() == io::ErrorKind::NotFound {
        ObservedResourceVersion::Absent
    } else {
        ObservedResourceVersion::RefreshFailed(error.to_string().chars().take(256).collect())
    };
    record_version(&active, path, version)
}
pub(crate) fn read_to_string(path: &Path) -> Option<io::Result<String>> {
    use sha2::{Digest, Sha256};
    let active = ACTIVE.with(|slot| slot.borrow().clone())?;
    Some((|| {
        charge(
            &active,
            (path.as_os_str().len() as u64)
                .checked_mul(4)
                .and_then(|n| n.checked_add(512))
                .ok_or_else(|| io::Error::other("resource path overflow"))?,
            1,
            false,
        )?;
        let result = (|| {
            let mut file = File::open(path)?;
            let declared = file.metadata()?.len();
            let remaining = {
                let budget = active.lock().unwrap_or_else(|error| error.into_inner());
                budget.byte_limit.saturating_sub(budget.bytes) / 8
            };
            if declared > remaining {
                return Err(exhausted(
                    &mut active.lock().unwrap_or_else(|error| error.into_inner()),
                ));
            }
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut digest = Sha256::new();
            loop {
                let count = file.read(&mut chunk)?;
                if count == 0 {
                    break;
                }
                charge(&active, count as u64, 0, true)?;
                digest.update(&chunk[..count]);
                bytes.extend_from_slice(&chunk[..count]);
            }
            record_version(
                &active,
                path,
                ObservedResourceVersion::Present(format!("{:x}", digest.finalize())),
            )?;
            String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })();
        if let Err(error) = &result {
            observe_read_error(path, error)?;
        }
        result
    })())
}
pub(crate) fn read_to_end(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> Option<io::Result<usize>> {
    let active = ACTIVE.with(|slot| slot.borrow().clone())?;
    Some((|| {
        let before = buffer.len();
        let mut chunk = [0u8; 8192];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            charge(&active, count as u64, 0, true)?;
            buffer.extend_from_slice(&chunk[..count]);
        }
        Ok(buffer.len() - before)
    })())
}
pub(crate) struct ResourceReadDir {
    inner: ReadDir,
    active: Option<Arc<Mutex<RefreshBudget>>>,
    stopped: bool,
}
impl ResourceReadDir {
    pub(crate) fn new(inner: ReadDir) -> Self {
        Self {
            inner,
            active: ACTIVE.with(|slot| slot.borrow().clone()),
            stopped: false,
        }
    }
}
impl Iterator for ResourceReadDir {
    type Item = io::Result<DirEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.stopped {
            return None;
        }
        if let Some(active) = &self.active {
            // Covers an entry plus its path/name and derived catalog records;
            // this reservation precedes the OS iterator's owned DirEntry.
            if let Err(error) = charge(active, 8192, 8, false) {
                self.stopped = true;
                return Some(Err(error));
            }
        }
        self.inner.next()
    }
}

/// Derived collection nodes are charged where the original loader creates
/// them, separately from the iterator's DirEntry/path/name allocation.
pub(crate) fn charge_owned_metadata(bytes: u64, records: usize) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    charge(&active, bytes, records, false)
}
/// Conservative structural upper bound, before the TOML/frontmatter parser can
/// allocate nodes. Long text remains a byte charge rather than one record/char.
pub(crate) fn charge_structured_text(text: &str) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let structural = text
        .bytes()
        .filter(|b| {
            matches!(
                b,
                b'=' | b':' | b',' | b'[' | b']' | b'{' | b'}' | b'.' | b'\n'
            )
        })
        .count();
    let nodes = structural
        .checked_mul(4)
        .and_then(|n| n.checked_add(16))
        .ok_or_else(|| exhausted(&mut active.lock().unwrap_or_else(|error| error.into_inner())))?;
    let bytes = u64::try_from(nodes)
        .ok()
        .and_then(|n| n.checked_mul(256))
        .ok_or_else(|| exhausted(&mut active.lock().unwrap_or_else(|error| error.into_inner())))?;
    charge(&active, bytes, nodes, false)
}

tokio::task_local! { static NORMAL_CAPTURE_DENIED: bool; }
pub(crate) fn check_reader_capability() -> io::Result<()> {
    if NORMAL_CAPTURE_DENIED
        .try_with(|denied| *denied)
        .unwrap_or(false)
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ordinary capture has no resource file-read capability",
        ))
    } else {
        Ok(())
    }
}
#[cfg(test)]
pub(crate) async fn deny_readers<F: std::future::Future>(future: F) -> F::Output {
    NORMAL_CAPTURE_DENIED.scope(true, future).await
}

/// Refresh-only presence observation. Optional readers can preserve omission
/// while retaining a failure that must not be represented as observed absence.
pub(crate) fn observe_metadata_source(path: &Path) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    charge(&active, path.as_os_str().len() as u64 + 512, 4, false)?;
    match std::fs::metadata(path) {
        Ok(metadata) => record_version(
            &active,
            path,
            ObservedResourceVersion::Present(format!(
                "metadata:{}:{}:{:?}",
                metadata.is_dir(),
                metadata.len(),
                metadata.modified().ok()
            )),
        ),
        Err(error) => observe_read_error(path, &error),
    }
}

//! Allocation gate for the existing synchronous context-resource preparation.
mod structured;
pub(crate) use structured::{
    admit_cloned_value, admit_json_payload, admit_serialized_value, cloned_value_material,
    json_payload_material,
};

use crate::runtime::{
    input_evidence_budget::{
        BudgetViewGuard, InputEvidenceBudgetLedger, InputMemoryCategory, OwnedBytesEstimate,
        OwnedCompletionBudget,
    },
    resident_input_evidence::InputEvidenceScope,
};
use std::{
    cell::RefCell,
    fs::{DirEntry, File, ReadDir},
    io::{self, Read},
    marker::PhantomData,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

thread_local! { static ACTIVE: RefCell<Option<Arc<Mutex<ReadMaterial>>>> = const { RefCell::new(None) }; }
struct ReadMaterial {
    bytes: u64,
    records: usize,
    scratch_bytes: u64,
    scratch_records: usize,
    byte_limit: u64,
    record_limit: usize,
    material: OwnedCompletionBudget,
    failed: bool,
    parent: Option<Arc<Mutex<ReadMaterial>>>,
    read_error: Option<io::ErrorKind>,
    #[cfg(test)]
    after_file_metadata: Option<Box<dyn FnOnce() + Send>>,
}
impl ReadMaterial {
    fn remaining_bytes(&self) -> u64 {
        self.byte_limit - self.bytes - self.scratch_bytes
    }
    fn remaining_records(&self) -> usize {
        self.record_limit - self.records - self.scratch_records
    }
}
/// Explicit allowance for synchronous acquisition and its owning projections.
/// Named callers share it with source adapters; this is not a publication cut.
/// The synchronous TLS entry is never held across an await.
#[derive(Clone)]
pub(crate) struct ResourceReadMaterial {
    active: Arc<Mutex<ReadMaterial>>,
}
impl std::fmt::Debug for ResourceReadMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceReadMaterial")
            .finish_non_exhaustive()
    }
}
pub(crate) struct ResourceReadBudget {
    previous: Option<Arc<Mutex<ReadMaterial>>>,
    _thread: PhantomData<Rc<()>>,
}
impl ResourceReadMaterial {
    /// Reserve an owning result's independent copy before cloning its fields.
    /// The copy neither retains its source acquisition nor inherits TLS.
    pub(crate) fn fork_owned(&self) -> Self {
        self.finish().expect("owning result acquisition failed");
        let source = self
            .active
            .lock()
            .expect("owning result acquisition poisoned");
        Self {
            active: Arc::new(Mutex::new(ReadMaterial {
                bytes: source.bytes,
                records: source.records,
                scratch_bytes: source.scratch_bytes,
                scratch_records: source.scratch_records,
                byte_limit: source.byte_limit,
                record_limit: source.record_limit,
                material: source.material.fork_owned(),
                failed: false,
                parent: None,
                read_error: None,
                #[cfg(test)]
                after_file_metadata: None,
            })),
        }
    }

    pub(crate) fn shares_allowance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.active, &other.active)
    }

    pub(crate) fn new(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
    ) -> anyhow::Result<Self> {
        Self::after(ledger, scope, (0, 0))
    }
    /// Reuse the caller's allowance, or admit a standalone synchronous read.
    pub(crate) fn current_or_new(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
    ) -> anyhow::Result<Self> {
        Self::current_or_new_for_identity(ledger, &scope.agent_id, scope.identity_incarnation)
    }
    pub(crate) fn current_or_new_for_identity(
        ledger: &InputEvidenceBudgetLedger,
        agent_id: &str,
        identity_incarnation: u64,
    ) -> anyhow::Result<Self> {
        if let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) {
            let material = Self { active };
            material.finish()?;
            Ok(material)
        } else {
            Self::after_identity(ledger, agent_id, identity_incarnation, (0, 0))
        }
    }
    /// A source key and its candidate share one resource-phase allowance.
    pub(crate) fn after(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
        prior: (u64, usize),
    ) -> anyhow::Result<Self> {
        Self::after_identity(ledger, &scope.agent_id, scope.identity_incarnation, prior)
    }
    fn after_identity(
        ledger: &InputEvidenceBudgetLedger,
        agent_id: &str,
        identity_incarnation: u64,
        prior: (u64, usize),
    ) -> anyhow::Result<Self> {
        let limits = ledger.limits();
        let bytes = limits
            .history_per_agent_bytes
            .checked_add(limits.pending_per_agent_bytes)
            .ok_or_else(resource_exhausted)?
            .min(64 * 1024 * 1024);
        let records = usize::try_from(limits.record_limit.min(65_536))?;
        Self::with_identity_limits(
            ledger,
            agent_id,
            identity_incarnation,
            bytes.checked_sub(prior.0).ok_or_else(resource_exhausted)?,
            records
                .checked_sub(prior.1)
                .ok_or_else(resource_exhausted)?,
        )
    }
    #[cfg(test)]
    fn with_limits(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
        bytes: u64,
        records: usize,
    ) -> anyhow::Result<Self> {
        Self::with_identity_limits(
            ledger,
            &scope.agent_id,
            scope.identity_incarnation,
            bytes,
            records,
        )
    }
    fn with_identity_limits(
        ledger: &InputEvidenceBudgetLedger,
        agent_id: &str,
        identity_incarnation: u64,
        bytes: u64,
        records: usize,
    ) -> anyhow::Result<Self> {
        // Gate the exact original header before ScopeKey clones or Arc/Mutex allocation.
        let header_bytes = u64::try_from(agent_id.len())?
            .checked_add(256)
            .and_then(|bytes| bytes.checked_mul(8))
            .ok_or_else(resource_exhausted)?;
        let header_records = 8 * 8;
        if header_bytes > bytes || header_records > records {
            return Err(resource_exhausted());
        }
        let material = ledger.reserve_owned_identity_completion(
            agent_id,
            identity_incarnation,
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: header_bytes,
            },
        )?;
        let result = Self {
            active: Arc::new(Mutex::new(ReadMaterial {
                bytes: header_bytes,
                records: header_records,
                scratch_bytes: 0,
                scratch_records: 0,
                byte_limit: bytes,
                record_limit: records,
                material,
                failed: false,
                parent: None,
                read_error: None,
                #[cfg(test)]
                after_file_metadata: None,
            })),
        };
        Ok(result)
    }
    pub(crate) fn enter(&self) -> ResourceReadBudget {
        let previous = ACTIVE.with(|slot| slot.replace(Some(self.active.clone())));
        ResourceReadBudget {
            previous,
            _thread: PhantomData,
        }
    }
    /// One synchronous acquisition, including nested source adapters. The
    /// caller retains this phase's guard with the returned owning material.
    pub(crate) fn read<T>(&self, read: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        self.finish()?;
        let _entry = self.enter();
        let value = read()?;
        self.finish()?;
        Ok(value)
    }
    /// Transaction ownership releases separately, without refunding its original allowance.
    pub(crate) fn transaction(&self) -> anyhow::Result<Self> {
        self.finish()?;
        let mut phase = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let bytes = phase.material.sibling_header_bytes()?;
        let records = 64;
        let byte_limit = phase.remaining_bytes();
        let record_limit = phase.remaining_records();
        reserve_allowance(&mut phase, bytes, records).map_err(|_| resource_exhausted())?;
        let material = phase.material.reserve_sibling(OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: bytes,
        })?;
        drop(phase);
        Ok(Self {
            active: Arc::new(Mutex::new(ReadMaterial {
                bytes,
                records,
                scratch_bytes: 0,
                scratch_records: 0,
                byte_limit,
                record_limit,
                material,
                failed: false,
                parent: Some(self.active.clone()),
                read_error: None,
                #[cfg(test)]
                after_file_metadata: None,
            })),
        })
    }
    /// Real Host ownership, sharing this phase's remaining allowance.
    pub(crate) fn host(ledger: &InputEvidenceBudgetLedger) -> anyhow::Result<Self> {
        let parent = ACTIVE
            .with(|slot| slot.borrow().clone())
            .ok_or_else(|| anyhow::anyhow!("Host resource material requires an active phase"))?;
        let mut phase = parent.lock().unwrap_or_else(|error| error.into_inner());
        let bytes = 2048;
        let records = 64;
        let byte_limit = phase.remaining_bytes();
        let record_limit = phase.remaining_records();
        reserve_allowance(&mut phase, bytes, records).map_err(|_| resource_exhausted())?;
        // Ledger registration admits its metadata against this same parent.
        drop(phase);
        let material = ledger.reserve_host_owned_completion(
            InputMemoryCategory::Candidate,
            OwnedBytesEstimate {
                payload_bytes: 0,
                metadata_estimate_bytes: bytes,
            },
        )?;
        Ok(Self {
            active: Arc::new(Mutex::new(ReadMaterial {
                bytes,
                records,
                scratch_bytes: 0,
                scratch_records: 0,
                byte_limit,
                record_limit,
                material,
                failed: false,
                parent: Some(parent),
                read_error: None,
                #[cfg(test)]
                after_file_metadata: None,
            })),
        })
    }
    #[cfg(test)]
    pub(crate) fn with_test_limits(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
        bytes: u64,
        records: usize,
    ) -> anyhow::Result<Self> {
        Self::with_limits(ledger, scope, bytes, records)
    }
    #[cfg(test)]
    pub(crate) fn after_file_metadata(&self, action: impl FnOnce() + Send + 'static) {
        self.active.lock().unwrap().after_file_metadata = Some(Box::new(action));
    }
    pub(crate) fn finish(&self) -> anyhow::Result<(u64, usize)> {
        let material = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if material.failed {
            return Err(resource_exhausted());
        }
        if let Some(kind) = material.read_error {
            return Err(io::Error::new(kind, "context resource reader failed").into());
        }
        Ok((
            material.bytes + material.scratch_bytes,
            material.records + material.scratch_records,
        ))
    }
    pub(crate) fn guard(&self) -> anyhow::Result<Arc<BudgetViewGuard>> {
        self.finish()?;
        Ok(self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .material
            .guard())
    }
    pub(crate) fn matches_scope(&self, scope: &InputEvidenceScope) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .material
            .matches_scope(scope)
    }
    pub(crate) fn matches_identity(&self, agent_id: &str, incarnation: u64) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .material
            .matches_identity(agent_id, incarnation)
    }
    pub(crate) fn matches_owner_identity(
        &self,
        ledger: &InputEvidenceBudgetLedger,
        agent_id: &str,
        incarnation: u64,
    ) -> bool {
        let phase = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        phase.material.matches_ledger(ledger)
            && phase.material.matches_identity(agent_id, incarnation)
    }
    /// Keep the existing phase allocation with a synchronous consumer's value.
    pub(crate) fn active_guard() -> anyhow::Result<Option<Arc<BudgetViewGuard>>> {
        ACTIVE
            .with(|slot| slot.borrow().clone())
            .map(|active| Self { active }.guard())
            .transpose()
    }
    /// Reuse an existing observation only for its actual agent and ledger.
    /// Host/foreign phases still meter the caller, but are not its owned view.
    pub(crate) fn active_owned_guard(
        ledger: &InputEvidenceBudgetLedger,
        scope: &InputEvidenceScope,
    ) -> anyhow::Result<Option<Arc<BudgetViewGuard>>> {
        let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
            return Ok(None);
        };
        let matches = {
            let phase = active.lock().unwrap_or_else(|error| error.into_inner());
            phase.material.matches_ledger(ledger) && phase.material.matches_scope(scope)
        };
        if matches {
            Ok(Some(Self { active }.guard()?))
        } else {
            Ok(None)
        }
    }
    /// Seal actual retained totals (coverage floors, not raw eightfold charges).
    /// Allocation gates must still admit every owned allocation beforehand.
    /// Extend only the original Candidate view; repeated/lower seals are free.
    pub(crate) fn seal_retained(
        &self,
        retained_bytes: u64,
        retained_records: usize,
    ) -> anyhow::Result<Arc<BudgetViewGuard>> {
        let mut material = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if material.failed {
            return Err(resource_exhausted());
        }
        let added_bytes = retained_bytes.saturating_sub(material.bytes);
        let added_records = retained_records.saturating_sub(material.records);
        let next = material.bytes.checked_add(added_bytes);
        let count = material.records.checked_add(added_records);
        let (Some(next), Some(count)) = (next, count) else {
            material.failed = true;
            return Err(resource_exhausted());
        };
        if added_bytes > material.remaining_bytes() || added_records > material.remaining_records()
        {
            material.failed = true;
            return Err(resource_exhausted());
        }
        // All fallible checks precede changes to either dimension or the ledger.
        material.material.observe_material(0, added_bytes);
        material.bytes = next;
        material.records = count;
        Ok(material.material.guard())
    }
}
impl Drop for ResourceReadBudget {
    fn drop(&mut self) {
        ACTIVE.with(|slot| slot.replace(self.previous.take()));
    }
}
fn resource_exhausted() -> anyhow::Error {
    crate::runtime_error::RuntimeError::new(
        crate::runtime_error::RuntimeErrorDomain::Io,
        "context_resource_exhausted",
        "ResourceExhausted: context resource read material",
    )
    .with_retryable(true)
    .into()
}
pub(crate) fn check_active() -> anyhow::Result<()> {
    let active = ACTIVE.with(|slot| slot.borrow().clone());
    if let Some(active) = active {
        let material = active.lock().unwrap_or_else(|error| error.into_inner());
        if material.failed {
            return Err(resource_exhausted());
        }
        if let Some(kind) = material.read_error {
            return Err(io::Error::new(kind, "context resource reader failed").into());
        }
    }
    Ok(())
}
/// Check before allocating optional error context; preserve ordinary I/O diagnostics.
pub(crate) fn check_active_budget() -> anyhow::Result<()> {
    if let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) {
        if active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .failed
        {
            return Err(resource_exhausted());
        }
    }
    Ok(())
}
pub(crate) fn observe_read_error<T>(result: &io::Result<T>) {
    if let Err(error) = result {
        // Optional catalogs own their malformed-text policy; strict readers
        // still propagate this error directly. Real I/O failures stay latched.
        let decode_error = error.kind() == io::ErrorKind::InvalidData
            && error
                .get_ref()
                .is_some_and(|cause| cause.is::<std::string::FromUtf8Error>());
        if error.kind() == io::ErrorKind::NotFound || decode_error {
            return;
        }
        if let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) {
            let mut material = active.lock().unwrap_or_else(|error| error.into_inner());
            material.read_error = Some(error.kind());
            if let Some(parent) = &material.parent {
                parent
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .read_error = Some(error.kind());
            }
        }
    }
}
fn latch_failure(material: &mut ReadMaterial) {
    material.failed = true;
    if let Some(parent) = &material.parent {
        latch_failure(&mut parent.lock().unwrap_or_else(|error| error.into_inner()));
    }
}
fn exhausted(material: &mut ReadMaterial) -> io::Error {
    latch_failure(material);
    // io::Error forwards its inner error's source, not the inner error itself.
    // Keep the typed rejection visible when a reader returns before finish().
    io::Error::other(
        resource_exhausted().context("ResourceExhausted: context resource read material"),
    )
}
fn reserve_allowance(material: &mut ReadMaterial, bytes: u64, records: usize) -> io::Result<()> {
    if material.failed
        || bytes > material.remaining_bytes()
        || records > material.remaining_records()
    {
        return Err(exhausted(material));
    }
    if let Some(parent) = &material.parent {
        let result = reserve_allowance(
            &mut parent.lock().unwrap_or_else(|error| error.into_inner()),
            bytes,
            records,
        );
        if let Err(error) = result {
            material.failed = true;
            return Err(error);
        }
    }
    material.bytes += bytes;
    material.records += records;
    Ok(())
}
fn charge(
    active: &Arc<Mutex<ReadMaterial>>,
    bytes: u64,
    records: usize,
    payload: bool,
) -> io::Result<()> {
    let mut material = active.lock().unwrap_or_else(|error| error.into_inner());
    if material.failed {
        return Err(exhausted(&mut material));
    }
    let added = bytes
        .checked_mul(8)
        .ok_or_else(|| exhausted(&mut material))?;
    let count = records
        .checked_mul(8)
        .ok_or_else(|| exhausted(&mut material))?;
    reserve_allowance(&mut material, added, count)?;
    material.material.observe_material(
        if payload { added } else { 0 },
        if payload { 0 } else { added },
    );
    Ok(())
}
pub(crate) fn read_to_string(path: &Path) -> Option<io::Result<String>> {
    let active = ACTIVE.with(|slot| slot.borrow().clone())?;
    Some((|| {
        charge(&active, 0, 0, false)?;
        let mut file = File::open(path)?;
        admit_file_bytes(file.metadata()?.len())?;
        #[cfg(test)]
        run_after_file_metadata();
        let mut bytes = Vec::new();
        read_to_end_active(&active, &mut file, &mut bytes)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })())
}
#[cfg(test)]
pub(crate) fn run_after_file_metadata() {
    let action = ACTIVE.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|active| active.lock().unwrap().after_file_metadata.take())
    });
    if let Some(action) = action {
        action();
    }
}
/// Admit fixed reader/iterator state and pathname material before opening it.
pub(crate) fn admit_path(path: &Path) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let bytes = (path.as_os_str().len() as u64)
        .checked_mul(4)
        .and_then(|n| n.checked_add(512))
        .ok_or_else(|| exhausted(&mut active.lock().unwrap_or_else(|error| error.into_inner())))?;
    charge(&active, bytes, 1, false)
}

/// Metadata preflight only; actual body chunks are charged before buffer growth.
pub(crate) fn admit_file_bytes(declared: u64) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let mut material = active.lock().unwrap_or_else(|error| error.into_inner());
    let required = declared.checked_add(512);
    if material.failed || required.is_none_or(|bytes| bytes > material.remaining_bytes() / 8) {
        return Err(exhausted(&mut material));
    }
    Ok(())
}

pub(crate) fn read_to_end(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> Option<io::Result<usize>> {
    let active = ACTIVE.with(|slot| slot.borrow().clone())?;
    Some(read_to_end_active(&active, reader, buffer))
}
fn read_to_end_active(
    active: &Arc<Mutex<ReadMaterial>>,
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
) -> io::Result<usize> {
    // Even an empty reader consumes state; a latched failure must not call Read.
    charge(active, 512, 1, false)?;
    let before = buffer.len();
    let mut chunk = [0u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            break;
        }
        charge(active, count as u64, 0, true)?;
        buffer.extend_from_slice(&chunk[..count]);
    }
    Ok(buffer.len() - before)
}
pub(crate) struct ResourceReadDir {
    inner: ReadDir,
    active: Option<Arc<Mutex<ReadMaterial>>>,
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
            if let Err(error) = charge(active, 8192, 8, false) {
                self.stopped = true;
                return Some(Err(error));
            }
        }
        self.inner.next()
    }
}
pub(crate) fn charge_owned_payload(bytes: u64) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    charge(&active, bytes, 0, true)
}
/// A fixed final byte buffer has no decoded/resolver expansion. Its owner and
/// container metadata still require their separate conservative admission.
pub(crate) fn charge_owned_buffer(bytes: u64) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let mut material = active.lock().unwrap_or_else(|error| error.into_inner());
    reserve_allowance(&mut material, bytes, 0)?;
    material.material.observe_material(bytes, 0);
    Ok(())
}
pub(crate) fn charge_owned_metadata(bytes: u64, records: usize) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    charge(&active, bytes, records, false)
}
/// Ledger bookkeeping is material too. Admit it before locking the ledger,
/// then let the new view retain the original phase through its last owner.
pub(crate) fn owned_registration(
    identity_bytes: usize,
) -> anyhow::Result<Option<Arc<BudgetViewGuard>>> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(None);
    };
    let bytes = u64::try_from(identity_bytes)?
        .checked_add(256)
        .ok_or_else(resource_exhausted)?;
    charge(&active, bytes, 8, false).map_err(|_| resource_exhausted())?;
    Ok(Some(ResourceReadMaterial { active }.guard()?))
}
/// Check a whole side-effecting kernel against this phase's used remainder.
/// Its constituent allocations still use the normal charging gates.
pub(crate) fn admit_owned_metadata(bytes: u64, records: usize) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let mut material = active.lock().unwrap_or_else(|error| error.into_inner());
    if material.failed
        || bytes
            .checked_mul(8)
            .is_none_or(|n| n > material.remaining_bytes())
        || records
            .checked_mul(8)
            .is_none_or(|n| n > material.remaining_records())
    {
        return Err(exhausted(&mut material));
    }
    Ok(())
}

pub(crate) fn charge_skill_catalog_entry(
    entry: &crate::types::SkillCatalogEntry,
) -> io::Result<()> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(());
    };
    let bytes = [
        entry.skill_id.len(),
        entry.root_id.len(),
        entry.skill_dir.len(),
        entry.name.len(),
        entry.description.len(),
        entry.path.as_os_str().len(),
        std::mem::size_of::<crate::types::SkillCatalogEntry>(),
    ]
    .into_iter()
    .try_fold(0usize, |total, length| total.checked_add(length))
    .ok_or_else(|| exhausted(&mut active.lock().unwrap_or_else(|error| error.into_inner())))?;
    charge(
        &active,
        u64::try_from(bytes).map_err(io::Error::other)?,
        16,
        false,
    )
}

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

/// Template-only scratch. Returned values and errors are admitted separately
/// into the original material before parsing, so neither borrows this lease.
pub(crate) struct TemplateParseBudget {
    active: Arc<Mutex<ReadMaterial>>,
    bytes: u64,
    records: usize,
    material: Option<OwnedCompletionBudget>,
}
impl Drop for TemplateParseBudget {
    fn drop(&mut self) {
        let mut phase = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Never subtract from the original Candidate or clear a failure latch.
        drop(self.material.take());
        phase.scratch_bytes -= self.bytes;
        phase.scratch_records -= self.records;
        if let Some(parent) = &phase.parent {
            let mut parent = parent.lock().unwrap_or_else(|error| error.into_inner());
            parent.scratch_bytes -= self.bytes;
            parent.scratch_records -= self.records;
        }
    }
}

/// Allocation-free upper bound, not a second TOML lexer. Every stock token
/// starts at one of these bytes or at an atom run. Quotes split extra runs:
/// the stock atom lexer intentionally does not treat them as delimiters.
fn template_token_bound(text: &str) -> Option<usize> {
    let mut tokens = 1usize; // EOF
    let mut previous_delimiter = true;
    for byte in text.bytes() {
        let delimiter = b".=,[]{} \t#\r\n'\"".contains(&byte);
        tokens = tokens.checked_add(usize::from(delimiter || previous_delimiter))?;
        previous_delimiter = delimiter;
    }
    Some(tokens)
}

pub(crate) fn template_parse(text: &str) -> io::Result<Option<TemplateParseBudget>> {
    let Some(active) = ACTIVE.with(|slot| slot.borrow().clone()) else {
        return Ok(None);
    };
    let overflow = || exhausted(&mut active.lock().unwrap_or_else(|error| error.into_inner()));
    let n = u64::try_from(text.len()).map_err(|_| overflow())?;
    // Success values, input-carrying errors and conversion/name copies stay
    // charged through the original last holder, including optional-reader errors.
    let retained_bytes = n
        .checked_mul(16)
        .and_then(|bytes| bytes.checked_add(512))
        .ok_or_else(overflow)?;
    // Only TemplateManifest / TemplateSkillsManifest use this gate. Metadata
    // has five strings; each skill requires at least kind= and one value=.
    // Eight slots per '=' cover the two vectors, fields and normalized names
    // of each skill. Unknown fields inside an internally tagged skill are
    // buffered by Serde Content, whose errors can retain deep dotted key paths.
    // Count dots before parsing too; the fixed portion covers bounded array
    // nesting and error/context wrappers, not an assumed two-field error path.
    // Lexer capacity and arbitrary ignored DOM nodes must not be retained.
    let retained_records = text
        .bytes()
        .filter(|byte| matches!(*byte, b'=' | b'.'))
        .count()
        .checked_add(16)
        .ok_or_else(overflow)?;
    charge(&active, retained_bytes, retained_records, false)?;

    // Unlike punctuation counting, length covers lexer capacity even for a
    // single comment/string, EOF growth, recovery events, Serde Content and
    // simultaneous conversion buffers. These are owned-budget estimates, not
    // an allocator/RSS measurement.
    let units = n.checked_add(1).ok_or_else(overflow)?;
    let bytes = units.checked_mul(8192).ok_or_else(overflow)?;
    // The locked lexer reserves n tokens, even for one comment. Include both
    // old/new allocations when EOF triggers growth (n + 2n). Events start at
    // token count, not input length; four events/token and old/new Vec growth
    // need at most 12 slots/token. The preflight deliberately overcounts tokens.
    // Recovery can synthesize separators (for example, missing array commas),
    // so DOM/key paths and Serde collections must also use the token bound.
    let tokens = template_token_bound(text).ok_or_else(overflow)?;
    let collections = tokens
        .checked_add(1)
        .and_then(|n| n.checked_mul(32))
        .ok_or_else(overflow)?;
    let records = text
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| {
            tokens
                .checked_mul(12)
                .and_then(|events| n.checked_add(events))
        })
        .and_then(|n| n.checked_add(collections))
        .and_then(|n| n.checked_add(64))
        .ok_or_else(overflow)?;
    reserve_template_scratch(active, bytes, records).map(Some)
}

fn reserve_template_scratch(
    active: Arc<Mutex<ReadMaterial>>,
    bytes: u64,
    records: usize,
) -> io::Result<TemplateParseBudget> {
    let mut phase = active.lock().unwrap_or_else(|error| error.into_inner());
    let header = phase
        .material
        .sibling_header_bytes()
        .map_err(|_| exhausted(&mut phase))?;
    let bytes = bytes
        .checked_add(header)
        .ok_or_else(|| exhausted(&mut phase))?;
    if phase.failed || bytes > phase.remaining_bytes() || records > phase.remaining_records() {
        return Err(exhausted(&mut phase));
    }
    let parent_phase = phase.parent.clone();
    let mut parent = parent_phase
        .as_ref()
        .map(|parent| parent.lock().unwrap_or_else(|error| error.into_inner()));
    if parent.as_ref().is_some_and(|parent| {
        parent.failed || bytes > parent.remaining_bytes() || records > parent.remaining_records()
    }) {
        drop(parent);
        return Err(exhausted(&mut phase));
    }
    let material = phase.material.reserve_sibling(OwnedBytesEstimate {
        payload_bytes: 0,
        metadata_estimate_bytes: bytes,
    });
    let material = match material {
        Ok(material) => material,
        Err(_) => {
            drop(parent);
            return Err(exhausted(&mut phase));
        }
    };
    phase.scratch_bytes += bytes;
    phase.scratch_records += records;
    if let Some(parent) = parent.as_mut() {
        parent.scratch_bytes += bytes;
        parent.scratch_records += records;
    }
    drop(parent);
    drop(phase);
    Ok(TemplateParseBudget {
        active,
        bytes,
        records,
        material: Some(material),
    })
}

#[cfg(test)]
pub(crate) fn assert_retryable_resource_error(error: &anyhow::Error) {
    use crate::runtime_error::{describe_runtime_error, RuntimeError, RuntimeErrorDomain};

    let typed = error
        .chain()
        .find_map(|source| source.downcast_ref::<RuntimeError>())
        .unwrap_or_else(|| panic!("resource rejection lost its typed source: {error:#}"));
    let original = typed.descriptor();
    assert_eq!(original.domain, RuntimeErrorDomain::Io);
    assert_eq!(original.code, "context_resource_exhausted");
    assert!(original.retryable);
    let descriptor = describe_runtime_error(error);
    assert_eq!(descriptor.domain, original.domain);
    assert_eq!(descriptor.code, original.code);
    assert_eq!(descriptor.retryable, original.retryable);
    assert!(descriptor
        .source_chain
        .iter()
        .any(|source| source == &original.operator_message));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_isolated_plan_probe(test_name: &str) -> bool {
        const CHILD_TEST: &str = "HOLON_RESOURCE_PLAN_PROBE_CHILD";
        if std::env::var(CHILD_TEST).as_deref() == Ok(test_name) {
            return false;
        }
        // IoProbe counts the whole process, including other tests' file reads.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("diagnostics::resource_io::tests::{test_name}"),
                "--nocapture",
            ])
            .env(CHILD_TEST, test_name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated plan probe failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    fn fixture() -> (InputEvidenceBudgetLedger, InputEvidenceScope) {
        (
            InputEvidenceBudgetLedger::new(Default::default()).unwrap(),
            InputEvidenceScope {
                agent_id: "resource-read".into(),
                identity_incarnation: 1,
            },
        )
    }
    #[test]
    fn fixed_buffer_admission_is_exact_cumulative_and_keeps_original_latch() -> anyhow::Result<()> {
        let (ledger, scope) = fixture();
        let header = ResourceReadMaterial::new(&ledger, &scope)?.finish()?;
        // Two exact buffers; generic payload and owner metadata keep 8x.
        let bytes = header.0 + 2 * 1024 + 8 + 17 * 8;
        let records = header.1 + 2 * 8;
        for (short_bytes, short_records) in [(0, 0), (1, 0), (0, 1)] {
            let phase = ResourceReadMaterial::with_limits(
                &ledger,
                &scope,
                bytes - short_bytes,
                records - short_records,
            )?;
            let result = phase.read(|| {
                charge_owned_buffer(1024)?;
                charge_owned_payload(1)?;
                charge_owned_buffer(1024)?;
                charge_owned_metadata(17, 2)?;
                Ok(())
            });
            if short_bytes != 0 || short_records != 0 {
                assert_retryable_resource_error(&result.unwrap_err());
                let _entry = phase.enter();
                assert_retryable_resource_error(&charge_owned_buffer(0).unwrap_err().into());
                assert_retryable_resource_error(&check_active().unwrap_err());
                assert_retryable_resource_error(&phase.finish().unwrap_err());
                continue;
            }
            result?;
            assert_eq!(phase.finish()?, (bytes, records));
            let last = phase.guard()?;
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, bytes);
            drop(last);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn immediate_material_rejection_keeps_typed_source_and_parent_latch() -> anyhow::Result<()> {
        let rejectors: [fn() -> io::Result<()>; 3] = [
            || charge_owned_buffer(u64::MAX),
            || charge_owned_payload(u64::MAX),
            || charge_owned_metadata(0, usize::MAX),
        ];
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for reject in rejectors {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 2 * header + 64, 136)?;
            let transaction = phase.transaction()?;
            {
                let _entry = transaction.enter();
                charge_owned_metadata(8, 1)?;
                assert_eq!(phase.finish()?, (2 * header + 64, 136));
                assert_eq!(transaction.finish()?, (header + 64, 72));
                let raw = reject().unwrap_err();
                io_source_chain_keeps_typed_rejection(&raw);
                let error =
                    anyhow::Error::from(raw).context("source reader returned before phase finish");
                assert_retryable_resource_error(&error);
                assert_retryable_resource_error(&check_active().unwrap_err());
                assert_retryable_resource_error(&charge_owned_buffer(0).unwrap_err().into());
            }
            assert_retryable_resource_error(&transaction.finish().unwrap_err());
            assert_retryable_resource_error(&phase.finish().unwrap_err());
            let mut called = false;
            let error = phase
                .read(|| {
                    called = true;
                    Ok(())
                })
                .unwrap_err();
            assert!(!called);
            assert_retryable_resource_error(&error);
            drop((transaction, phase));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }
    #[test]
    fn synchronous_acquisition_restores_entry_and_retains_original_failure() -> anyhow::Result<()> {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, header + 8, 72)?;
        let held = phase.read(|| {
            charge_owned_metadata(1, 1)?;
            phase.guard()
        })?;
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let failure = phase.read::<()>(|| {
            assert!(charge_owned_metadata(1, 1).is_err());
            anyhow::bail!("original source failure")
        });
        assert_eq!(failure.unwrap_err().to_string(), "original source failure");
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let mut called = false;
        assert!(phase
            .read(|| {
                called = true;
                Ok(())
            })
            .is_err());
        assert!(!called);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, header + 8);
        drop(held);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }

    #[test]
    fn transaction_material_releases_ownership_without_refunding_original_allowance(
    ) -> anyhow::Result<()> {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 3 * header + 17 * 8, 208)?;
        let transaction = phase.transaction()?;
        {
            let _entry = transaction.enter();
            charge_owned_metadata(17, 2)?;
        }
        assert_eq!(phase.finish()?, (2 * header + 17 * 8, 144));
        let held = transaction.guard()?;
        drop(transaction);
        assert_eq!(ledger.report()?.observed_bytes, 2 * header + 17 * 8);
        drop(held);
        assert_eq!(ledger.report()?.observed_bytes, header);
        drop(phase.transaction()?);
        assert_eq!(phase.finish()?, (3 * header + 17 * 8, 208));
        assert_eq!(ledger.report()?.observed_bytes, header);
        assert!(phase.transaction().is_err());
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, header);
        drop(phase);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    }
    #[test]
    fn transaction_material_header_is_admitted_before_registering_its_view() -> anyhow::Result<()> {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (bytes, records, fits) in [
            (2 * header - 1, 128, false),
            (2 * header, 127, false),
            (2 * header, 128, true),
        ] {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, records)?;
            let transaction = phase.transaction();
            assert_eq!(transaction.is_ok(), fits);
            if fits {
                assert_eq!(phase.finish()?, (2 * header, 128));
                assert_eq!(ledger.report()?.observed_bytes, 2 * header);
            } else {
                assert!(phase.finish().is_err());
                assert!(phase.transaction().is_err());
                assert_eq!(ledger.report()?.observed_bytes, header);
            }
            drop((transaction, phase));
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        Ok(())
    }
    #[test]
    fn transaction_material_charges_and_latches_every_original_ancestor() -> anyhow::Result<()> {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (bytes, records, rejected_bytes, rejected_records) in
            [(3 * header + 29 * 8, 512, 1, 0), (8 * header, 208, 0, 1)]
        {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, records)?;
            let transaction = phase.transaction()?;
            let nested = transaction.transaction()?;
            {
                let _entry = nested.enter();
                charge_owned_metadata(29, 2)?;
            }
            assert_eq!(phase.finish()?, (3 * header + 29 * 8, 208));
            assert_eq!(transaction.finish()?, (2 * header + 29 * 8, 144));
            assert_eq!(nested.finish()?, (header + 29 * 8, 80));
            let held = nested.guard()?;
            let before = ledger.report()?.observed_bytes;
            {
                let _entry = nested.enter();
                assert!(charge_owned_metadata(rejected_bytes, rejected_records).is_err());
            }
            assert!(nested.finish().is_err());
            assert!(transaction.finish().is_err());
            assert!(phase.finish().is_err());
            assert!(phase.transaction().is_err());
            assert_eq!(ledger.report()?.observed_bytes, before);
            drop((nested, transaction, phase));
            assert_eq!(ledger.report()?.observed_bytes, header + 29 * 8);
            drop(held);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        Ok(())
    }
    #[test]
    fn template_token_bound_counts_delimiters_and_atom_runs_without_allocating() {
        for (text, expected) in [
            ("", 1),
            (".=,[]{}", 8),
            ("\"a\"b", 5),
            ("a\r\nb", 5),
            ("key='hello'", 6),
            ("汉字", 2),
        ] {
            assert_eq!(template_token_bound(text), Some(expected), "{text:?}");
        }
        let atom = "a".repeat(4096);
        assert_eq!(template_token_bound(&atom), Some(2));
        assert_eq!(template_token_bound(&format!("'{atom}'")), Some(4));
        assert_eq!(template_token_bound(&format!("#{atom}")), Some(3));
    }
    #[test]
    fn template_scratch_exact_limits_and_one_less_are_checked_before_registration() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let scratch = header + 1024;
        for (bytes, records, accepted) in [
            (header + scratch, 64 + 8, true),
            (header + scratch - 1, 64 + 8, false),
            (header + scratch, 64 + 8 - 1, false),
        ] {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, records).unwrap();
            let lease = reserve_template_scratch(phase.active.clone(), 1024, 8);
            assert_eq!(lease.is_ok(), accepted);
            if accepted {
                assert_eq!(phase.finish().unwrap(), (header + scratch, 72));
                assert_eq!(ledger.report().unwrap().observed_bytes, header + scratch);
                drop(lease);
                assert_eq!(phase.finish().unwrap(), (header, 64));
            } else {
                assert!(phase.finish().is_err());
                assert_eq!(ledger.report().unwrap().observed_bytes, header);
            }
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
    }
    #[test]
    fn template_scratch_keeps_original_material_alive_but_cannot_clear_failure() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let phase =
            ResourceReadMaterial::with_limits(&ledger, &scope, header * 2 + 1024, 72).unwrap();
        let retained = phase.guard().unwrap();
        let lease = reserve_template_scratch(phase.active.clone(), 1024, 8).unwrap();
        assert!(reserve_template_scratch(phase.active.clone(), 0, 0).is_err());
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, header * 2 + 1024);
        assert!(lease.active.lock().unwrap().failed);
        drop(lease);
        assert_eq!(ledger.report().unwrap().observed_bytes, header);
        drop(retained);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn host_registration_reenters_parent_without_lock_and_preserves_exact_limits() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (bytes, records) in [(header + 4096 - 1, 192), (header + 4096, 191)] {
            let parent =
                ResourceReadMaterial::with_limits(&ledger, &scope, bytes, records).unwrap();
            assert!(parent.read(|| ResourceReadMaterial::host(&ledger)).is_err());
            assert!(parent.finish().is_err());
            assert_eq!(
                ledger.report().unwrap().host_owned,
                OwnedBytesEstimate::default()
            );
            drop(parent);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
        let parent =
            ResourceReadMaterial::with_limits(&ledger, &scope, header + 4096, 192).unwrap();
        let host = parent.read(|| ResourceReadMaterial::host(&ledger)).unwrap();
        assert_eq!(parent.finish().unwrap(), (header + 4096, 192));
        assert_eq!(host.finish().unwrap(), (2048, 64));
        let last = host.guard().unwrap();
        assert!(host
            .read(|| {
                charge_owned_metadata(1, 0)?;
                Ok(())
            })
            .is_err());
        assert!(parent.finish().is_err());
        drop((host, parent));
        assert_eq!(ledger.report().unwrap().observed_bytes, header + 4096);
        drop(last);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn template_host_scratch_shares_parent_and_preserves_exact_host_ownership() {
        let (ledger, scope) = fixture();
        let parent = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let before = parent.finish().unwrap();
        let _io = parent.enter();
        let host = ResourceReadMaterial::host(&ledger).unwrap();
        let baseline = parent.finish().unwrap();
        let host_baseline = ledger.report().unwrap().host_owned;
        let lease = reserve_template_scratch(host.active.clone(), 4096, 17).unwrap();
        let live = parent.finish().unwrap();
        assert_eq!(live.0, baseline.0 + 4096 + 2048);
        assert_eq!(live.1, baseline.1 + 17);
        let report = ledger.report().unwrap();
        assert_eq!(
            report.host_owned.metadata_estimate_bytes,
            host_baseline.metadata_estimate_bytes + 4096 + 2048
        );
        drop(lease);
        assert_eq!(parent.finish().unwrap(), baseline);
        assert_eq!(ledger.report().unwrap().host_owned, host_baseline);
        // Host's header and ledger registration outlive its scratch.
        assert_eq!(baseline, (before.0 + 4096, before.1 + 128));
    }
    #[test]
    fn template_parent_rejection_latches_both_phases_without_a_scratch_view() {
        let (ledger, scope) = fixture();
        let parent = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let _io = parent.enter();
        let host = ResourceReadMaterial::host(&ledger).unwrap();
        // Another real owner consumes parent allowance after the Host was made.
        let remaining = parent.active.lock().unwrap().remaining_records();
        charge(&parent.active, 0, remaining / 8, false).unwrap();
        let before = ledger.report().unwrap().observed_bytes;
        assert!(reserve_template_scratch(host.active.clone(), 0, 1).is_err());
        assert!(parent.finish().is_err());
        assert!(host.finish().is_err());
        assert_eq!(ledger.report().unwrap().observed_bytes, before);
    }
    #[test]
    fn template_parse_releases_only_scratch_and_is_not_punctuation_dependent() {
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let _io = phase.enter();
        let original = phase.finish().unwrap();
        let mut previous = original;
        let mut previous_scratch = None;
        for text in ["summary='aaaaaaaa'", "summary='........'"] {
            let lease = template_parse(text).unwrap().unwrap();
            let live = phase.finish().unwrap();
            let size = (lease.bytes, lease.records);
            assert!(size.1 >= (text.len() + 1) * 10);
            if let Some((bytes, records)) = previous_scratch {
                assert_eq!(size.0, bytes);
                assert!(size.1 > records);
            }
            previous_scratch = Some(size);
            drop(lease);
            let retained = phase.finish().unwrap();
            assert!(retained.0 > previous.0);
            assert!(retained.1 > previous.1);
            assert_eq!((live.0 - retained.0, live.1 - retained.1), size);
            assert_eq!(ledger.report().unwrap().observed_bytes, retained.0);
            previous = retained;
        }
        let guard = phase.guard().unwrap();
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, previous.0);
        drop(_io);
        drop(guard);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn template_parse_long_comment_and_overflow_fail_closed() {
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let _io = phase.enter();
        assert!(template_parse(&format!("#{}", "a".repeat(100_000))).is_err());
        assert!(check_active().is_err());
        assert!(template_parse("").is_err());
        assert_eq!(phase.active.lock().unwrap().scratch_bytes, 0);
        drop(_io);
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        assert!(reserve_template_scratch(phase.active.clone(), u64::MAX, 0).is_err());
        assert!(phase.finish().is_err());
    }
    #[test]
    fn template_retained_records_follow_typed_values_not_lexer_capacity() {
        let (ledger, scope) = fixture();
        let mut previous_scratch = 0;
        for value in ["a".to_owned(), "a".repeat(1024)] {
            let text = format!("summary='{value}'");
            let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
            let _io = phase.enter();
            let before = phase.finish().unwrap();
            let lease = template_parse(&text).unwrap().unwrap();
            assert!(lease.records > previous_scratch);
            previous_scratch = lease.records;
            drop(lease);
            let retained = phase.finish().unwrap();
            assert_eq!(retained.1 - before.1, 128 + 8);
            assert_eq!(retained.0 - before.0, (text.len() as u64 * 16 + 512) * 8);
        }
    }
    #[test]
    fn template_parse_admits_length_and_structure_at_exact_remaining_limits() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (text, tokens, assignments) in [
            ("# comment".to_owned(), 4, 0),
            // Only three punctuation bytes, but recovery adds 128 DOM values.
            (format!("a=[{}]", "1 ".repeat(128)), 261, 1),
        ] {
            assert_eq!(template_token_bound(&text), Some(tokens));
            let retained_bytes = (text.len() as u64 * 16 + 512) * 8;
            let retained_records = (16 + assignments) * 8;
            let scratch_bytes = (text.len() as u64 + 1) * 8192 + header;
            let scratch_records = (text.len() + 1) * 3 + tokens * 12 + (tokens + 1) * 32 + 64;
            let bytes = header + retained_bytes + scratch_bytes;
            let records = 64 + retained_records + scratch_records;
            for (byte_limit, record_limit, accepted) in [
                (bytes, records, true),
                (bytes - 1, records, false),
                (bytes, records - 1, false),
            ] {
                let phase =
                    ResourceReadMaterial::with_limits(&ledger, &scope, byte_limit, record_limit)
                        .unwrap();
                let _io = phase.enter();
                let lease = template_parse(&text);
                assert_eq!(lease.is_ok(), accepted);
                if accepted {
                    assert_eq!(phase.finish().unwrap(), (bytes, records));
                    drop(lease);
                    assert_eq!(
                        phase.finish().unwrap(),
                        (header + retained_bytes, 64 + retained_records)
                    );
                } else {
                    assert!(phase.finish().is_err());
                    assert_eq!(phase.active.lock().unwrap().scratch_records, 0);
                }
            }
        }
    }
    #[test]
    fn retained_seal_adds_only_missing_totals_to_the_same_guard() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, header + 32, 80).unwrap();
        let original = phase.guard().unwrap();
        for (requested, expected) in [
            ((header + 7, 64), (header + 7, 64)),
            ((header + 7, 67), (header + 7, 67)),
            ((header + 3, 65), (header + 7, 67)),
            ((header + 32, 80), (header + 32, 80)),
        ] {
            let sealed = phase.seal_retained(requested.0, requested.1).unwrap();
            assert!(Arc::ptr_eq(&original, &sealed));
            assert_eq!(phase.finish().unwrap(), expected);
            assert_eq!(ledger.report().unwrap().observed_bytes, expected.0);
            phase.seal_retained(requested.0, requested.1).unwrap();
            phase.seal_retained(0, 0).unwrap();
            assert_eq!(phase.finish().unwrap(), expected);
            assert_eq!(ledger.report().unwrap().observed_bytes, expected.0);
        }
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, header + 32);
        drop(original);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn retained_seal_limit_failure_is_atomic_and_permanently_latched() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for requested in [
            (header + 33, 65),
            (header + 1, 81),
            (u64::MAX, 65),
            (header + 1, usize::MAX),
        ] {
            let phase =
                ResourceReadMaterial::with_limits(&ledger, &scope, header + 32, 80).unwrap();
            let original = phase.guard().unwrap();
            let error = phase.seal_retained(requested.0, requested.1).unwrap_err();
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&error).code,
                "context_resource_exhausted"
            );
            assert!(phase.finish().is_err());
            assert!(phase.seal_retained(header, 64).is_err());
            let material = phase.active.lock().unwrap();
            assert_eq!((material.bytes, material.records), (header, 64));
            drop(material);
            assert_eq!(ledger.report().unwrap().observed_bytes, header);
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, header);
            drop(original);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
    }
    #[test]
    fn retained_seal_cannot_revive_an_arithmetic_overflowed_reader() {
        let (ledger, scope) = fixture();
        for (bytes, records) in [(u64::MAX, 0), (0, usize::MAX)] {
            let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
            let before = phase.finish().unwrap();
            let original = phase.guard().unwrap();
            {
                let _io = phase.enter();
                assert!(charge_owned_metadata(bytes, records).is_err());
            }
            assert!(phase.seal_retained(before.0, before.1).is_err());
            assert!(phase.seal_retained(0, 0).is_err());
            assert!(phase.finish().is_err());
            assert_eq!(ledger.report().unwrap().observed_bytes, before.0);
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, before.0);
            drop(original);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
    }
    fn assert_interrupted_read_matches_std(interrupt_on: usize) {
        struct InterruptingReader {
            remaining: &'static [u8],
            reads: usize,
            interrupt_on: usize,
        }
        impl Read for InterruptingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                if self.reads == self.interrupt_on {
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                let count = buffer.len().min(3);
                self.remaining.read(&mut buffer[..count])
            }
        }
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let expected = (header + (512 + 6) * 8, 72);
        let phase =
            ResourceReadMaterial::with_limits(&ledger, &scope, expected.0, expected.1).unwrap();
        for active in [false, true] {
            let mut reader = InterruptingReader {
                remaining: b"abcdef",
                reads: 0,
                interrupt_on,
            };
            let mut buffer = Vec::with_capacity(64);
            buffer.extend_from_slice(b"prefix:");
            {
                let _entry = active.then(|| phase.enter());
                assert_eq!(
                    crate::diagnostics::io_probe::read_to_end(&mut reader, &mut buffer).unwrap(),
                    6
                );
            }
            assert_eq!(buffer, b"prefix:abcdef");
            assert_eq!(reader.reads, 4);
            assert_eq!(
                phase.finish().unwrap(),
                if active { expected } else { (header, 64) }
            );
        }
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn initial_interrupted_read_matches_std_without_duplicate_charges() {
        assert_interrupted_read_matches_std(1);
    }

    #[test]
    fn midstream_interrupted_read_matches_std_without_duplicate_charges() {
        assert_interrupted_read_matches_std(2);
    }

    #[test]
    fn non_interrupted_read_errors_are_not_retried() {
        struct FailingReader {
            reads: usize,
        }
        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "reader failure",
                ))
            }
        }
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let before = phase.finish().unwrap();
        for active in [false, true] {
            let mut reader = FailingReader { reads: 0 };
            let mut buffer = b"prefix:".to_vec();
            let error = {
                let _entry = active.then(|| phase.enter());
                crate::diagnostics::io_probe::read_to_end(&mut reader, &mut buffer).unwrap_err()
            };
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "reader failure");
            assert_eq!(reader.reads, 1);
            assert_eq!(buffer, b"prefix:");
            assert_eq!(
                phase.finish().unwrap(),
                if active {
                    (before.0 + 512 * 8, before.1 + 8)
                } else {
                    before
                }
            );
        }
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn empty_and_failed_readers_admit_headers_before_calling_read() {
        struct CountRead {
            reads: usize,
        }
        impl Read for CountRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                Ok(0)
            }
        }
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (bytes, records, latch) in [
            (header, 1024, false),
            (header + 4096, 64, false),
            (header + 4096, 72, true),
        ] {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, records).unwrap();
            let mut reader = CountRead { reads: 0 };
            {
                let _entry = phase.enter();
                if latch {
                    assert!(charge_owned_metadata(bytes, 0).is_err());
                }
                assert!(read_to_end(&mut reader, &mut Vec::new()).unwrap().is_err());
                assert!(
                    crate::diagnostics::io_probe::open_file(Path::new("/absent-plan")).is_err()
                );
                // Admission wins over NotFound, including before a directory is opened.
                assert_eq!(
                    crate::diagnostics::io_probe::read_dir(Path::new("/absent-directory"))
                        .err()
                        .unwrap()
                        .kind(),
                    io::ErrorKind::Other
                );
            }
            assert_eq!(reader.reads, 0);
            assert_retryable_resource_error(&phase.finish().unwrap_err());
            drop(phase);
        }
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, header + 4096, 72).unwrap();
        let mut reader = CountRead { reads: 0 };
        {
            let _entry = phase.enter();
            assert_eq!(
                read_to_end(&mut reader, &mut Vec::new()).unwrap().unwrap(),
                0
            );
        }
        assert_eq!(reader.reads, 1);
        assert_eq!(phase.finish().unwrap(), (header + 4096, 72));
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn nested_entries_restore_outer_and_share_same_material_without_resetting_it() {
        let (ledger, scope) = fixture();
        let outer = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let inner = ResourceReadMaterial::with_limits(
            &ledger,
            &scope,
            (scope.agent_id.len() as u64 + 256) * 8,
            64,
        )
        .unwrap();
        let before = outer.finish().unwrap();
        {
            let _outer = outer.enter();
            {
                let _same = outer.enter();
                charge_owned_metadata(10, 1).unwrap();
            }
            assert_eq!(outer.finish().unwrap(), (before.0 + 80, before.1 + 8));
            {
                let _inner = inner.enter();
                assert!(charge_owned_metadata(1, 0).is_err());
                assert!(check_active().is_err());
            }
            check_active().unwrap();
            charge_owned_metadata(20, 1).unwrap();
        }
        assert!(ACTIVE.with(|slot| slot.borrow().is_none()));
        assert_eq!(outer.finish().unwrap(), (before.0 + 240, before.1 + 16));
        assert_retryable_resource_error(&inner.finish().unwrap_err());
        drop((outer, inner));
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn current_or_new_admits_one_header_and_reuses_original_remainder_and_failure() {
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::current_or_new(&ledger, &scope).unwrap();
        let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
        assert_eq!(phase.finish().unwrap(), header);
        assert!(ACTIVE.with(|slot| slot.borrow().is_none()));
        let (unused_ledger, unused_scope) = fixture();
        let weak = Arc::downgrade(&phase.guard().unwrap());
        {
            let _entry = phase.enter();
            charge_owned_metadata(10, 1).unwrap();
            let reused =
                ResourceReadMaterial::current_or_new(&unused_ledger, &unused_scope).unwrap();
            assert!(Arc::ptr_eq(&phase.active, &reused.active));
            assert_eq!(reused.finish().unwrap(), (header.0 + 80, header.1 + 8));
            assert_eq!(unused_ledger.report().unwrap().observed_bytes, 0);
            assert!(charge_owned_metadata(u64::MAX, 0).is_err());
            assert_retryable_resource_error(
                &ResourceReadMaterial::current_or_new(&unused_ledger, &unused_scope)
                    .err()
                    .expect("a failed active phase must not acquire fresh credit"),
            );
            assert!(reused.finish().is_err());
            assert_eq!(unused_ledger.report().unwrap().observed_bytes, 0);
        }
        assert!(ACTIVE.with(|slot| slot.borrow().is_none()));
        drop(phase);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn empty_and_small_plans_admit_owned_metadata_at_exact_byte_and_record_limits() {
        if run_isolated_plan_probe(
            "empty_and_small_plans_admit_owned_metadata_at_exact_byte_and_record_limits",
        ) {
            return;
        }
        use crate::types::{WorkItemRecord, WorkItemState};
        use crate::work_item_plan::{ensure_plan_artifact, plan_path};

        let home = tempfile::tempdir().unwrap();
        let record = WorkItemRecord::new("resource-read", "plan boundary", WorkItemState::Open);
        let path = plan_path(home.path(), &record.id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let (ledger, scope) = fixture();
        for body in [b"".as_slice(), "small plan 🦀".as_bytes(), &[0xff, b'x']] {
            std::fs::write(&path, body).unwrap();
            let phase =
                ResourceReadMaterial::with_limits(&ledger, &scope, 1024 * 1024, 1024).unwrap();
            let expected = {
                let _entry = phase.enter();
                ensure_plan_artifact(home.path(), &record, None).unwrap()
            };
            let (bytes, records) = phase.finish().unwrap();
            assert!(records > 64, "even an empty plan owns descriptor records");
            assert!(bytes > (scope.agent_id.len() as u64 + 256) * 8);
            drop(phase);

            for (byte_limit, record_limit, accepted) in [
                (bytes, records, true),
                (bytes - 1, records, false),
                (bytes, records - 1, false),
            ] {
                let phase =
                    ResourceReadMaterial::with_limits(&ledger, &scope, byte_limit, record_limit)
                        .unwrap();
                let probe = crate::diagnostics::io_probe::IoProbe::start();
                let result = {
                    let _entry = phase.enter();
                    ensure_plan_artifact(home.path(), &record, None)
                };
                if accepted {
                    assert_eq!(result.unwrap(), expected);
                    assert_eq!(phase.finish().unwrap(), (bytes, records));
                } else {
                    assert!(result.is_err());
                    assert_retryable_resource_error(&phase.finish().unwrap_err());
                    if byte_limit < bytes {
                        assert_eq!(
                            probe.snapshot().counters.file_read_attempts,
                            0,
                            "declared body preflight rejects before the reader wrapper"
                        );
                    }
                }
                drop((probe, phase));
                assert_eq!(ledger.report().unwrap().observed_bytes, 0);
            }
        }
    }

    #[test]
    fn plan_first_file_uses_remaining_allowance_before_directory_or_file_creation() {
        use crate::{
            types::{WorkItemRecord, WorkItemState},
            work_item_plan::{ensure_plan_artifact, plan_path},
        };
        let home = tempfile::tempdir().unwrap();
        let record =
            WorkItemRecord::new("resource-read", "first file admission", WorkItemState::Open);
        let path = plan_path(home.path(), &record.id);
        let (ledger, scope) = fixture();
        let body = "nonempty initial plan\n".repeat(512);
        let denied = ResourceReadMaterial::with_limits(&ledger, &scope, 512 * 1024, 4096).unwrap();
        {
            let _entry = denied.enter();
            charge_owned_metadata(48 * 1024, 32).unwrap();
            assert!(ensure_plan_artifact(home.path(), &record, Some(&body)).is_err());
        }
        assert_retryable_resource_error(&denied.finish().unwrap_err());
        assert!(!path.parent().unwrap().exists());
        assert!(record.plan_artifact.is_none());
        drop(denied);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        let retry =
            ResourceReadMaterial::with_limits(&ledger, &scope, 2 * 1024 * 1024, 4096).unwrap();
        {
            let _entry = retry.enter();
            let artifact = ensure_plan_artifact(home.path(), &record, Some(&body)).unwrap();
            assert_eq!(artifact.bytes, body.len() as u64);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        drop(retry);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn plan_growth_after_stat_is_admitted_and_reports_actual_bytes_or_exhaustion() {
        use crate::work_item_plan::describe_plan_artifact;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("plan.md");
        let (ledger, scope) = fixture();
        for large in [false, true] {
            std::fs::write(&path, "old").unwrap();
            let phase =
                ResourceReadMaterial::with_limits(&ledger, &scope, 128 * 1024, 4096).unwrap();
            let grown = if large {
                "x".repeat(128 * 1024)
            } else {
                "grown plan body".into()
            };
            phase.after_file_metadata({
                let path = path.clone();
                let grown = grown.clone();
                move || std::fs::write(path, grown).unwrap()
            });
            let result = {
                let _entry = phase.enter();
                describe_plan_artifact(&path, "resource-read", "work-a")
            };
            if large {
                assert!(result.is_err());
                assert_retryable_resource_error(&phase.finish().unwrap_err());
            } else {
                let artifact = result.unwrap();
                assert_eq!(artifact.bytes, grown.len() as u64);
                assert_eq!(artifact.preview, grown);
                phase.finish().unwrap();
            }
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
    }

    #[test]
    fn optional_plans_stop_open_read_and_creation_after_the_first_quota_failure() {
        if run_isolated_plan_probe(
            "optional_plans_stop_open_read_and_creation_after_the_first_quota_failure",
        ) {
            return;
        }
        use crate::types::{WorkItemRecord, WorkItemState};
        use crate::work_item_plan::{ensure_plan_artifact, plan_path};

        let home = tempfile::tempdir().unwrap();
        let records = (0..3)
            .map(|_| WorkItemRecord::new("resource-read", "optional plan", WorkItemState::Open))
            .collect::<Vec<_>>();
        for record in &records[..2] {
            let path = plan_path(home.path(), &record.id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"small later plan").unwrap();
        }
        std::fs::File::create(plan_path(home.path(), &records[0].id))
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 64 * 1024, 1024).unwrap();
        let probe = crate::diagnostics::io_probe::IoProbe::start();
        {
            let _entry = phase.enter();
            // Same real optional read used by FilesystemContextResources::plan.
            let plan = |record| ensure_plan_artifact(home.path(), record, None).ok();
            assert!(plan(&records[0]).is_none());
            let first = probe.snapshot().counters;
            assert_eq!(first.file_open_attempts, 1);
            assert_eq!(first.file_read_attempts, 0);
            assert!(plan(&records[1]).is_none());
            assert!(plan(&records[2]).is_none());
            let later = probe.snapshot().counters;
            assert_eq!(later.file_open_attempts, first.file_open_attempts);
            assert_eq!(later.file_read_attempts, first.file_read_attempts);
            assert_eq!(later.directory_read_attempts, first.directory_read_attempts);
        }
        assert!(!plan_path(home.path(), &records[2].id).exists());
        assert_retryable_resource_error(&phase.finish().unwrap_err());
        drop((probe, phase));
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn optional_text_decode_does_not_latch_but_physical_invalid_data_does() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SKILL.md");
        std::fs::write(&file, [0xff]).unwrap();
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        {
            let _entry = phase.enter();
            let error = crate::diagnostics::io_probe::read_to_string(&file).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            check_active().unwrap();
        }
        phase.finish().unwrap();
        drop(phase);
        for kind in [io::ErrorKind::InvalidData, io::ErrorKind::PermissionDenied] {
            let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
            {
                let _entry = phase.enter();
                observe_read_error(&Err::<(), _>(io::Error::new(kind, "physical read failure")));
                let error = check_active().unwrap_err();
                assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), kind);
                assert_eq!(
                    crate::runtime_error::describe_runtime_error(&error).code,
                    "io_error",
                );
            }
            let error = phase.finish().unwrap_err();
            assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), kind);
            assert_eq!(
                crate::runtime_error::describe_runtime_error(&error).code,
                "io_error",
            );
        }
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn actual_file_allowance_precedes_utf8_and_keeps_original_error_without_a_phase() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("AGENTS.md");
        std::fs::write(&file, vec![0xff; 128 * 1024]).unwrap();
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 64 * 1024, 1024).unwrap();
        {
            let _entry = phase.enter();
            assert!(crate::diagnostics::io_probe::read_to_string(&file)
                .unwrap_err()
                .to_string()
                .contains("ResourceExhausted"));
        }
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(ledger.report().unwrap().owned.payload_bytes, 0);
        assert_eq!(
            crate::diagnostics::io_probe::read_to_string(&file)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    #[test]
    fn real_guidance_and_catalog_share_exact_allowance_and_last_guard() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("AGENTS.md"), "original guidance").unwrap();
        let root = home.join("skills");
        std::fs::create_dir_all(root.join("sample")).unwrap();
        std::fs::write(
            root.join("sample/SKILL.md"),
            "---\nname: Sample\ndescription: Original catalog\n---\nbody",
        )
        .unwrap();
        let (ledger, scope) = fixture();
        let load = |phase: &ResourceReadMaterial| {
            let _entry = phase.enter();
            let guidance = crate::agents_md::load_agents_md(None, home, None).unwrap();
            let catalog =
                crate::skills::load_catalog_for_scope(crate::types::SkillScope::Agent, &root)
                    .unwrap();
            (guidance, catalog)
        };
        let measured = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let (guidance, catalog) = load(&measured);
        assert_eq!(
            guidance.agent_source.as_ref().unwrap().content,
            "original guidance"
        );
        assert_eq!(catalog.len(), 1);
        let usage = measured.finish().unwrap();
        drop((guidance, catalog, measured));
        for limits in [(usage.0 - 1, usage.1), (usage.0, usage.1 - 1)] {
            let phase =
                ResourceReadMaterial::with_limits(&ledger, &scope, limits.0, limits.1).unwrap();
            let result = {
                let _entry = phase.enter();
                let guidance = crate::agents_md::load_agents_md(None, home, None);
                let catalog =
                    crate::skills::load_catalog_for_scope(crate::types::SkillScope::Agent, &root);
                (guidance, catalog)
            };
            assert!(
                phase.finish().is_err(),
                "optional catalog omissions cannot hide a quota error"
            );
            drop((result, phase));
        }
        let exact = ResourceReadMaterial::with_limits(&ledger, &scope, usage.0, usage.1).unwrap();
        let values = load(&exact);
        assert_eq!(exact.finish().unwrap(), usage);
        let guard = exact.guard().unwrap();
        let weak = Arc::downgrade(&guard);
        let clone = guard.clone();
        drop((values, exact, guard));
        assert!(weak.upgrade().is_some());
        drop(clone);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn one_material_can_resume_sync_reads_without_leaking_tls_across_the_gap() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("AGENTS.md");
        std::fs::write(&file, "observed").unwrap();
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        {
            let _entry = phase.enter();
            assert_eq!(
                crate::diagnostics::io_probe::read_to_string(&file).unwrap(),
                "observed"
            );
        }
        let before = phase.finish().unwrap();
        assert!(ACTIVE.with(|slot| slot.borrow().is_none()));
        {
            let _entry = phase.enter();
            assert_eq!(
                crate::diagnostics::io_probe::read_to_string(&file).unwrap(),
                "observed"
            );
        }
        let after = phase.finish().unwrap();
        assert!(after.0 > before.0 && after.1 > before.1);
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    fn io_source_chain_keeps_typed_rejection(error: &io::Error) {
        let mut source: Option<&dyn std::error::Error> = Some(error);
        while let Some(error) = source {
            if let Some(typed) = error.downcast_ref::<crate::runtime_error::RuntimeError>() {
                assert_eq!(typed.descriptor().code, "context_resource_exhausted");
                assert!(typed.descriptor().retryable);
                return;
            }
            source = error.source();
        }
        panic!("direct charge rejection lost its typed RuntimeError source");
    }

    #[test]
    fn borrowed_owner_identity_header_precedes_registration_and_retains_exact_charge() {
        let (ledger, scope) = fixture();
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        for (bytes, records) in [(header - 1, 64), (header, 63)] {
            // Invalid incarnation is a registration sentinel: the header must
            // reject first, without constructing an owned InputEvidenceScope.
            let error = ResourceReadMaterial::with_identity_limits(
                &ledger,
                &scope.agent_id,
                0,
                bytes,
                records,
            )
            .err()
            .unwrap();
            assert_retryable_resource_error(&error);
            assert!(ledger.report().unwrap().scopes.is_empty());
            assert!(ResourceReadMaterial::active_guard().unwrap().is_none());
        }
        let large_id = "a".repeat(9 * 1024 * 1024);
        let error = ResourceReadMaterial::current_or_new_for_identity(&ledger, &large_id, 0)
            .err()
            .unwrap();
        assert_retryable_resource_error(&error);
        assert!(ledger.report().unwrap().scopes.is_empty());
        let exact = ResourceReadMaterial::with_identity_limits(
            &ledger,
            &scope.agent_id,
            scope.identity_incarnation,
            header,
            64,
        )
        .unwrap();
        assert_eq!(exact.finish().unwrap(), (header, 64));
        let report = ledger.report().unwrap();
        assert_eq!(report.observed_bytes, header);
        assert_eq!(report.scopes.len(), 1);
        assert_eq!(report.scopes[0].scope, scope);
        let last = exact.guard().unwrap();
        let weak = Arc::downgrade(&last);
        drop(exact);
        assert_eq!(ledger.report().unwrap().observed_bytes, header);
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        assert!(ResourceReadMaterial::active_guard().unwrap().is_none());
    }
    #[test]
    fn borrowed_owner_identity_reuses_active_credit_and_failure_without_registration() {
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let before = phase.finish().unwrap();
        let (unused, _) = fixture();
        {
            let _entry = phase.enter();
            // Unused invalid identity cannot create another namespace or credit.
            let reused = ResourceReadMaterial::current_or_new_for_identity(&unused, "", 0).unwrap();
            assert!(Arc::ptr_eq(&phase.active, &reused.active));
            charge_owned_metadata(10, 1).unwrap();
            assert_eq!(reused.finish().unwrap(), (before.0 + 80, before.1 + 8));
            assert!(charge_owned_metadata(u64::MAX, 0).is_err());
            let error = ResourceReadMaterial::current_or_new_for_identity(&unused, "", 0)
                .err()
                .unwrap();
            assert_retryable_resource_error(&error);
            assert!(reused.finish().is_err());
        }
        assert!(ResourceReadMaterial::active_guard().unwrap().is_none());
        assert!(unused.report().unwrap().scopes.is_empty());
        drop(phase);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
    #[test]
    fn original_header_is_gated_before_scope_registration_and_exhaustion_is_typed() {
        let (ledger, scope) = fixture();
        let bytes = (scope.agent_id.len() as u64 + 256) * 8;
        for (limit_bytes, limit_nodes) in [(bytes - 1, 64), (bytes, 63), (0, 0)] {
            let error =
                ResourceReadMaterial::with_limits(&ledger, &scope, limit_bytes, limit_nodes)
                    .err()
                    .expect("header must reject before registration");
            assert_retryable_resource_error(&error);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
            assert!(ledger.report().unwrap().scopes.is_empty());
        }
        let large_scope = InputEvidenceScope {
            agent_id: "a".repeat(1024 * 1024),
            identity_incarnation: 1,
        };
        let error = ResourceReadMaterial::with_limits(&ledger, &large_scope, 1024, 64)
            .err()
            .unwrap();
        assert_retryable_resource_error(&error);
        assert!(ledger.report().unwrap().scopes.is_empty());
        let exact = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, 64).unwrap();
        assert_eq!(exact.finish().unwrap(), (bytes, 64));
        drop(exact);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn original_file_quota_latch_reaches_the_actual_runtime_error_describer() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("AGENTS.md");
        std::fs::write(&file, vec![0xff; 128 * 1024]).unwrap();
        let (ledger, scope) = fixture();
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 64 * 1024, 1024).unwrap();
        let read = {
            let _entry = phase.enter();
            crate::agents_md::load_agents_md(None, directory.path(), None)
        };
        assert!(read.is_err());
        let error = phase
            .finish()
            .unwrap_err()
            .context("actual context resource preparation");
        assert_retryable_resource_error(&error);
        drop((read, phase));
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn cached_catalog_name_entry_and_view_copies_share_the_original_allowance() {
        use crate::types::{
            SkillRootRegistration, SkillRootScanStatus, SkillRootSourceKind, SkillRootWatchStatus,
        };
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        let skill = root.join("sample/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            format!(
                "---\nname: Sample\ndescription: {}\n---\nbody",
                "c".repeat(128 * 1024)
            ),
        )
        .unwrap();
        let roots = vec![SkillRootRegistration {
            source_kind: SkillRootSourceKind::AgentHome,
            owner_agent_id: None,
            root_path: root.clone(),
            scan_status: SkillRootScanStatus::NeverScanned,
            watch_status: SkillRootWatchStatus::NotWatched,
        }];
        let mut registry = crate::skills::SkillsRegistry::new();
        registry.replace_roots(roots.clone()).unwrap();
        let original = registry.catalog();
        assert_eq!(original.len(), 1);
        // The real catalog path is cache-only; the guard must not rely on file reads.
        std::fs::remove_file(&skill).unwrap();
        let (ledger, scope) = fixture();
        let project = |phase: &ResourceReadMaterial| {
            let _entry = phase.enter();
            crate::skills::skills_runtime_view_from_catalog(
                registry.catalog_for_roots(&roots, None),
                &roots,
                &[],
            )
        };
        let measured = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let view = project(&measured).unwrap();
        assert_eq!(*view.discoverable_skills, original);
        assert_eq!(*view.attached_skills, original);
        assert!(Arc::ptr_eq(
            &view.discoverable_skills,
            &view.attached_skills
        ));
        let usage = measured.finish().unwrap();
        drop((view, measured));
        for (bytes, nodes) in [(usage.0 - 1, usage.1), (usage.0, usage.1 - 1)] {
            let phase = ResourceReadMaterial::with_limits(&ledger, &scope, bytes, nodes).unwrap();
            assert!(project(&phase).is_err());
            assert_retryable_resource_error(&phase.finish().unwrap_err());
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
        let exact = ResourceReadMaterial::with_limits(&ledger, &scope, usage.0, usage.1).unwrap();
        let view = project(&exact).unwrap();
        assert_eq!(*view.discoverable_skills, original);
        assert_eq!(*view.attached_skills, original);
        assert_eq!(exact.finish().unwrap(), usage);
        let guard = exact.guard().unwrap();
        let weak = Arc::downgrade(&guard);
        drop((view, exact));
        assert!(weak.upgrade().is_some());
        drop(guard);
        assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        assert_eq!(
            registry.catalog(),
            original,
            "without a phase catalog behavior is unchanged"
        );
    }

    #[test]
    fn shared_registry_sync_rejects_quota_candidates_without_changing_cached_roots() {
        use crate::types::{
            SkillRootRegistration, SkillRootScanStatus, SkillRootSourceKind, SkillRootWatchStatus,
        };
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        let skill = root.join("sample/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\nname: Sample\ndescription: original\n---\nbody",
        )
        .unwrap();
        let registration = SkillRootRegistration {
            source_kind: SkillRootSourceKind::AgentHome,
            owner_agent_id: None,
            root_path: root.clone(),
            scan_status: SkillRootScanStatus::NeverScanned,
            watch_status: SkillRootWatchStatus::NotWatched,
        };
        let mut registry = crate::skills::SkillsRegistry::new();
        registry
            .sync_effective_roots([registration.clone()])
            .unwrap();
        registry.record_watcher_started(&root);
        let original_catalog = registry.catalog();
        let original_roots = registry.roots().to_vec();
        let (ledger, scope) = fixture();
        // Fail during the initial fingerprint of an otherwise unchanged root.
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 4096, 1024).unwrap();
        let result = {
            let _entry = phase.enter();
            registry.sync_effective_roots([registration.clone()])
        };
        assert_retryable_resource_error(&result.unwrap_err());
        assert_retryable_resource_error(&phase.finish().unwrap_err());
        drop(phase);
        assert_eq!(registry.catalog(), original_catalog);
        assert_eq!(registry.roots(), original_roots);

        // Admit fingerprinting, then fail the candidate's oversized file read.
        std::fs::write(&skill, vec![0xff; 128 * 1024]).unwrap();
        let mut changed_registration = registration.clone();
        changed_registration.owner_agent_id = Some("changed-owner".into());
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 512 * 1024, 16_384).unwrap();
        let result = {
            let _entry = phase.enter();
            registry.sync_effective_roots([changed_registration.clone()])
        };
        assert_retryable_resource_error(&result.unwrap_err());
        assert_retryable_resource_error(&phase.finish().unwrap_err());
        drop(phase);
        assert_eq!(registry.catalog(), original_catalog);
        assert_eq!(registry.roots(), original_roots);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);

        // A failed first registration must not install a root or an empty cache.
        let mut fresh_registry = crate::skills::SkillsRegistry::new();
        let phase = ResourceReadMaterial::with_limits(&ledger, &scope, 512 * 1024, 16_384).unwrap();
        let result = {
            let _entry = phase.enter();
            fresh_registry.sync_effective_roots([registration])
        };
        assert_retryable_resource_error(&result.unwrap_err());
        drop(phase);
        assert!(fresh_registry.roots().is_empty());
        assert!(fresh_registry.catalog().is_empty());

        // Ordinary invalid UTF-8 is skipped by a successful scan outside a phase.
        registry
            .sync_effective_roots([changed_registration])
            .unwrap();
        assert!(registry.catalog().is_empty());
        assert!(matches!(
            registry.roots()[0].scan_status,
            SkillRootScanStatus::Scanned { .. }
        ));
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }
}

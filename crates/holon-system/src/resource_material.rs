//! Separate admission envelopes for JSON decoding and existing typed clones.

use std::{cell::Cell, io, marker::PhantomData, rc::Rc};

/// Synchronous owning-material accounting installed by the runtime.
#[derive(Clone, Copy)]
pub struct ResourceObserver {
    pub payload: fn(u64) -> io::Result<()>,
    pub buffer: fn(u64) -> io::Result<()>,
    pub metadata: fn(u64, usize) -> io::Result<()>,
}

thread_local! {
    static OBSERVERS: Cell<Option<ResourceObserver>> = const { Cell::new(None) };
}

#[must_use = "the observer is restored when the scope is dropped"]
pub struct ResourceObserverScope {
    previous: Option<ResourceObserver>,
    thread: PhantomData<Rc<()>>,
}

impl ResourceObserverScope {
    pub fn enter(observer: Option<ResourceObserver>) -> Self {
        Self {
            previous: OBSERVERS.with(|slot| slot.replace(observer)),
            thread: PhantomData,
        }
    }
}

impl Drop for ResourceObserverScope {
    fn drop(&mut self) {
        OBSERVERS.with(|slot| slot.set(self.previous));
    }
}

pub fn charge_owned_payload(bytes: u64) -> io::Result<()> {
    OBSERVERS.with(|slot| {
        slot.get()
            .map_or(Ok(()), |observer| (observer.payload)(bytes))
    })
}

pub fn charge_owned_buffer(bytes: u64) -> io::Result<()> {
    OBSERVERS.with(|slot| {
        slot.get()
            .map_or(Ok(()), |observer| (observer.buffer)(bytes))
    })
}

pub fn charge_owned_metadata(bytes: u64, records: usize) -> io::Result<()> {
    OBSERVERS.with(|slot| {
        slot.get()
            .map_or(Ok(()), |observer| (observer.metadata)(bytes, records))
    })
}

use anyhow::Result;

/// Allocation-free JSON-shape accounting, independent of punctuation inside
/// report strings. No source body is cloned by this counter.
#[derive(Default)]
struct StructuredMaterialCounter {
    bytes: u64,
    slots: usize,
    cells: usize,
    cells_peak: usize,
    quoted: bool,
    escaped: bool,
    literal: bool,
}

impl std::io::Write for StructuredMaterialCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let exhausted = || std::io::Error::other("ResourceExhausted: completion material");
        self.bytes = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| exhausted())?)
            .ok_or_else(exhausted)?;
        for byte in bytes {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.quoted = false;
                }
            } else {
                // Count JSON values, not struct field names or punctuation.
                // A colon removes the preceding key string; nested values and
                // array entries each retain their own cell. No parser allocates.
                if matches!(*byte, b'"' | b'{' | b'[')
                    || (!self.literal && matches!(*byte, b'-' | b'0'..=b'9' | b't' | b'f' | b'n'))
                {
                    self.cells = self.cells.checked_add(1).ok_or_else(exhausted)?;
                    self.cells_peak = self.cells_peak.max(self.cells);
                }
                if *byte == b':' {
                    self.cells = self.cells.saturating_sub(1);
                }
                if matches!(*byte, b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',')
                    || byte.is_ascii_whitespace()
                {
                    self.literal = false;
                } else {
                    self.literal = true;
                }
                if matches!(*byte, b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',') {
                    self.slots = self.slots.checked_add(1).ok_or_else(exhausted)?;
                }
                self.quoted = *byte == b'"';
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl StructuredMaterialCounter {
    #[track_caller]
    fn admit(self) -> Result<()> {
        // Completion decoding also covers aggregate/resolver/projection copies.
        let records = self
            .slots
            .checked_mul(4)
            .and_then(|records| records.checked_add(16))
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))?;
        self.admit_records(records)
    }

    #[track_caller]
    fn admit_clone(self) -> Result<()> {
        // No parser or resolver is run for this one typed clone. Structural
        // punctuation bounds its owning cells; the phase's original eightfold
        // allowance covers container capacity. Every later clone is gated anew.
        let records = self
            .slots
            .checked_add(16)
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: cloned material"))?;
        self.admit_records(records)
    }

    #[track_caller]
    fn admit_records(self, records: usize) -> Result<()> {
        let bytes = self.material_bytes()?;
        charge_owned_metadata(bytes, records)?;
        Ok(())
    }

    fn material_bytes(&self) -> Result<u64> {
        // Keep the original conservative bytes envelope for both paths.
        self.bytes
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(u64::try_from(self.slots).ok()?.checked_mul(128)?))
            .and_then(|bytes| bytes.checked_add(512))
            .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))
    }
}

/// Pure material estimate for callers admitting to an explicit Native phase.
pub fn json_payload_material(payload: &str) -> Result<(usize, usize)> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    let records = counter
        .slots
        .checked_mul(4)
        .and_then(|records| records.checked_add(16))
        .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: completion material"))?;
    Ok((usize::try_from(counter.material_bytes()?)?, records))
}

/// One existing typed clone, without a JSON parser or completion resolver.
pub fn cloned_value_material(value: &impl serde::Serialize) -> Result<(usize, usize)> {
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    let records = counter
        .slots
        .checked_add(16)
        .ok_or_else(|| anyhow::anyhow!("ResourceExhausted: cloned material"))?;
    Ok((usize::try_from(counter.material_bytes()?)?, records))
}

/// Only for existing Context types whose owning fields are serialized. This is
/// not the admission gate for parsing JSON or resolving completion aggregates.
#[track_caller]
pub fn admit_cloned_value(value: &impl serde::Serialize) -> Result<()> {
    if !OBSERVERS.with(|slot| slot.get().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    counter.admit_clone()
}

/// One direct typed clone whose serialized fields cover its owning material.
/// Count values rather than JSON punctuation, as for a direct typed decode.
#[track_caller]
pub fn admit_typed_cloned_value(value: &impl serde::Serialize) -> Result<()> {
    if !OBSERVERS.with(|slot| slot.get().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    let records = counter.cells_peak;
    counter.admit_records(records)
}

/// Admit only serde_json::to_string's owned buffer, without a decoded tree.
/// Preserve the conservative bytes envelope and count that one allocation.
#[track_caller]
pub fn admit_json_buffer(value: &impl serde::Serialize) -> Result<()> {
    if !OBSERVERS.with(|slot| slot.get().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    counter.admit_records(1)
}

#[track_caller]
pub fn admit_serialized_value(value: &impl serde::Serialize) -> Result<()> {
    admit_serialized_material(|writer| serde_json::to_writer(writer, value))
}

/// Admit a value serialized by a caller in another crate using the same counter.
#[track_caller]
pub fn admit_serialized_material_with(
    serialize: &dyn Fn(&mut dyn std::io::Write) -> serde_json::Result<()>,
) -> Result<()> {
    admit_serialized_material(|writer| serialize(writer))
}

#[track_caller]
fn admit_serialized_material(
    serialize: impl FnOnce(&mut StructuredMaterialCounter) -> serde_json::Result<()>,
) -> Result<()> {
    if !OBSERVERS.with(|slot| slot.get().is_some()) {
        return Ok(());
    }
    let mut counter = StructuredMaterialCounter::default();
    serialize(&mut counter)?;
    counter.admit()
}

#[track_caller]
pub fn admit_json_payload(payload: &str) -> Result<()> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    counter.admit()
}

/// One direct typed decode, with no completion aggregate/resolver copies.
/// Inline scalars still count as cells; the original eightfold allowance covers
/// owning strings, map entries and container capacity. Keep the bytes envelope.
#[track_caller]
pub fn admit_typed_json_payload(payload: &str) -> Result<()> {
    use std::io::Write;
    let mut counter = StructuredMaterialCounter::default();
    counter.write_all(payload.as_bytes())?;
    // Malformed suffixes must not erase an already admitted decode prefix.
    let records = counter.cells_peak;
    counter.admit_records(records)
}

#[cfg(test)]
mod tests;

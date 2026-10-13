//! Small replay decoration for an original typed business command.
//! This is not a receipt, memory authority, or permission to run the command.
use super::super::input_mutation_receipts::{lookup_input_mutation_receipt_tx, ReceiptError};
use super::*;

#[derive(Debug)]
pub(crate) enum OriginalAutoMemory {
    Unseen,
    /// The original business command committed without automatic memory.
    PlainReceipt,
    Restore(OriginalAutomaticMemory),
}

/// Fields stay private so restoration must rehash the caller's actual typed
/// business command. No old State, Work body, seed baseline, or effects live here.
#[derive(Debug)]
pub(crate) struct OriginalAutomaticMemory {
    metadata: AutomaticMemoryMetadata,
}
impl OriginalAutomaticMemory {
    pub(in crate::runtime_db) fn into_parts(
        self,
    ) -> (
        PreparedInputMutation,
        PreparedInputMutation,
        DispatchMemoryIntent,
    ) {
        (
            self.metadata.base,
            self.metadata.final_descriptor,
            self.metadata.intent.into_intent(),
        )
    }
}

/// Started always comes from its actual StartedTurn command, never this map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AutomaticMemoryIntent {
    Deferred {
        binding: DispatchMemoryBinding,
        frozen: FrozenEpisodeMemoryValues,
    },
    Finish {
        binding: DispatchMemoryBinding,
        frozen: FrozenEpisodeMemoryValues,
    },
}
impl AutomaticMemoryIntent {
    fn from_intent(intent: &DispatchMemoryIntent) -> Result<Self> {
        Ok(match intent {
            DispatchMemoryIntent::Deferred { binding, frozen } => Self::Deferred {
                binding: binding.clone(),
                frozen: frozen.clone(),
            },
            DispatchMemoryIntent::Finish { binding, frozen } => Self::Finish {
                binding: binding.clone(),
                frozen: frozen.clone(),
            },
            DispatchMemoryIntent::Started { .. } => {
                anyhow::bail!("actual StartedTurn memory cannot be an automatic decoration")
            }
        })
    }
    fn into_intent(self) -> DispatchMemoryIntent {
        match self {
            Self::Deferred { binding, frozen } => {
                DispatchMemoryIntent::Deferred { binding, frozen }
            }
            Self::Finish { binding, frozen } => DispatchMemoryIntent::Finish { binding, frozen },
        }
    }
    fn validate(&self, base: &PreparedInputMutation) -> Result<()> {
        let (binding, frozen, family_matches) = match self {
            Self::Deferred { binding, frozen } => (
                binding,
                frozen,
                matches!(
                    base.family(),
                    InputMutationFamily::WorkItem
                        | InputMutationFamily::Focus
                        | InputMutationFamily::Wait
                        | InputMutationFamily::Task
                        | InputMutationFamily::Queue
                ),
            ),
            Self::Finish { binding, frozen } => (
                binding,
                frozen,
                matches!(
                    base.family(),
                    InputMutationFamily::Terminal | InputMutationFamily::Queue
                ),
            ),
        };
        validate_frozen(frozen)?;
        validate_key(binding.key(), binding.invocation())?;
        ensure!(
            family_matches && base.scopes().contains(&binding.scope()),
            ReceiptError::CorruptReceipt(
                "automatic memory differs from original command scope/family"
            )
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AutomaticMemoryMetadata {
    base: PreparedInputMutation,
    final_descriptor: PreparedInputMutation,
    intent: AutomaticMemoryIntent,
}
impl AutomaticMemoryMetadata {
    fn validate(&self) -> Result<()> {
        validate_descriptor(&self.base)?;
        validate_descriptor(&self.final_descriptor)?;
        self.intent.validate(&self.base)?;
        ensure!(
            self.base.command_nonce() == self.final_descriptor.command_nonce()
                && self.base.family() == self.final_descriptor.family()
                && self.base.scopes() == self.final_descriptor.scopes()
                && self.base.prepared_at() == self.final_descriptor.prepared_at(),
            ReceiptError::CorruptReceipt("automatic memory changed original command address")
        );
        ensure!(
            self.base.scopes().len() <= MAX_METADATA_RECORDS,
            DispatchMemoryError::ResourceExhausted
        );
        Ok(())
    }
}

fn load_metadata_tx(
    tx: &Transaction<'_>,
    base: &PreparedInputMutation,
) -> Result<Option<AutomaticMemoryMetadata>> {
    validate_descriptor(base)?;
    let length: Option<usize> = tx.query_row(
        "SELECT length(CAST(payload_json AS BLOB)) FROM dispatch_memory_auto_commands WHERE command_nonce=?1",
        [base.command_nonce()], |row| row.get(0)).optional()?;
    let Some(length) = length else {
        return Ok(None);
    };
    ensure!(
        length <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    let (base_digest, final_digest, digest, payload): (String,String,String,String) = tx.query_row(
        "SELECT base_command_digest,final_command_digest,metadata_digest,payload_json FROM dispatch_memory_auto_commands WHERE command_nonce=?1",
        [base.command_nonce()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    let metadata: AutomaticMemoryMetadata =
        serde_json::from_str(&payload).context("decoding original automatic memory decoration")?;
    metadata.validate()?;
    ensure!(
        metadata.base.command_digest() == base_digest
            && metadata.final_descriptor.command_digest() == final_digest
            && canonical_command_digest(&metadata)? == digest,
        ReceiptError::CorruptReceipt("automatic memory normalized metadata differs")
    );
    ensure!(
        metadata.base.scopes() == base.scopes(),
        ReceiptError::GroupConflict
    );
    ensure!(metadata.base == *base, ReceiptError::CommandConflict);
    Ok(Some(metadata))
}

/// Protocol preparation under the already-held original owner group, before
/// allocating new frozen memory values or consulting today's binding/Done.
/// Restore still needs PreparedInputTransition's full typed-command rehash.
pub(crate) fn load_original_auto_memory_tx(
    tx: &Transaction<'_>,
    base: &PreparedInputMutation,
) -> Result<OriginalAutoMemory> {
    if let Some(metadata) = load_metadata_tx(tx, base)? {
        ensure!(
            lookup_input_mutation_receipt_tx(tx, &metadata.final_descriptor)?.is_some(),
            ReceiptError::CorruptReceipt("automatic memory has no original whole receipt")
        );
        return Ok(OriginalAutoMemory::Restore(OriginalAutomaticMemory {
            metadata,
        }));
    }
    // An explicit-memory nonce collision also fails here; never turn an
    // unrelated original command into a new automatic invocation.
    if lookup_input_mutation_receipt_tx(tx, base)?.is_some() {
        Ok(OriginalAutoMemory::PlainReceipt)
    } else {
        Ok(OriginalAutoMemory::Unseen)
    }
}

/// The outer whole transaction calls this after business + D1/Done, immediately
/// before its original whole receipt. A rollback removes both rows together.
pub(in crate::runtime_db) fn insert_original_auto_memory_tx(
    tx: &Transaction<'_>,
    base: &PreparedInputMutation,
    final_descriptor: &PreparedInputMutation,
    intent: &DispatchMemoryIntent,
) -> Result<()> {
    let metadata = AutomaticMemoryMetadata {
        base: base.clone(),
        final_descriptor: final_descriptor.clone(),
        intent: AutomaticMemoryIntent::from_intent(intent)?,
    };
    metadata.validate()?;
    if let Some(existing) = load_metadata_tx(tx, base)? {
        ensure!(existing == metadata, ReceiptError::CommandConflict);
        return Ok(());
    }
    let payload = serde_json::to_string(&metadata)?;
    ensure!(
        payload.len() <= MAX_METADATA_BYTES,
        DispatchMemoryError::ResourceExhausted
    );
    tx.execute("INSERT INTO dispatch_memory_auto_commands(command_nonce,base_command_digest,final_command_digest,metadata_digest,payload_json) VALUES(?1,?2,?3,?4,?5)",
        params![base.command_nonce(),base.command_digest(),final_descriptor.command_digest(),canonical_command_digest(&metadata)?,payload])?;
    Ok(())
}

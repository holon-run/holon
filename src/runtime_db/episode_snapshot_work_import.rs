//! Exact one-time legacy-domain records, never ordinary append authority.

use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime_db) enum ImportOrigin {
    ActualWrite,
    LegacyImportEqual,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ImportRecord {
    agent_id: String,
    raw_digest: String,
    origin: ImportOrigin,
    work_item_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportCheckpoint {
    policy_version: u32,
    records: BTreeMap<String, ImportRecord>,
}

#[derive(Default)]
pub(in crate::runtime_db) struct ImportWrites {
    keys: BTreeMap<(String, String), (String, ImportOrigin)>,
    metadata_bytes: usize,
}
impl ImportWrites {
    pub(in crate::runtime_db) fn record(
        &mut self,
        family: &str,
        id: &str,
        agent_id: &str,
        origin: ImportOrigin,
    ) -> Result<()> {
        ensure!(
            !id.is_empty() && !agent_id.is_empty(),
            "empty legacy import footprint key"
        );
        table(family)?;
        let key = (family.to_owned(), id.to_owned());
        if let Some((agent, prior)) = self.keys.get_mut(&key) {
            ensure!(
                agent == agent_id,
                "legacy import footprint ID changed agent"
            );
            if origin == ImportOrigin::ActualWrite {
                *prior = origin;
            }
        } else {
            self.metadata_bytes = self
                .metadata_bytes
                .saturating_add(family.len())
                .saturating_add(id.len())
                .saturating_add(agent_id.len())
                .saturating_add(64);
            self.keys.insert(key, (agent_id.to_owned(), origin));
        }
        bound(self.metadata_bytes)
    }

    /// Borrow the old import map only when a new original Input source phase
    /// coexists with it. This grants no source member and makes no body copy.
    pub(in crate::runtime_db) fn input_import_material_estimate(&self) -> Result<(u64, usize)> {
        let mut bytes = 1024u64;
        for ((family, id), (agent, origin)) in &self.keys {
            bytes = bytes
                .checked_add(crate::runtime::input_evidence_budget::serialized_bytes(&(
                    family, id, agent, origin,
                ))?)
                .and_then(|n| n.checked_add(512))
                .context("Evidence Work import map bytes overflow")?;
        }
        let records = self
            .keys
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(32))
            .and_then(|n| n.checked_mul(8))
            .context("Evidence Work import map records overflow")?;
        let bytes = bytes
            .checked_mul(8)
            .and_then(|n| n.checked_add(u64::try_from(records).ok()?.checked_mul(256)?))
            .context("Evidence Work import map bytes overflow")?;
        Ok((bytes, records))
    }

    /// Must run after every write in this original domain import transaction.
    pub(in crate::runtime_db) fn finish(self, tx: &Transaction<'_>) -> Result<serde_json::Value> {
        let mut records = BTreeMap::new();
        let mut metadata_bytes = 64usize;
        for ((family, id), (agent_id, origin)) in self.keys {
            let (table, column) = table(&family)?;
            let (stored_agent, payload): (String, String) = tx.query_row(
                &format!("SELECT agent_id,payload_json FROM {table} WHERE {column}=?1"),
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            ensure!(
                stored_agent == agent_id,
                "legacy import actual record changed scope"
            );
            let work_item_id = match family.as_str() {
                "work_item" => {
                    let record: WorkItemRecord = serde_json::from_str(&payload)?;
                    ensure!(
                        record.id == id && record.agent_id == agent_id,
                        "legacy Work import projection mismatch"
                    );
                    None
                }
                "wait" => {
                    let record: WaitConditionRecord = serde_json::from_str(&payload)?;
                    ensure!(
                        record.id == id && record.agent_id == agent_id,
                        "legacy Wait import projection mismatch"
                    );
                    None
                }
                "brief" => {
                    let record: BriefRecord = serde_json::from_str(&payload)?;
                    ensure!(
                        record.id == id && record.agent_id == agent_id,
                        "legacy Brief import projection mismatch"
                    );
                    record.work_item_id
                }
                "message" => {
                    let record: MessageEnvelope = serde_json::from_str(&payload)?;
                    ensure!(
                        record.id == id && record.agent_id == agent_id,
                        "legacy Message import projection mismatch"
                    );
                    None
                }
                "transcript" => {
                    let record: TranscriptEntry = serde_json::from_str(&payload)?;
                    ensure!(
                        record.id == id && record.agent_id == agent_id,
                        "legacy Transcript import projection mismatch"
                    );
                    None
                }
                _ => unreachable!(),
            };
            let key = format!("{family}:{id}");
            let record = ImportRecord {
                agent_id,
                raw_digest: bytes_digest(payload.as_bytes()),
                origin,
                work_item_id,
            };
            metadata_bytes = metadata_bytes
                .saturating_add(serde_json::to_vec(&key)?.len())
                .saturating_add(serde_json::to_vec(&record)?.len())
                .saturating_add(2);
            bound(metadata_bytes)?;
            records.insert(key, record);
        }
        let checkpoint = serde_json::to_value(ImportCheckpoint {
            policy_version: 1,
            records,
        })?;
        bound(serde_json::to_vec(&checkpoint)?.len())?;
        Ok(checkpoint)
    }
}

fn table(family: &str) -> Result<(&'static str, &'static str)> {
    Ok(match family {
        "work_item" => ("work_items", "work_item_id"),
        "wait" => ("wait_conditions", "wait_condition_id"),
        "brief" => ("briefs", "evidence_id"),
        "message" => ("messages", "evidence_id"),
        "transcript" => ("transcript_entries", "evidence_id"),
        _ => anyhow::bail!("invalid legacy Work footprint family"),
    })
}
fn bound(bytes: usize) -> Result<()> {
    if bytes > CONTINUITY_MAX_PAYLOAD_BYTES {
        return Err(EpisodeSnapshotError::ClosureResourceExhausted {
            limit: "legacy Work import footprint metadata",
        }
        .into());
    }
    Ok(())
}

#[path = "episode_snapshot_work_import_records.rs"]
mod imported_records;

pub(in crate::runtime_db) struct ImportAuthority {
    frozen: EpisodeContinuityRoots,
    records: imported_records::ImportedRecords,
    heads: BTreeMap<String, BTreeSet<String>>,
    additional: BTreeSet<(String, String)>,
    startup_wait_versions: BTreeMap<String, String>,
}
impl ImportAuthority {
    /// Called only for the original fixed import_required seed's first seal.
    pub(in crate::runtime_db::episode_snapshot) fn load(
        tx: &Transaction<'_>,
        agent_id: &str,
        frozen: EpisodeContinuityRoots,
    ) -> Result<Self> {
        let checkpoint = frozen
            .work_domain_checkpoint
            .as_ref()
            .context("fixed Work checkpoint missing before legacy import")?;
        let records = imported_records::ImportedRecords::load(tx, agent_id)?;
        let mut heads = BTreeMap::<String, BTreeSet<String>>::new();
        for (work, head) in &checkpoint.latest_results {
            if let Some(head) = head {
                heads
                    .entry(work.clone())
                    .or_default()
                    .insert(head.brief_id.clone());
            }
        }
        let mut additional = BTreeSet::new();
        for key in checkpoint.record_digests.keys() {
            if records.has(tx, key)? {
                let (family, id) = key
                    .split_once(':')
                    .context("invalid legacy Work footprint key")?;
                additional.insert((family.to_owned(), id.to_owned()));
            }
            if let Some(work) = key.strip_prefix("work_item:") {
                if records.has_brief_for_work(tx, work)? {
                    additional.insert(("work_item".to_owned(), work.to_owned()));
                }
            }
        }
        let startup_wait_versions =
            crate::runtime_db::work_snapshot::fixed_unbound_startup_wait_versions_tx(
                tx,
                agent_id,
                &frozen.record_digests,
            )?;
        Ok(Self {
            frozen,
            records,
            heads,
            additional,
            startup_wait_versions,
        })
    }
    /// Only exact received rows from the original, sealed domain imports.
    pub(in crate::runtime_db) fn visit_task_query_message_imports(
        &self,
        connection: &Connection,
        visit: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        self.records.visit_messages(connection, visit)
    }
    pub(in crate::runtime_db::episode_snapshot) fn finish(
        self,
        connection: &Connection,
    ) -> Result<()> {
        self.records.finish(connection)
    }
    pub(super) fn original_relations(&self) -> &WorkContinuityCheckpoint {
        self.frozen
            .work_domain_checkpoint
            .as_ref()
            .expect("ImportAuthority validated its fixed Work checkpoint")
    }
    pub(super) fn permits_relation_source(
        &self,
        connection: &Connection,
        id: &str,
        digest: &str,
    ) -> Result<bool> {
        let key = format!("message:{id}");
        Ok(self
            .frozen
            .record_digests
            .get(&key)
            .is_some_and(|stored| stored == digest)
            || self.records.matches(connection, &key, digest)?)
    }
    pub(super) fn validate_relation_wait(
        &self,
        connection: &Connection,
        id: &str,
        digest: &str,
    ) -> Result<()> {
        let key = format!("wait:{id}");
        ensure!(
            self.original_relations()
                .record_digests
                .get(&key)
                .is_some_and(|stored| stored == digest)
                || self
                    .startup_wait_versions
                    .get(&key)
                    .is_some_and(|stored| stored == digest)
                || self.records.matches(connection, &key, digest)?,
            "Wait relation has no original fixed or legacy import footprint: {id}"
        );
        Ok(())
    }
    pub(super) fn additional(&self) -> &BTreeSet<(String, String)> {
        &self.additional
    }
    pub(super) fn select_head(
        &self,
        connection: &Connection,
        agent: &str,
        work: &str,
    ) -> Result<(
        Option<(String, String)>,
        Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    )> {
        let empty = BTreeSet::new();
        self.records.select_head(
            connection,
            agent,
            work,
            self.heads.get(work).unwrap_or(&empty),
        )
    }
    pub(super) fn validate_fixed_version(
        &self,
        connection: &Connection,
        key: &str,
        digest: &str,
    ) -> Result<()> {
        if let Some(original) = self.frozen.record_digests.get(key) {
            ensure!(
                original == digest
                    || self
                        .startup_wait_versions
                        .get(key)
                        .is_some_and(|stored| stored == digest)
                    || self.records.matches(connection, key, digest)?,
                "fixed Work member changed outside original legacy import footprint: {key}"
            );
        }
        Ok(())
    }
    pub(super) fn require_original_head(&self, work: &str) -> Result<()> {
        ensure!(
            !self
                .frozen
                .work_domain_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.latest_results.get(work))
                .is_some_and(Option::is_some),
            "fixed Work completion head is unavailable, not an observed empty namespace"
        );
        Ok(())
    }
    pub(super) fn validate_head(
        &self,
        connection: &Connection,
        id: &str,
        payload: &str,
    ) -> Result<()> {
        let key = format!("brief:{id}");
        let digest = bytes_digest(payload.as_bytes());
        ensure!(
            self.frozen.record_digests.get(&key) == Some(&digest)
                || self.records.matches(connection, &key, &digest)?,
            "Work completion head has no original import or fixed proof"
        );
        Ok(())
    }
    pub(super) fn validate_inputs(
        &self,
        connection: &Connection,
        baseline: &EpisodeContinuityRoots,
        actual: &BTreeMap<String, String>,
    ) -> Result<()> {
        for (key, digest) in actual {
            if baseline.record_digests.get(key) == Some(digest) {
                continue;
            }
            ensure!(
                self.frozen.record_digests.get(key) == Some(digest)
                    || self.startup_wait_versions.get(key) == Some(digest)
                    || self.records.matches(connection, key, digest)?,
                "Work imported owning input has no original import or fixed proof: {key}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_footprint_does_not_limit_global_history_to_fixed_closure_count() {
        let mut writes = ImportWrites::default();
        for n in 0..100_000 {
            writes
                .record(
                    "message",
                    &format!("history-{n}"),
                    "agent-a",
                    ImportOrigin::ActualWrite,
                )
                .unwrap();
        }
        assert_eq!(writes.keys.len(), 100_000);
        assert!(writes.metadata_bytes < CONTINUITY_MAX_PAYLOAD_BYTES);
    }
}

//! Exact one-time legacy-domain records, never ordinary append authority.

use super::*;

const CHECKPOINT_FIELD: &str = "work_domain_actual_writes";

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

pub(in crate::runtime_db) struct ImportAuthority {
    frozen: EpisodeContinuityRoots,
    records: BTreeMap<String, ImportRecord>,
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
        let mut records = BTreeMap::<String, ImportRecord>::new();
        let mut metadata_bytes = 0usize;
        for (domain, families) in [
            ("work_items", &["work_item"][..]),
            ("wait_conditions", &["wait"][..]),
            ("messages", &["message"][..]),
            ("transcript_entries", &["transcript"][..]),
            ("evidence", &["brief", "message", "transcript"][..]),
        ] {
            let bytes: Option<usize> = tx.query_row(
                "SELECT length(CAST(source_checkpoint_json AS BLOB)) FROM storage_domains WHERE domain=?1 AND import_status='complete' AND canonical_source='db'",
                [domain], |row| row.get(0),
            ).optional()?.flatten();
            let Some(bytes) = bytes else {
                continue;
            };
            metadata_bytes = metadata_bytes.saturating_add(bytes);
            bound(metadata_bytes)?;
            let payload: String = tx.query_row(
                "SELECT source_checkpoint_json FROM storage_domains WHERE domain=?1",
                [domain],
                |row| row.get(0),
            )?;
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            let Some(footprint) = payload.get(CHECKPOINT_FIELD) else {
                continue;
            };
            let footprint: ImportCheckpoint = serde_json::from_value(footprint.clone())?;
            ensure!(
                footprint.policy_version == 1,
                "legacy Work footprint policy mismatch"
            );
            for (key, record) in footprint.records {
                let (family, _) = key
                    .split_once(':')
                    .context("invalid legacy Work footprint key")?;
                ensure!(
                    families.contains(&family),
                    "legacy Work footprint domain mismatch"
                );
                if record.agent_id != agent_id {
                    continue;
                }
                // Domain order matches the original importer: Evidence follows
                // Message/Transcript and may legitimately refine the same row.
                records.insert(key, record);
                bound(metadata_bytes)?;
            }
        }
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
        for (key, record) in &records {
            let (family, id) = key
                .split_once(':')
                .context("invalid legacy Work footprint key")?;
            if checkpoint.record_digests.contains_key(key) {
                additional.insert((family.to_owned(), id.to_owned()));
            }
            if family == "brief" {
                if let Some(work) = &record.work_item_id {
                    heads.entry(work.clone()).or_default().insert(id.to_owned());
                    if checkpoint
                        .record_digests
                        .contains_key(&format!("work_item:{work}"))
                    {
                        additional.insert(("work_item".into(), work.clone()));
                    }
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
    pub(in crate::runtime_db) fn task_query_message_imports(
        &self,
    ) -> impl Iterator<Item = (&str, &str)> {
        self.records.iter().filter_map(|(key, record)| {
            key.strip_prefix("message:")
                .map(|id| (id, record.raw_digest.as_str()))
        })
    }
    pub(super) fn original_relations(&self) -> &WorkContinuityCheckpoint {
        self.frozen
            .work_domain_checkpoint
            .as_ref()
            .expect("ImportAuthority validated its fixed Work checkpoint")
    }
    pub(super) fn permits_relation_source(&self, id: &str, digest: &str) -> bool {
        let key = format!("message:{id}");
        self.frozen
            .record_digests
            .get(&key)
            .is_some_and(|stored| stored == digest)
            || self
                .records
                .get(&key)
                .is_some_and(|record| record.raw_digest == digest)
    }
    pub(super) fn validate_relation_wait(&self, id: &str, digest: &str) -> Result<()> {
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
                || self
                    .records
                    .get(&key)
                    .is_some_and(|record| record.raw_digest == digest),
            "Wait relation has no original fixed or legacy import footprint: {id}"
        );
        Ok(())
    }
    pub(super) fn additional(&self) -> &BTreeSet<(String, String)> {
        &self.additional
    }
    pub(super) fn heads(&self, work: &str) -> BTreeSet<String> {
        self.heads.get(work).cloned().unwrap_or_default()
    }
    pub(super) fn validate_fixed_version(&self, key: &str, digest: &str) -> Result<()> {
        if let Some(original) = self.frozen.record_digests.get(key) {
            ensure!(
                original == digest
                    || self
                        .startup_wait_versions
                        .get(key)
                        .is_some_and(|stored| stored == digest)
                    || self
                        .records
                        .get(key)
                        .is_some_and(|record| record.raw_digest == digest),
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
    pub(super) fn validate_head(&self, id: &str, payload: &str) -> Result<()> {
        let key = format!("brief:{id}");
        let digest = bytes_digest(payload.as_bytes());
        ensure!(
            self.frozen.record_digests.get(&key) == Some(&digest)
                || self
                    .records
                    .get(&key)
                    .is_some_and(|record| record.raw_digest == digest),
            "Work completion head has no original import or fixed proof"
        );
        Ok(())
    }
    pub(super) fn validate_inputs(
        &self,
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
                    || self
                        .records
                        .get(key)
                        .is_some_and(|record| record.raw_digest == *digest),
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

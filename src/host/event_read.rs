//! Finite qualified historical Event reads under the original incarnation owner.
use super::*;
use crate::{
    diagnostics::resource_io::{self, ResourceReadMaterial},
    runtime_db::current_query::{prepare_event_page, PreparedEventPage},
    storage::EventLogPageOrder,
    types::AuditEvent,
};

struct EventPageMaterial {
    page: PreparedEventPage,
    material: ResourceReadMaterial,
    scope: crate::runtime::resident_input_evidence::InputEvidenceScope,
}

#[derive(Clone)]
pub(crate) struct OwnedEventPageRead(Arc<EventPageMaterial>);

#[path = "event_stream_read.rs"]
mod stream;

struct EventPageWire {
    bytes: Vec<u8>,
    _read: Arc<EventPageMaterial>,
}

impl AsRef<[u8]> for EventPageWire {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::ops::Deref for OwnedEventPageRead {
    type Target = PreparedEventPage;

    fn deref(&self) -> &Self::Target {
        &self.0.page
    }
}

impl OwnedEventPageRead {
    pub(crate) fn json_bytes(
        &self,
        value: &impl serde::Serialize,
    ) -> Result<tokio_util::bytes::Bytes> {
        self.0.material.read(|| {
            let size = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
                value,
            )?)?;
            resource_io::charge_owned_buffer(u64::try_from(size)?)?;
            resource_io::charge_owned_metadata(
                u64::try_from(std::mem::size_of::<EventPageWire>() + 128)?,
                1,
            )?;
            let mut bytes = vec![0; size];
            let mut writer = std::io::Cursor::new(bytes.as_mut_slice());
            serde_json::to_writer(&mut writer, value)?;
            anyhow::ensure!(
                usize::try_from(writer.position())? == size,
                "Event JSON size changed"
            );
            Ok(tokio_util::bytes::Bytes::from_owner(EventPageWire {
                bytes,
                _read: self.0.clone(),
            }))
        })
    }
}

impl RuntimeHost {
    /// Runs on the caller's blocking worker with its exact read scope.
    pub(crate) fn operator_event_page_scoped<F>(
        &self,
        agent_id: &str,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        limit: usize,
        order: EventLogPageOrder,
        db: &RuntimeDb,
        handle: &tokio::runtime::Handle,
        prepare_filter: impl FnOnce(&AppStorage) -> Result<F>,
    ) -> std::result::Result<OwnedEventPageRead, PublicAgentError>
    where
        F: FnMut(&AuditEvent) -> bool,
    {
        db.check_read_scope().map_err(PublicAgentError::Runtime)?;
        let identity = self.active_agent_identity(agent_id)?;
        let requested = [(identity.agent_id.clone(), identity.incarnation)];
        let reservations = handle
            .block_on(self.reserve_mutation_scopes_in(&requested, db))
            .map_err(PublicAgentError::Runtime)?;
        let result = (|| {
            db.check_read_scope()?;
            let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: identity.agent_id,
                identity_incarnation: identity.incarnation,
            };
            let material = ResourceReadMaterial::new(&self.inner.input_evidence_budget, &scope)?;
            let page = material.read(|| {
                let storage = AppStorage::open_read_only_for_agent(
                    self.agent_data_dir(agent_id),
                    agent_id.to_owned(),
                    db.clone(),
                )?;
                let mut filter = prepare_filter(&storage)?;
                db.with_read(|connection| {
                    let tx = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                    let page = prepare_event_page(
                        &tx,
                        &scope,
                        &self.inner.input_evidence_budget,
                        before_seq,
                        after_seq,
                        limit,
                        order,
                        &mut filter,
                    )?;
                    tx.commit()?;
                    Ok(page)
                })
            })?;
            db.check_read_scope()?;
            material.read(|| {
                resource_io::charge_owned_metadata(
                    u64::try_from(std::mem::size_of::<EventPageMaterial>() + 128)?,
                    1,
                )?;
                Ok(())
            })?;
            Ok(OwnedEventPageRead(Arc::new(EventPageMaterial {
                page,
                material,
                scope,
            })))
        })();
        drop(reservations);
        result.map_err(PublicAgentError::Runtime)
    }

    #[cfg(test)]
    pub(crate) async fn operator_event_page<F>(
        &self,
        agent_id: &str,
        before_seq: Option<u64>,
        after_seq: Option<u64>,
        limit: usize,
        order: EventLogPageOrder,
        prepare_filter: impl FnOnce(&AppStorage) -> Result<F>,
    ) -> std::result::Result<OwnedEventPageRead, PublicAgentError>
    where
        F: FnMut(&AuditEvent) -> bool,
    {
        let identity = self.active_agent_identity(agent_id)?;
        let reservations = self
            .reserve_mutation_scopes(&[(identity.agent_id.clone(), identity.incarnation)])
            .await
            .map_err(PublicAgentError::Runtime)?;
        let result = (|| {
            let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
                agent_id: identity.agent_id,
                identity_incarnation: identity.incarnation,
            };
            let material = ResourceReadMaterial::new(&self.inner.input_evidence_budget, &scope)?;
            let page = material.read(|| {
                let storage = self.agent_storage_read_only(agent_id)?;
                let mut filter = prepare_filter(&storage)?;
                self.runtime_db().transaction(|tx| {
                    prepare_event_page(
                        tx,
                        &scope,
                        &self.inner.input_evidence_budget,
                        before_seq,
                        after_seq,
                        limit,
                        order,
                        &mut filter,
                    )
                })
            })?;
            material.read(|| {
                resource_io::charge_owned_metadata(
                    u64::try_from(std::mem::size_of::<EventPageMaterial>() + 128)?,
                    1,
                )?;
                Ok(())
            })?;
            Ok(OwnedEventPageRead(Arc::new(EventPageMaterial {
                page,
                material,
                scope,
            })))
        })();
        drop(reservations);
        result.map_err(PublicAgentError::Runtime)
    }
}

#[cfg(test)]
#[path = "event_read_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "event_stream_http_tests.rs"]
mod stream_http_tests;

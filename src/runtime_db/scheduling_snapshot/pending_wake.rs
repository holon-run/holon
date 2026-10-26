#![cfg(test)]
//! Original writer branch observation. It grants no Scheduling member authority.
use super::*;
use crate::runtime_db::types::{TimerWakeRecord, TimerWakeStatus};
use crate::runtime_db::{types::PendingTimerWakeObservation, TimerRepository};

impl TimerRepository<'_> {
    pub(crate) fn pending_wake_for_writer(
        &self,
        scope: &InputEvidenceScope,
        timer_id: &str,
        request: &SchedulingDomainLoadRequest,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<PendingTimerWakeObservation> {
        self.db.transaction(|tx| {
            let mut material = Material::new(scope, request, ledger);
            // The separate original Timer proof retains business and membership
            // authority; this is only an exact existing branch observation.
            loader::scope_facts(tx, scope, &mut material, true)?
                .context(SchedulingSnapshotError::RetiredScope)?;
            let (count, bytes): (usize, usize) = tx.query_row(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(timer_id AS BLOB))+length(CAST(message_id AS BLOB))+length(CAST(fire_count AS BLOB))+length(CAST(status AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+COALESCE(length(CAST(incorporated_at AS BLOB)),0)+COALESCE(length(CAST(cancelled_at AS BLOB)),0)+128),0) FROM timer_wakes WHERE timer_id=?1 AND status='pending'",
                [timer_id], |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            ensure!(count<=1,"Timer has multiple pending wakes");
            material.charge(bytes.checked_add(timer_id.len()+128).context("pending Timer selector overflow")?, count+1)?;
            let owned: bool = tx.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM timers WHERE timer_id=?1 AND agent_id!=?2) AND (?3=0 OR EXISTS(SELECT 1 FROM timers WHERE timer_id=?1 AND agent_id=?2))",
                params![timer_id,scope.agent_id,count], |row|row.get(0),
            )?;
            ensure!(owned,"pending Timer owner differs from original scope");
            let row = tx.query_row(
                "SELECT timer_id,message_id,fire_count,created_at,updated_at,incorporated_at,cancelled_at FROM timer_wakes WHERE timer_id=?1 AND status='pending'",
                [timer_id], |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?)),
            ).optional()?;
            let record = row.map(|(timer,message,fire,created,updated,incorporated,cancelled)| -> Result<_> {
                ensure!(timer==timer_id,"pending Timer address changed in original view");
                Ok(TimerWakeRecord {timer_id:timer,message_id:message,fire_count:u64::try_from(fire).context("timer wake fire_count is negative")?,status:TimerWakeStatus::Pending,
                    created_at:repositories::parse_timestamp(&created)?,updated_at:repositories::parse_timestamp(&updated)?,
                    incorporated_at:incorporated.map(|value|repositories::parse_timestamp(&value)).transpose()?,cancelled_at:cancelled.map(|value|repositories::parse_timestamp(&value)).transpose()?})
            }).transpose()?;
            // A single retained guard carries all earlier admitted tuple/scope
            // material. The original guards overlap until this handoff ends.
            let guard=ledger.register_owned_bytes(scope,InputMemoryCategory::Candidate,OwnedBytesEstimate {payload_bytes:u64::try_from(material.bytes)?,metadata_estimate_bytes:0})?;
            Ok(PendingTimerWakeObservation {record,_material:guard})
        })
    }
}

//! Closed written-row sizing and decoding have separate actual lifetimes.
use super::*;

fn quoted_text_bound(value: &[u8]) -> Result<usize> {
    value.iter().try_fold(2usize, |n, byte| {
        let escaped = match byte {
            b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
            0..=31 => 6,
            _ => 1,
        };
        n.checked_add(escaped).context("Task projection text overflow")
    })
}
impl TaskReadMaterial<'_> {
    pub(in crate::runtime_db::task_snapshot) fn original_written_columns_tx(
        &mut self, tx:&Transaction<'_>, agent:&str, key:&TaskKey,
    ) -> Result<Option<(usize,usize,usize)>> {
        let (owner_sql, raw_sql, _, fields)=stored_material_sql(key.family);
        self.charge_record(key,0)?;
        let owner:Option<bool>=tx.query_row(owner_sql,params![key.id,agent],|row|row.get(0)).optional()?;
        let Some(owner)=owner else {return Ok(None)};
        ensure!(owner,TaskSnapshotError::ForeignRecord(key.clone()));
        let (payload,normalized):(usize,usize)=tx.query_row(raw_sql,[&key.id],|row|Ok((row.get(0)?,row.get(1)?)))?;
        let fixed=fields.checked_mul(128).and_then(|n|n.checked_add(4096)).context("Task written scalar slots overflow")?;
        // These are SQLite's normalized cells, not owned Rust Strings/Values.
        self.charge(payload.checked_add(normalized).and_then(|n|n.checked_add(fixed))
            .context("Task written columns overflow")?,fields+1,1,false)?;
        let projection=projection_sql(key.family);
        let columns=projection.strip_prefix("json_array(").and_then(|s|s.strip_suffix(')'))
            .context("Task original projection shape changed")?;
        let (table,id,_)=family_table(key.family);
        let escaped=tx.query_row(&format!("SELECT {columns} FROM {table} WHERE {id}=?1"),[&key.id],|row| {
            let mut bytes=fields.checked_add(2).ok_or(rusqlite::Error::InvalidQuery)?;
            for column in 0..fields {
                let n=match row.get_ref(column)? {
                    rusqlite::types::ValueRef::Text(value)=>quoted_text_bound(value)
                        .map_err(|error|rusqlite::Error::FromSqlConversionFailure(column,rusqlite::types::Type::Text,error.into()))?,
                    // json_array will preserve its original BLOB error below;
                    // do not decode or reinterpret an opaque normalized cell.
                    rusqlite::types::ValueRef::Blob(value)=>value.len().checked_mul(6)
                        .and_then(|n|n.checked_add(64)).ok_or(rusqlite::Error::InvalidQuery)?,
                    _=>64, // full i64/finite/nonfinite REAL/NULL SQL spelling
                };
                bytes=bytes.checked_add(n).ok_or(rusqlite::Error::InvalidQuery)?;
            }
            Ok(bytes)
        })?;
        // Bundled jsonStringGrow may realloc/move: old and new capacity
        // coexist, <=3*(escaped+raw tail+fixed). Raw cells remain separate.
        // Original json_array sizing has no Rust projection String yet.
        #[cfg(test)]
        if escaped.checked_add(normalized).and_then(|n|n.checked_add(fixed))
            .and_then(|n|n.checked_mul(3)).and_then(|n|self.bytes.checked_add(n))
            .is_none_or(|n|n>self.request.byte_limit) {
            eprintln!("Task written sizing actual phase: prior=({}, {}) limit=({}, {}) payload={} normalized={} escaped={} fixed={} fields={} guards={}",
                self.bytes,self.records,self.request.byte_limit,self.request.record_limit,
                payload,normalized,escaped,fixed,fields,self.guards.len());
        }
        self.charge(escaped.checked_add(normalized).and_then(|n|n.checked_add(fixed))
            .and_then(|n|n.checked_mul(3)).context("Task written projection workspace overflow")?,fields+1,1,false)?;
        let exact=checkpoint_material_size_on(tx,key)?.context("Task written body disappeared")?;
        let projection=exact.checked_sub(payload).context("Task written projection length underflow")?;
        ensure!(projection<=escaped,"Task actual SQL projection exceeded its borrowed column bound");
        Ok(Some((payload,normalized,projection)))
    }
    pub(in crate::runtime_db::task_snapshot) fn admit_original_written_decode_tx(
        &mut self, tx:&Transaction<'_>, key:&TaskKey,
        columns:(usize,usize,usize),
    ) -> Result<()> {
        let (payload,normalized,projection)=columns;
        let (_,_,native_sql,fields)=stored_material_sql(key.family);
        self.charge_record(key,0)?;
        // Raw + typed + expected/temporary canonical fields: 4P. This
        // does not assume normalized cells still match canonical after a trigger.
        // Normalized owner/cell/realloc tails: 5N. SQLite growth/Rust copy: 3E.
        // The parsed projection stage has less live byte material than this bound.
        let bytes=payload.checked_mul(4)
            .and_then(|n|normalized.checked_mul(5).and_then(|m|n.checked_add(m)))
            .and_then(|n|projection.checked_mul(3).and_then(|m|n.checked_add(m)))
            .and_then(|n|fields.checked_mul(128).and_then(|m|n.checked_add(m)))
            .and_then(|n|n.checked_add(4096)).context("Task written decoder envelope overflow")?;
        self.charge(bytes,0,1,false)?;
        // Keep the original complete native bound, independently of raw bytes.
        self.charge(0,fields+1,8,false)?;
        let native:usize=tx.query_row(native_sql,[&key.id],|row|row.get(0))?;
        self.charge(0,native,8,false)
    }
}

#[cfg(test)]
#[path = "written_row_material_tests.rs"]
mod tests;

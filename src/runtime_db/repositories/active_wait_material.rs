use super::*;
use crate::diagnostics::resource_io::{admit_json_payload, charge_owned_metadata};
use serde::{de::value::BorrowedStrDeserializer, Deserialize};

// The caller's existing snapshot holder covers these records through consumption.
pub(super) fn active_wait_records(
    statement: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<WaitConditionRecord>> {
    let mut rows = statement.query(params)?;
    let mut records = Vec::<WaitConditionRecord>::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| anyhow::anyhow!("reading wait conditions: {error}"))?
    {
        if records.len() == records.capacity() {
            let additional = records.capacity().max(4);
            charge_owned_metadata(
                u64::try_from(
                    additional
                        .checked_mul(std::mem::size_of::<WaitConditionRecord>())
                        .context("active wait capacity overflow")?,
                )?,
                additional,
            )?;
            records.try_reserve_exact(additional)?;
        }
        let record = decode_active_wait_row(row)?;
        records.push(record);
    }
    Ok(records)
}

fn sql_text<'row>(row: &'row rusqlite::Row<'_>, column: usize) -> rusqlite::Result<&'row str> {
    let value = row.get_ref(column)?;
    let column_name = row.as_ref().column_name(column)?;
    value.as_str().map_err(|error| match error {
        rusqlite::types::FromSqlError::InvalidType => {
            rusqlite::Error::InvalidColumnType(column, column_name.to_owned(), value.data_type())
        }
        rusqlite::types::FromSqlError::Other(error) => {
            rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), error)
        }
        error => {
            rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
        }
    })
}

fn optional_sql_text<'row>(
    row: &'row rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<&'row str>> {
    if row.get_ref(column)?.data_type() == rusqlite::types::Type::Null {
        Ok(None)
    } else {
        sql_text(row, column).map(Some)
    }
}

fn owned_text(text: &str) -> Result<String> {
    charge_owned_metadata(
        u64::try_from(
            text.len()
                .checked_add(std::mem::size_of::<String>())
                .context("active wait text size overflow")?,
        )?,
        1,
    )?;
    Ok(text.to_owned())
}

// Keep legacy SQL/parse diagnostics without flattening new admission errors.
fn legacy_decode_error(error: impl Into<anyhow::Error>) -> anyhow::Error {
    anyhow::anyhow!(
        "reading wait conditions: {}",
        wait_condition_decode_error(error.into())
    )
}

fn decode_active_wait_row(row: &rusqlite::Row<'_>) -> Result<WaitConditionRecord> {
    // Validate SQL columns in the same order as the ordinary row decoder.
    let id = sql_text(row, 0).map_err(legacy_decode_error)?;
    let agent_id = sql_text(row, 1).map_err(legacy_decode_error)?;
    let work_item_id = optional_sql_text(row, 2).map_err(legacy_decode_error)?;
    let status_str = sql_text(row, 3).map_err(legacy_decode_error)?;
    let kind_str = sql_text(row, 4).map_err(legacy_decode_error)?;
    let source = optional_sql_text(row, 5).map_err(legacy_decode_error)?;
    let subject_ref = optional_sql_text(row, 6).map_err(legacy_decode_error)?;
    let waiting_for = sql_text(row, 7).map_err(legacy_decode_error)?;
    let created_at_str = sql_text(row, 8).map_err(legacy_decode_error)?;
    let updated_at_str = sql_text(row, 9).map_err(legacy_decode_error)?;
    let expires_at_str = optional_sql_text(row, 10).map_err(legacy_decode_error)?;
    let resolved_at_str = optional_sql_text(row, 11).map_err(legacy_decode_error)?;
    let cancelled_at_str = optional_sql_text(row, 12).map_err(legacy_decode_error)?;
    let turn_id = optional_sql_text(row, 13).map_err(legacy_decode_error)?;
    let trigger_message_id = optional_sql_text(row, 14).map_err(legacy_decode_error)?;
    let triggered_at_str = optional_sql_text(row, 15).map_err(legacy_decode_error)?;
    let wake_sources_json = sql_text(row, 16).map_err(legacy_decode_error)?;
    let continuation_json = optional_sql_text(row, 17).map_err(legacy_decode_error)?;

    let id = owned_text(id)?;
    let agent_id = owned_text(agent_id)?;
    let work_item_id = work_item_id.map(owned_text).transpose()?;
    let source = source.map(owned_text).transpose()?;
    let subject_ref = subject_ref.map(owned_text).transpose()?;
    let waiting_for = owned_text(waiting_for)?;
    let turn_id = turn_id.map(owned_text).transpose()?;
    let trigger_message_id = trigger_message_id.map(owned_text).transpose()?;
    let status = WaitConditionStatus::deserialize(
        BorrowedStrDeserializer::<serde_json::Error>::new(status_str),
    )
    .context("parsing wait condition status")
    .map_err(legacy_decode_error)?;
    let kind =
        WaitConditionKind::deserialize(BorrowedStrDeserializer::<serde_json::Error>::new(kind_str))
            .context("parsing wait condition kind")
            .map_err(legacy_decode_error)?;
    admit_json_payload(wake_sources_json)?;
    let wake_sources = serde_json::from_str(wake_sources_json)
        .context("parsing wake_sources_json")
        .map_err(legacy_decode_error)?;
    let continuation = continuation_json
        .map(|payload| {
            admit_json_payload(payload)?;
            serde_json::from_str(payload)
                .context("parsing continuation_json")
                .map_err(legacy_decode_error)
        })
        .transpose()?;
    Ok(WaitConditionRecord {
        id,
        agent_id,
        work_item_id,
        status,
        kind,
        source,
        subject_ref,
        waiting_for,
        wake_sources,
        continuation,
        created_at: parse_timestamp(created_at_str).map_err(legacy_decode_error)?,
        updated_at: parse_timestamp(updated_at_str).map_err(legacy_decode_error)?,
        expires_at: parse_optional_timestamp(expires_at_str).map_err(legacy_decode_error)?,
        resolved_at: parse_optional_timestamp(resolved_at_str).map_err(legacy_decode_error)?,
        cancelled_at: parse_optional_timestamp(cancelled_at_str).map_err(legacy_decode_error)?,
        turn_id,
        trigger_message_id,
        triggered_at: parse_optional_timestamp(triggered_at_str).map_err(legacy_decode_error)?,
    })
}

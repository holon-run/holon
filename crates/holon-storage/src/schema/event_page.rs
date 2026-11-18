//! Indexed selection is an optimization, never authority to decode an Event.
macro_rules! event_page_kind_key {
    () => {
        r#"CASE WHEN json_valid(data_json) THEN
          CASE WHEN json_type(data_json,'$.kind')='text'
            AND json_extract(data_json,'$.kind')=kind
            AND json_type(data_json,'$.id')='text'
            AND json_extract(data_json,'$.id')=audit_event_id
            AND json_type(data_json,'$.event_seq')='integer'
            AND json_extract(data_json,'$.event_seq')=event_seq
            AND instr(data_json,'\u')=0
            AND json_remove(data_json,
              '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
              '$.contract_version','$.payload_schema','$.payload_schema_version','$.data')
              =json_remove(json_remove(data_json,
                '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
                '$.contract_version','$.payload_schema','$.payload_schema_version','$.data'),
                '$.id','$.event_seq','$.event_log_epoch','$.created_at','$.kind',
                '$.contract_version','$.payload_schema','$.payload_schema_version','$.data')
          THEN kind END
        END"#
    };
}

// SQLite accepts duplicate fields that AuditEvent's serde decoder rejects.
// Removing known fields twice must be idempotent; escaped keys conservatively
// use fallback because SQLite does not resolve them like serde does.
// NULL is the disjoint fallback stream: legacy/mismatched/malformed records
// still go through the canonical decoder and filter, not a normalized shortcut.
pub const EVENT_PAGE_KIND_KEY: &str = event_page_kind_key!();
pub const EVENT_PAGE_KIND_INDEX: &str = concat!(
    "CREATE INDEX IF NOT EXISTS idx_audit_events_agent_canonical_kind_seq \
     ON audit_events(agent_id,(",
    event_page_kind_key!(),
    "),event_seq,created_at);"
);

#[cfg(test)]
mod tests;

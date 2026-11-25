use super::*;

#[test]
fn aggregate_keeps_the_worst_freshness_and_sums_exact_pending_rows() {
    let observed = || IndexMaintenanceStatus {
        has_stale_projection: false,
        has_pending_sources: false,
        lacks_full_backfill: false,
        is_dirty: false,
        rebuild_job: None,
    };
    let make = |index_exists, cursor, high_watermark, pending_count, oldest_pending_age_ms| {
        observed().into_search_status(
            index_exists,
            Some(IndexOutboxStatus {
                cursor,
                high_watermark,
                pending_count,
                oldest_pending_age_ms: Some(oldest_pending_age_ms),
                index_exists,
                last_indexed_at: None,
            }),
            true,
            0,
        )
    };
    let statuses = BTreeMap::from([
        ("alpha".into(), make(true, 10, 100, 2, 20)),
        ("beta".into(), make(false, 20, 21, 1, 400)),
    ]);
    let status = aggregate_index_status(&statuses);
    assert_eq!(status.freshness, "missing");
    assert_eq!(
        (status.cursor, status.high_watermark, status.lag),
        (20, 100, 90)
    );
    assert_eq!(status.pending_count, 3);
    assert_eq!(status.oldest_pending_age_ms, Some(400));
    assert_eq!(status.stale_reasons, ["index_missing", "outbox_lag"]);
    assert!(status.indexing_needed && status.results_may_be_incomplete);
    assert!(status.consumption_was_limited);
}

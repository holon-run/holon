use super::*;

fn entry(i: usize, size: usize) -> QuarantineEntry {
    QuarantineEntry {
        agent_id: format!("agent_{i}_{}", "a".repeat(size)),
        incarnation: 1,
        generation: 3,
        stage: QuarantineStage::StorageConstructor,
        reason: QuarantineReason::ProjectionChanged,
        failed_at: Utc::now(),
    }
}

#[test]
fn quarantine_report_replaces_current_and_keeps_bounded_boot_history() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(FILE_NAME);
    assert!(matches!(
        read_summary(&path),
        Err(QuarantineReportUnavailable::Missing)
    ));
    let entries: Vec<_> = (0..36).map(|i| entry(i, 300)).collect();
    let time = Utc::now();
    for boot in 0..4 {
        persist_quarantine_boot(
            dir.path(),
            QuarantineBoot::new(&format!("startup_{boot}"), time, &entries),
        )?;
    }
    let report = read_summary(&path).unwrap();
    assert_eq!(report.current.entries.len(), 32);
    assert_eq!(report.current.omitted_entries, 4);
    assert_eq!(report.history.len(), 2);
    assert_eq!(report.history[0].startup_id, "startup_2");
    assert_eq!(report.history[1].startup_id, "startup_1");
    assert!(report.history[1].omitted_entries > 4);
    assert!(fs::metadata(&path)?.len() <= MAX_WRITE_BYTES as u64);

    persist_quarantine_boot(
        dir.path(),
        QuarantineBoot::new("startup_3", time, std::iter::empty()),
    )?;
    let report = read_summary(&path).unwrap();
    assert!(report.current.entries.is_empty());
    assert_eq!(report.history[0].startup_id, "startup_2");
    assert_eq!(report.current.started_at, time);
    Ok(())
}

#[test]
fn quarantine_report_rejects_bad_metadata_without_echoing_it() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(FILE_NAME);
    let mut entries = vec![entry(0, 1000), entry(1, 0)];
    entries[1].agent_id = "not\na\nsafe\nlabel".into();
    persist_quarantine_boot(
        dir.path(),
        QuarantineBoot::new("startup_redaction", Utc::now(), &entries),
    )?;
    let report = read_summary(&path).unwrap();
    assert!(report.current.entries.is_empty());
    assert_eq!(report.current.omitted_entries, 2);
    fs::write(&path, b"invalid report")?;
    assert!(matches!(
        read_summary(&path),
        Err(QuarantineReportUnavailable::Invalid)
    ));
    fs::write(&path, vec![b' '; MAX_READ_BYTES as usize + 1])?;
    assert!(matches!(
        read_summary(&path),
        Err(QuarantineReportUnavailable::Oversized)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(matches!(
            read_summary(&path),
            Err(QuarantineReportUnavailable::NotPrivateRegularFile)
        ));
    }
    Ok(())
}

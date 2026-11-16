use super::*;
use std::{io::Read, sync::mpsc, thread, time::Duration};

#[test]
fn transaction_keeps_one_lock_across_atomic_replacements() {
    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    let transaction = PersistedConfigTransaction::begin(&path).unwrap();
    let mut config = transaction.load().unwrap();
    config.model.default = Some("before".into());
    transaction.save(&config).unwrap();
    let mut old_file = fs::File::open(&path).unwrap();
    let lock_probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("json.lock"))
        .unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&lock_probe).is_err());
    config.model.default = Some("after".into());
    transaction.save(&config).unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&lock_probe).is_err());
    let mut old_content = String::new();
    old_file.read_to_string(&mut old_content).unwrap();
    assert_eq!(
        serde_json::from_str::<HolonConfigFile>(&old_content)
            .unwrap()
            .model
            .default
            .as_deref(),
        Some("before")
    );
    assert_eq!(
        transaction.load().unwrap().model.default.as_deref(),
        Some("after")
    );
    drop(transaction);
    fs2::FileExt::try_lock_exclusive(&lock_probe).unwrap();
}

#[test]
fn waiting_transaction_loads_the_preceding_commit() {
    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    let first = PersistedConfigTransaction::begin(&path).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (loaded_tx, loaded_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        started_tx.send(()).unwrap();
        let transaction = PersistedConfigTransaction::begin(&path).unwrap();
        let mut config = transaction.load().unwrap();
        loaded_tx.send(config.model.default.clone()).unwrap();
        config.vision.default = Some("second".into());
        transaction.save(&config).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(loaded_rx.recv_timeout(Duration::from_millis(100)).is_err());
    let mut config = first.load().unwrap();
    config.model.default = Some("first".into());
    first.save(&config).unwrap();
    drop(first);
    assert_eq!(
        loaded_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Some("first".into())
    );
    second.join().unwrap();
    let stored = load_persisted_config_at(&persisted_config_path(home.path())).unwrap();
    assert_eq!(stored.model.default.as_deref(), Some("first"));
    assert_eq!(stored.vision.default.as_deref(), Some("second"));
}

#[test]
fn failed_replace_preserves_destination_and_removes_staging_file() {
    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    fs::create_dir(&path).unwrap();
    let marker = path.join("keep");
    fs::write(&marker, "unchanged").unwrap();
    let transaction = PersistedConfigTransaction::begin(&path).unwrap();
    assert!(transaction.save(&HolonConfigFile::default()).is_err());
    assert_eq!(fs::read_to_string(marker).unwrap(), "unchanged");
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 2);
}

#[test]
fn dropping_without_save_preserves_config_and_releases_lock() {
    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    save_persisted_config_at(&path, &HolonConfigFile::default()).unwrap();
    let original = fs::read(&path).unwrap();
    {
        let transaction = PersistedConfigTransaction::begin(&path).unwrap();
        let mut config = transaction.load().unwrap();
        config.model.default = Some("not-saved".into());
    }
    let _transaction = PersistedConfigTransaction::begin(&path).unwrap();
    assert_eq!(fs::read(path).unwrap(), original);
}

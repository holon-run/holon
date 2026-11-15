#[cfg(test)]
pub(crate) use holon_storage::write_queue::RuntimeDbWriterState;
pub(crate) use holon_storage::write_queue::{
    begin_capability_policy_write, record_audit_insert, RuntimeDbWriteContext, RuntimeDbWriter,
};

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod persistence_tests {
    use std::time::Duration;

    use anyhow::Result;

    #[test]
    fn preparation_read_returns_the_lease_after_callback_failure() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut db = crate::runtime_db::RuntimeDb::open_and_migrate(
            temp.path().join("runtime.sqlite"),
            temp.path().join("runtime.lock"),
        )?;
        db.configure_reader_pool_for_tests(1, Duration::from_millis(250))?;
        let error = db
            .read_for_preparation::<()>(|_| anyhow::bail!("preparation read failed"))
            .expect_err("callback failure must propagate");
        assert_eq!(error.to_string(), "preparation read failed");
        db.read_for_preparation(|tx| {
            assert!(!tx.is_autocommit());
            Ok(tx.query_row("SELECT 7", [], |row| row.get::<_, i64>(0))?)
        })
        .map(|value| assert_eq!(value, 7))?;
        assert_eq!(db.preparation_pool_stats_for_tests().connections_opened, 1);
        Ok(())
    }
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod async_worker_tests {
    use std::{
        sync::{mpsc, Arc},
        thread,
        time::Duration,
    };

    use anyhow::{Context, Result};

    use super::RuntimeDbWriteContext;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn borrowed_sql_callbacks_keep_their_thread_and_leave_the_async_worker_available(
    ) -> Result<()> {
        for mode in [
            "pooled",
            "preparation",
            "recovery",
            "append",
            "context",
            "once",
        ] {
            let home = tempfile::tempdir()?;
            let db = crate::runtime_db::RuntimeDb::open_and_migrate(
                home.path().join("runtime.sqlite"),
                home.path().join("runtime.lock"),
            )?;
            let writer = db.writer.clone();
            let entered = Arc::new(tokio::sync::Notify::new());
            let observed = entered.clone();
            let (release, wait) = mpsc::channel();
            let callback = tokio::spawn(async move {
                let original_thread = thread::current().id();
                let original_scope = std::cell::Cell::new(7);
                let borrowed = &original_scope;
                let progress = || -> Result<()> {
                    assert_eq!(thread::current().id(), original_thread);
                    assert_eq!(borrowed.get(), 7);
                    observed.notify_one();
                    wait.recv_timeout(Duration::from_secs(3))
                        .context("async probe could not progress while SQL callback was waiting")?;
                    borrowed.set(8);
                    Ok(())
                };
                match mode {
                    "pooled" => db.with_read(|_| progress()),
                    "preparation" => db.read_for_preparation(|_| progress()),
                    "recovery" => db.recovery_transaction_once(|_| progress()),
                    "append" => writer.append_wait(|_| progress()),
                    "context" => writer.append_wait_with_context(
                        RuntimeDbWriteContext::sync("worker_probe", "probe"),
                        |_| progress(),
                    ),
                    "once" => writer.append_wait_once(|_| progress()),
                    _ => unreachable!(),
                }?;
                assert_eq!(original_scope.get(), 8);
                Ok::<_, anyhow::Error>(())
            });
            let probe = tokio::spawn(async move {
                entered.notified().await;
                release
                    .send(())
                    .context("original callback ended before async probe ran")
            });
            probe.await??;
            callback.await??;
        }
        Ok(())
    }
}

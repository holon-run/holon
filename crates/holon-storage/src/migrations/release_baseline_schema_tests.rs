use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Barrier;

#[test]
fn concurrent_initialization_builds_one_immutable_schema() {
    let cache = ReleaseBaselineSchema::new();
    let builds = AtomicUsize::new(0);
    let barrier = Barrier::new(16);
    std::thread::scope(|scope| {
        let handles = (0..16)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    cache
                        .get_or_build(|| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            Ok(vec!["CREATE TABLE baseline (id INTEGER);".to_owned()])
                        })
                        .expect("schema initialization")
                })
            })
            .collect::<Vec<_>>();
        let schemas = handles
            .into_iter()
            .map(|handle| handle.join().expect("initializer thread"))
            .collect::<Vec<_>>();
        for schema in &schemas {
            assert!(std::ptr::eq(*schema, schemas[0]));
            assert_eq!(schema[0], "CREATE TABLE baseline (id INTEGER);");
        }
    });
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_initialization_is_retryable_and_success_is_reused() -> Result<()> {
    let cache = ReleaseBaselineSchema::new();
    let error = cache
        .get_or_build(|| bail!("scratch migration failed"))
        .expect_err("failed schema must not be published");
    assert!(error.to_string().contains("scratch migration failed"));
    assert!(cache.schema.get().is_none());

    let schema =
        cache.get_or_build(|| Ok(vec!["CREATE TABLE retried (id INTEGER);".to_owned()]))?;
    let reused = cache.get_or_build(|| panic!("successful schema must not be rebuilt"))?;
    assert!(std::ptr::eq(schema, reused));
    assert_eq!(schema[0], "CREATE TABLE retried (id INTEGER);");
    Ok(())
}

#[test]
fn shared_schema_matches_uncached_migration_derivation() -> Result<()> {
    let shared = release_baseline_final_schema()?;
    assert!(!shared.is_empty());
    assert_eq!(shared, build_release_baseline_final_schema()?);
    assert!(std::ptr::eq(shared, release_baseline_final_schema()?));
    Ok(())
}

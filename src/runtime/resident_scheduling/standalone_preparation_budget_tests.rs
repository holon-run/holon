use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        tests::support::{context_config, tempdir, StubProvider},
        RuntimeHandle,
    },
    types::WorkItemExecutionBinding,
};
use std::{future::Future, task::Poll};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "preparation-budget",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "preparation-budget".into(),
        context_config(),
    )
}

fn binding(bytes: usize) -> WorkItemExecutionBinding {
    WorkItemExecutionBinding {
        source_message_id: "x".repeat(bytes),
        turn_id: "turn".into(),
        work_item_id: None,
        activation_id: None,
        admission_provenance: None,
        owner: None,
        claimed_work_revision: None,
    }
}

#[tokio::test]
async fn standalone_preparation_gates_constructor_before_private_candidate() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let mut owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    owner.state.current_execution_binding = Some(binding(3 * 1024 * 1024));
    let error = owner
        .prepare_owner_scheduling_request(&SchedulingCaptureRequest::current())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"), "{error:#}");
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(published.token, root.token);
    assert_eq!(published.state, root.state);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    Ok(())
}

#[tokio::test]
async fn standalone_preparation_reuses_active_remainder_and_failed_phase() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let mut owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    owner.state.current_execution_binding = Some(binding(4096));
    let capture = SchedulingCaptureRequest::current();
    for bytes_bound in [false, true] {
        let material = ResourceReadMaterial::with_test_limits(
            &runtime.inner.input_evidence_budget,
            &owner.input_evidence_scope(),
            if bytes_bound {
                20_000
            } else {
                64 * 1024 * 1024
            },
            if bytes_bound { 65536 } else { 64 },
        )?;
        let guard = material.guard()?;
        let weak = Arc::downgrade(&guard);
        {
            let _entry = material.enter();
            for _ in 0..2 {
                // Poll only through synchronous rejection; never carry TLS
                // through an await or leave a suspended future in the block.
                let mut future = owner.prepare_owner_scheduling_request(&capture);
                let Poll::Ready(result) = future
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                else {
                    panic!("constructor rejection must not suspend");
                };
                let error = result.unwrap_err();
                assert!(
                    error
                        .to_string()
                        .to_ascii_lowercase()
                        .contains("resourceexhausted"),
                    "{error:#}"
                );
            }
            assert!(material.finish().is_err());
            assert!(ResourceReadMaterial::active_guard().is_err());
        }
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let published = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(published.token, root.token);
        assert_eq!(published.state, root.state);
        drop((material, guard));
        assert!(weak.upgrade().is_none());
    }
    // The unbounded-by-test independent entry still reaches the original
    // candidate check; the failed caller material did not leak into TLS.
    let error = owner
        .prepare_owner_scheduling_request(&capture)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot publish a private candidate"),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn standalone_preparation_installs_original_phase_until_last_capture_holder() -> Result<()> {
    for reuse in [false, true] {
        let home = tempdir()?;
        let runtime = runtime(home.path())?;
        runtime.join_resource_worker().await?;
        let mut owner = runtime.begin_mutation().await?;
        let before = owner.capture_published_root()?;
        // The initial current domain is already prepared. An unseen exact key
        // forces the real SQL preparation and resident installation path.
        let key = SchedulingKey::new(SchedulingFamily::Timer, "unprepared-exact-timer");
        let mut capture = SchedulingCaptureRequest::current();
        capture.required_keys.insert(key.clone());
        assert!(before
            .scheduling
            .as_ref()
            .unwrap()
            .capture(&capture)
            .is_err());
        let material = reuse
            .then(|| {
                ResourceReadMaterial::new(
                    &runtime.inner.input_evidence_budget,
                    &owner.input_evidence_scope(),
                )
            })
            .transpose()?;
        {
            let mut future = owner.prepare_owner_scheduling_request(&capture);
            let first = {
                let _entry = material.as_ref().map(|material| material.enter());
                future
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            };
            // If installation suspends, resume without the caller TLS entry.
            match first {
                Poll::Ready(result) => result?,
                Poll::Pending => future.await?,
            }
        }
        let installed = owner.capture_published_root()?;
        assert_ne!(installed.token, before.token);
        assert_eq!(installed.state, before.state);
        let domain = installed.scheduling.as_ref().unwrap();
        let source = domain.context_material.as_ref().unwrap();
        if let Some(material) = &material {
            assert!(Arc::ptr_eq(source, &material.guard()?));
        }
        let weak = Arc::downgrade(source);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let read = Arc::new(owner.capture_scheduling_read(&capture)?);
        assert_eq!(read.token, installed.token);
        assert!(read.scheduling.lookup(&key)?.is_none());
        assert!(read
            .scheduling
            ._views
            .iter()
            .any(|view| Arc::ptr_eq(view, source)));
        let last = read.clone();
        drop((read, installed, before, owner, runtime, material));
        assert!(weak.upgrade().is_some());
        drop(last);
        assert!(weak.upgrade().is_none());
    }
    Ok(())
}

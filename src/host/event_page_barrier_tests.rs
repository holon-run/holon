use super::*;
use crate::runtime_db::current_query::EventPageScan;
use std::{future::Future, task::Poll, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_page_preserves_owner_barrier_and_cancelled_read_releases_it() -> Result<()> {
    for cancel in [false, true] {
        let (_home, host) = fixture().await?;
        let identity = host.active_agent_identity("default")?;
        let storage = host.operator_agent_read_storage("default")?;
        let reader_host = host.clone();
        let handle = tokio::runtime::Handle::current();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let reader = tokio::spawn(async move {
            storage
                .read_block("test.events.owner_barrier", move |storage| {
                    let scope = crate::runtime_db::read_block::current_scope()
                        .context("registered blocking read scope")?;
                    let db = storage
                        .runtime_db()?
                        .context("runtime database")?
                        .with_read_scope(scope.clone());
                    let mut entered = Some(entered);
                    Ok(reader_host.operator_event_page_scoped(
                        "default",
                        None,
                        None,
                        1,
                        EventLogPageOrder::Asc,
                        Some(EventPageScan { kind: None }),
                        &db,
                        &handle,
                        |_| {
                            Ok(move |_: &AuditEvent| {
                                if let Some(entered) = entered.take() {
                                    entered.send(scope.clone()).expect("reader scope receiver");
                                    released
                                        .recv_timeout(Duration::from_secs(30))
                                        .expect("release owner barrier");
                                }
                                true
                            })
                        },
                    ))
                })
                .await
        });
        let scope = ready.await?;
        let started = std::time::Instant::now();
        let writer = async {
            let reservations = host
                .reserve_mutation_scopes(&[(identity.agent_id.clone(), identity.incarnation)])
                .await?;
            let event = AuditEvent::legacy("short-operation", serde_json::json!({}));
            host.runtime_db()
                .audit_events()
                .append(Some("default"), &event)?;
            drop(reservations);
            Ok::<_, anyhow::Error>(())
        };
        tokio::pin!(writer);
        // A real same-incarnation writer must not enter while preparation is
        // paused. No sleep or elapsed-time threshold stands in for this barrier.
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(writer.as_mut().poll(cx).is_pending())).await
        );
        if cancel {
            scope.cancel();
        }
        release.send(())?;
        let result = reader.await??;
        if cancel {
            assert!(
                result.is_err(),
                "cancelled preparation must not publish a page"
            );
        } else {
            let page = result?;
            assert_eq!(page.events().len(), 1);
            assert_eq!(page.next_after_seq, Some(1));
            assert_eq!(page.costs.decoded, 1);
            drop(page);
        }
        writer.await?;
        eprintln!(
            "Event-page owner barrier cancel={cancel} observed_wait={:?}",
            started.elapsed()
        );
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        // The cancelled reader and owner reservation must both be reusable.
        let page = host
            .operator_event_page("default", None, None, 1, EventLogPageOrder::Asc, |_| {
                Ok(|_: &AuditEvent| true)
            })
            .await?;
        assert_eq!(page.events().len(), 1);
        drop(page);
        host.shutdown().await?;
    }
    Ok(())
}

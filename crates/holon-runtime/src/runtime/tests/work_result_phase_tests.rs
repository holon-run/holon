//! A later independent Result phase cannot discard an already-ACKed Work event.
use super::*;

#[tokio::test]
async fn work_effects_are_published_before_independent_result_wake_receipt_failure() {
    Box::pin(async {
        for pick in [true,false] {
            let home=tempdir().unwrap();let workspace=tempdir().unwrap();
            let runtime=RuntimeHandle::new("default",home.path().to_path_buf(),workspace.path().to_path_buf(),
                "http://127.0.0.1:7878".into(),Arc::new(StubProvider::new("unused")),"default".into(),context_config()).unwrap();
            let work=runtime.create_work_item("original Work before Result phase".into(),None,None,Vec::new()).await.unwrap();
            runtime.pick_work_item(work.id.clone()).await.unwrap();
            if pick {
                runtime.register_wait_for("default",Some(work.id.clone()),WaitForWakeKind::External,
                    Some("test:result-phase".into()),"original blocker".into(),Some(60_000)).await.unwrap();
            } else {
                runtime.update_work_item_fields(work.id.clone(),None,None,None,None,Some(Some("original blocker".into()))).await.unwrap();
            }
            runtime.storage().enable_event_bus(crate::storage::EventBus::new(32)).unwrap();
            let mut events=runtime.storage().subscribe_events().unwrap().unwrap();
            runtime.inner.runtime_db.connection().unwrap().execute_batch(
                "CREATE TRIGGER reject_result_phase BEFORE INSERT ON input_mutation_receipts WHEN NEW.family='task_result' BEGIN SELECT RAISE(ABORT,'later-result-receipt-fault'); END;"
            ).unwrap();
            let error=if pick {
                runtime.pick_work_item_with_reason_and_clear_blocker(work.id.clone(),Some("inspected original wait".into()),true).await.err().unwrap()
            } else {
                runtime.update_work_item_fields(work.id.clone(),None,None,None,None,Some(None)).await.err().unwrap()
            };
            assert!(format!("{error:#}").contains("later-result-receipt-fault"),"{error:#}");
            let current=runtime.storage().latest_work_item(&work.id).unwrap().unwrap();
            assert!(current.blocked_by.is_none(),"original Work ACK must remain committed");
            let expected_kind=if pick {"work_item_picked"} else {"work_item_written"};
            let mut published=None;
            while let Ok(event)=events.try_recv() {
                if event.event.kind==expected_kind {published=Some(event.event);break;}
            }
            let published=published.expect("the original Work effects precede the failed Result phase");
            let raw:String=runtime.inner.runtime_db.connection().unwrap().query_row(
                "SELECT data_json FROM audit_events WHERE audit_event_id=?1",[&published.id],|r|r.get(0)).unwrap();
            let durable:AuditEvent=serde_json::from_str(&raw).unwrap();
            assert_eq!(published,durable);
            assert_eq!(runtime.inner.runtime_db.connection().unwrap().query_row(
                "SELECT COUNT(*) FROM input_mutation_receipts WHERE family='task_result'",[],|r|r.get::<_,usize>(0)).unwrap(),0);
            let owner=runtime.begin_mutation().await.unwrap();
            let captured=owner.capture_work(&crate::runtime::resident_work::WorkCaptureRequest::current()).unwrap();
            let key=crate::runtime::resident_work::WorkKey::new(crate::runtime::resident_work::WorkFamily::Work,&work.id);
            let actual=captured.lookup(&key).unwrap().unwrap();
            assert!(matches!(&actual.record,crate::runtime::resident_work::CanonicalWorkRecord::Work(work) if work.blocked_by.is_none()));
        }
    }).await;
}

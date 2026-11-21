//! Daemon-local idle activity. Durable credential state is still read at admission.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::AuthSessionRecord;
use crate::runtime_db::RuntimeDb;

#[derive(Default)]
pub(crate) struct SessionActivity {
    entries: Mutex<HashMap<String, Activity>>,
    checkpoint: Mutex<()>,
}

struct Activity {
    last_seen_at: DateTime<Utc>,
    persisted_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

pub(crate) fn checkpoint_interval(idle_ttl: Duration) -> Duration {
    (idle_ttl / 4).min(Duration::from_secs(60))
}

impl SessionActivity {
    pub(crate) fn forget(&self, digest: &str) {
        self.entries.lock().unwrap().remove(digest);
    }

    pub(crate) fn is_active(
        &self,
        session: &mut AuthSessionRecord,
        now: DateTime<Utc>,
        idle_ttl: Duration,
    ) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if let Some(activity) = entries.get(&session.session_digest) {
            session.last_seen_at = session.last_seen_at.max(activity.last_seen_at);
        }
        let active = session.is_active_at(now, idle_ttl);
        if !active {
            entries.remove(&session.session_digest);
        }
        active
    }

    /// Check idle expiry and advance activity under one lock, including concurrent requests.
    pub(crate) fn touch(
        &self,
        session: &mut AuthSessionRecord,
        now: DateTime<Utc>,
        idle_ttl: Duration,
    ) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if let Some(activity) = entries.get(&session.session_digest) {
            session.last_seen_at = session.last_seen_at.max(activity.last_seen_at);
        }
        if !session.is_active_at(now, idle_ttl) {
            entries.remove(&session.session_digest);
            return false;
        }
        let persisted_at = session.last_seen_at;
        let activity = entries
            .entry(session.session_digest.clone())
            .or_insert(Activity {
                last_seen_at: persisted_at,
                persisted_at,
                expires_at: session.expires_at,
            });
        activity.last_seen_at = activity.last_seen_at.max(session.last_seen_at).max(now);
        session.last_seen_at = activity.last_seen_at;
        true
    }

    /// Snapshot without holding the activity lock during SQL; failed saves remain dirty.
    pub(crate) fn checkpoint(
        &self,
        db: &RuntimeDb,
        now: DateTime<Utc>,
        idle_ttl: Duration,
    ) -> Result<()> {
        let _checkpoint = self.checkpoint.lock().unwrap();
        let pending: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, activity)| activity.last_seen_at > activity.persisted_at)
            .map(|(digest, activity)| (digest.clone(), activity.last_seen_at))
            .collect();
        if !pending.is_empty() {
            db.authentication().checkpoint_session_activity(&pending)?;
        }
        let mut entries = self.entries.lock().unwrap();
        for (digest, saved_at) in pending {
            if let Some(activity) = entries.get_mut(&digest) {
                activity.persisted_at = activity.persisted_at.max(saved_at);
            }
        }
        let idle_ttl = chrono::Duration::from_std(idle_ttl).unwrap_or_default();
        entries.retain(|_, activity| {
            activity.last_seen_at > activity.persisted_at
                || (activity.expires_at.is_none_or(|expiry| expiry > now)
                    && now.signed_duration_since(activity.last_seen_at) < idle_ttl)
        });
        Ok(())
    }
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;
    use crate::authentication::AuthUserRecord;

    fn fixture() -> (tempfile::TempDir, RuntimeDb, AuthSessionRecord) {
        let directory = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            directory.path().join("runtime.sqlite"),
            directory.path().join("runtime.lock"),
        )
        .unwrap();
        let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
        db.authentication()
            .upsert_user(&AuthUserRecord {
                user_id: "activity-user".into(),
                issuer: "local".into(),
                subject: "activity-user".into(),
                display_name: None,
                email: None,
                created_at: now,
                updated_at: now,
                disabled_at: None,
            })
            .unwrap();
        let session = AuthSessionRecord {
            session_digest: "activity-session".into(),
            user_id: "activity-user".into(),
            auth_method: "local".into(),
            created_at: now,
            expires_at: None,
            last_seen_at: now,
            revoked_at: None,
        };
        db.authentication().create_session(&session).unwrap();
        (directory, db, session)
    }

    fn admissions(db: &RuntimeDb) -> u64 {
        db.reader_pool_writer_weak_for_tests()
            .upgrade()
            .unwrap()
            .write_channel_admissions_for_tests()
            .unwrap()
    }

    #[test]
    fn short_idle_expiry_uses_memory_and_does_not_revive_expired_credentials() {
        let (_directory, _db, session) = fixture();
        let activity = SessionActivity::default();
        let ttl = Duration::from_secs(1);
        let at = session.last_seen_at;
        let touch = |milliseconds| {
            activity.touch(
                &mut session.clone(),
                at + chrono::Duration::milliseconds(milliseconds),
                ttl,
            )
        };
        assert!(touch(500));
        assert!(touch(1200)); // Persisted last-seen alone would have expired.
        assert!(touch(900)); // Clock/request ordering cannot move activity backwards.
        let mut current = session.clone();
        assert!(activity.is_active(&mut current, at + chrono::Duration::milliseconds(1200), ttl));
        assert_eq!(
            current.last_seen_at,
            at + chrono::Duration::milliseconds(1200)
        );
        let mut advanced = session.clone();
        advanced.last_seen_at = at + chrono::Duration::milliseconds(1250);
        assert!(activity.touch(
            &mut advanced,
            at + chrono::Duration::milliseconds(1200),
            ttl
        ));
        assert_eq!(
            advanced.last_seen_at,
            at + chrono::Duration::milliseconds(1250)
        );
        let mut revoked = session.clone();
        revoked.revoked_at = Some(at);
        assert!(!activity.touch(&mut revoked, at + chrono::Duration::milliseconds(1300), ttl));
        assert!(!touch(1300)); // Revocation also forgot the cached activity.
        assert!(activity.entries.lock().unwrap().is_empty());
        assert!(touch(500));
        let mut absolute = session.clone();
        absolute.expires_at = Some(at + chrono::Duration::milliseconds(600));
        assert!(!activity.touch(&mut absolute, at + chrono::Duration::milliseconds(700), ttl));
        assert!(!touch(1000)); // Exact idle deadline is expired.
        assert_eq!(checkpoint_interval(ttl), Duration::from_millis(250));
        assert_eq!(
            checkpoint_interval(Duration::from_secs(86400)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn many_touches_coalesce_into_one_batch_and_clean_rounds_do_not_write() {
        let (_directory, db, session) = fixture();
        let mut second = session.clone();
        second.session_digest = "second-session".into();
        db.authentication().create_session(&second).unwrap();
        let activity = SessionActivity::default();
        let ttl = Duration::from_secs(10);
        let before = admissions(&db);
        for offset in 1..100 {
            let now = session.last_seen_at + chrono::Duration::milliseconds(offset);
            assert!(activity.touch(&mut session.clone(), now, ttl));
            assert!(activity.touch(&mut second.clone(), now, ttl));
        }
        assert_eq!(admissions(&db), before);
        let now = session.last_seen_at + chrono::Duration::seconds(1);
        activity.checkpoint(&db, now, ttl).unwrap();
        assert_eq!(admissions(&db), before + 1);
        assert_eq!(
            db.authentication()
                .find_session(&second.session_digest)
                .unwrap()
                .unwrap()
                .last_seen_at,
            session.last_seen_at + chrono::Duration::milliseconds(99),
        );
        activity
            .checkpoint(&db, now + chrono::Duration::seconds(20), ttl)
            .unwrap();
        assert_eq!(admissions(&db), before + 1);
        assert!(activity.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_checkpoint_retries_latest_activity_without_clearing_revocation() {
        let (_directory, db, session) = fixture();
        let activity = SessionActivity::default();
        let ttl = Duration::from_secs(60);
        let at = session.last_seen_at;
        assert!(activity.touch(&mut session.clone(), at + chrono::Duration::seconds(1), ttl));
        db.transaction(|tx| {
            tx.execute_batch(
                "CREATE TRIGGER fail_checkpoint BEFORE UPDATE OF last_seen_at
                ON auth_sessions BEGIN SELECT RAISE(ABORT,'checkpoint unavailable'); END",
            )?;
            Ok(())
        })
        .unwrap();
        assert!(activity.checkpoint(&db, at, ttl).is_err());
        assert!(activity.touch(&mut session.clone(), at + chrono::Duration::seconds(2), ttl));
        db.transaction(|tx| {
            tx.execute_batch("DROP TRIGGER fail_checkpoint")?;
            Ok(())
        })
        .unwrap();
        activity.checkpoint(&db, at, ttl).unwrap();
        let saved = db
            .authentication()
            .find_session(&session.session_digest)
            .unwrap()
            .unwrap();
        assert_eq!(saved.last_seen_at, at + chrono::Duration::seconds(2));
        assert!(activity.touch(&mut saved.clone(), at + chrono::Duration::seconds(3), ttl));
        db.authentication()
            .revoke_session(&session.session_digest, at)
            .unwrap();
        activity.checkpoint(&db, at, ttl).unwrap();
        let revoked = db
            .authentication()
            .find_session(&session.session_digest)
            .unwrap()
            .unwrap();
        assert_eq!(revoked.revoked_at, Some(at));
        assert_eq!(revoked.last_seen_at, saved.last_seen_at);
        assert!(!activity.touch(&mut revoked.clone(), at + chrono::Duration::seconds(4), ttl));
    }

    #[test]
    fn touch_during_queued_checkpoint_is_nonblocking_and_remains_dirty() {
        let (_directory, db, session) = fixture();
        let activity = SessionActivity::default();
        let ttl = Duration::from_secs(60);
        let first = session.last_seen_at + chrono::Duration::seconds(1);
        let next = first + chrono::Duration::seconds(1);
        assert!(activity.touch(&mut session.clone(), first, ttl));
        std::thread::scope(|scope| {
            let (held_tx, held_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let db_ref = &db;
            let blocker = scope.spawn(move || {
                db_ref.transaction(|_| {
                    held_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                    Ok(())
                })
            });
            held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let before = admissions(&db);
            let save = scope.spawn(|| activity.checkpoint(&db, first, ttl));
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while admissions(&db) == before && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            let queued = admissions(&db) > before;
            let (touched_tx, touched_rx) = std::sync::mpsc::channel();
            let activity_ref = &activity;
            let mut touched_session = session.clone();
            scope.spawn(move || {
                touched_tx
                    .send(activity_ref.touch(&mut touched_session, next, ttl))
                    .unwrap()
            });
            let touched = touched_rx.recv_timeout(Duration::from_secs(2));
            release_tx.send(()).unwrap();
            blocker.join().unwrap().unwrap();
            save.join().unwrap().unwrap();
            assert!(queued);
            assert!(touched.unwrap());
        });
        assert_eq!(
            db.authentication()
                .find_session(&session.session_digest)
                .unwrap()
                .unwrap()
                .last_seen_at,
            first
        );
        activity.checkpoint(&db, next, ttl).unwrap();
        assert_eq!(
            db.authentication()
                .find_session(&session.session_digest)
                .unwrap()
                .unwrap()
                .last_seen_at,
            next
        );
    }
}

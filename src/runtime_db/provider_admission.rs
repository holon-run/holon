//! Persistent shared availability. This repository does not schedule retries.

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

use super::{migrations::timestamp, RuntimeDb};
use crate::provider::ProviderQuotaIdentity;

mod recovery;
pub use recovery::*;
mod queue_parking;
pub use queue_parking::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
enum ScopeKind {
    Quota,
    Transport,
    RouteThrottle,
    Candidate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct ProviderAdmissionScope {
    kind: ScopeKind,
    key: String,
}

impl ProviderAdmissionScope {
    pub fn quota(identity: &ProviderQuotaIdentity) -> Self {
        Self {
            kind: ScopeKind::Quota,
            // Preserve the existing incident identity, including confidence.
            // A new key shape would silently lose pre-migration cooldowns.
            key: serde_json::to_string(identity).expect("quota identity is serializable"),
        }
    }

    /// Hash configured path identity, never a diagnostic URL or error message.
    pub fn transport(namespace: &str, configured_identity: &str) -> Result<Self> {
        Self::configured(ScopeKind::Transport, namespace, configured_identity)
    }

    pub fn route_throttle(configured_identity: &str) -> Result<Self> {
        Self::configured(
            ScopeKind::RouteThrottle,
            "route_throttle",
            configured_identity,
        )
    }

    pub fn candidate(configured_identity: &str) -> Result<Self> {
        Self::configured(ScopeKind::Candidate, "candidate", configured_identity)
    }

    fn configured(kind: ScopeKind, namespace: &str, configured_identity: &str) -> Result<Self> {
        ensure!(
            !namespace.trim().is_empty() && !configured_identity.trim().is_empty(),
            "provider scope requires a namespace and configured identity"
        );
        let mut hash = Sha256::new();
        hash.update(namespace.trim().as_bytes());
        hash.update([0]);
        hash.update(configured_identity.trim().as_bytes());
        Ok(Self {
            kind,
            key: format!("sha256:{:x}", hash.finalize()),
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    fn table(&self) -> &'static str {
        match self.kind {
            ScopeKind::Quota => "provider_quota_incidents",
            ScopeKind::Transport => "provider_transport_incidents",
            ScopeKind::RouteThrottle => "provider_route_throttle_incidents",
            ScopeKind::Candidate => "provider_candidate_incidents",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCooldownPolicy {
    TransientQuota,
    HardQuota,
    PaymentRequired,
    Transport,
}

impl ProviderCooldownPolicy {
    fn failure_kind(self) -> &'static str {
        match self {
            Self::TransientQuota | Self::HardQuota => "rate_limited",
            Self::PaymentRequired => "payment_required",
            Self::Transport => "transport",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::TransientQuota => "transient_quota",
            Self::HardQuota => "hard_quota",
            Self::PaymentRequired => "payment_required",
            Self::Transport => "transport",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "transient_quota" => Ok(Self::TransientQuota),
            "hard_quota" => Ok(Self::HardQuota),
            "payment_required" => Ok(Self::PaymentRequired),
            "transport" => Ok(Self::Transport),
            _ => anyhow::bail!("invalid persisted provider cooldown policy"),
        }
    }

    fn delay(self, failures: i64, key: &str) -> Duration {
        let exponent = failures.saturating_sub(1).clamp(0, 31) as u32;
        let (base_ms, cap_ms, jitter_percent) = match self {
            Self::TransientQuota => (250_i64.saturating_mul(1_i64 << exponent), 30_000, 0),
            Self::HardQuota | Self::PaymentRequired => (
                900_000_i64.saturating_mul(1_i64 << exponent),
                21_600_000,
                10,
            ),
            Self::Transport => (
                match failures {
                    0 | 1 => 10_000,
                    2 => 30_000,
                    3 => 120_000,
                    _ => 300_000,
                },
                300_000,
                20,
            ),
        };
        let base_ms = base_ms.min(cap_ms);
        let jitter_cap = (base_ms * jitter_percent / 100).min(cap_ms - base_ms);
        let hash = Sha256::digest(format!("{key}:{failures}").as_bytes());
        let seed = u64::from_le_bytes(hash[..8].try_into().expect("eight hash bytes"));
        let jitter = seed % (jitter_cap as u64 + 1);
        Duration::milliseconds(base_ms + jitter as i64)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProviderAdmissionBlocked {
    pub scope: ProviderAdmissionScope,
    pub not_before: DateTime<Utc>,
}

/// Safe shared-failure metadata, never an upstream error or request dump.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ProviderCooldownCause {
    pub failure_kind: String,
    pub http_status: Option<u16>,
    pub occurred_at: DateTime<Utc>,
    pub server_not_before: Option<DateTime<Utc>>,
    pub incident_id: Option<String>,
    pub details_available: bool,
}

#[derive(Debug)]
struct ScopeLease {
    scope: ProviderAdmissionScope,
    revision: i64,
    probe_token: Option<String>,
}

/// Opaque, non-cloneable reservation; it is not permission to replay a request.
#[derive(Debug)]
pub struct ProviderAdmissionLease {
    scopes: Vec<ScopeLease>,
}

impl ProviderAdmissionLease {
    pub fn is_probe(&self) -> bool {
        self.scopes.iter().any(|scope| scope.probe_token.is_some())
    }
}

#[derive(Debug)]
pub enum ProviderAdmissionReservation {
    Granted(ProviderAdmissionLease),
    Blocked(ProviderAdmissionBlocked),
}

struct ScopeState {
    revision: i64,
    failures: i64,
    next_retry_at: Option<DateTime<Utc>>,
    server_not_before: Option<DateTime<Utc>>,
    probe_token: Option<String>,
    probe_lease_until: Option<DateTime<Utc>>,
    resolved: bool,
    policy: Option<ProviderCooldownPolicy>,
}

impl ScopeState {
    fn blocked_until_with_policy(
        &self,
        now: DateTime<Utc>,
        allow_policy_probe: bool,
    ) -> Option<DateTime<Utc>> {
        if self.resolved {
            return None;
        }
        self.next_retry_at
            .filter(|_| {
                !allow_policy_probe || self.policy == Some(ProviderCooldownPolicy::PaymentRequired)
            })
            .into_iter()
            .chain(self.server_not_before)
            .chain(self.probe_lease_until)
            .filter(|deadline| *deadline > now)
            .max()
    }

    fn matches(&self, lease: &ScopeLease, now: DateTime<Utc>) -> bool {
        self.revision == lease.revision
            && self.probe_token == lease.probe_token
            && match lease.probe_token {
                Some(_) => self.probe_lease_until.is_some_and(|until| until > now),
                None => self.resolved,
            }
    }
}

fn read_state(
    connection: &Connection,
    scope: &ProviderAdmissionScope,
) -> Result<Option<ScopeState>> {
    let raw = connection
        .query_row(
            &format!(
                "SELECT revision, consecutive_failures, next_retry_at, server_not_before,
                        probe_token, probe_lease_until, resolved_at IS NOT NULL, cooldown_policy
                 FROM {} WHERE identity_key = ?1",
                scope.table()
            ),
            [scope.key()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    raw.map(
        |(revision, failures, next, server, token, lease, resolved, policy)| {
            let parse = |value: Option<String>| -> Result<Option<DateTime<Utc>>> {
                value
                    .map(|value| {
                        DateTime::parse_from_rfc3339(&value)
                            .map(|value| value.with_timezone(&Utc))
                            .context("invalid persisted provider admission deadline")
                    })
                    .transpose()
            };
            ensure!(
                token.is_some() == lease.is_some(),
                "incomplete persisted provider probe lease"
            );
            Ok(ScopeState {
                revision,
                failures,
                next_retry_at: parse(next)?,
                server_not_before: parse(server)?,
                probe_token: token,
                probe_lease_until: parse(lease)?,
                resolved,
                policy: policy
                    .map(|value| ProviderCooldownPolicy::parse(&value))
                    .transpose()?,
            })
        },
    )
    .transpose()
}

fn blocked(
    connection: &Connection,
    scopes: &[ProviderAdmissionScope],
    now: DateTime<Utc>,
) -> Result<Option<ProviderAdmissionBlocked>> {
    blocked_with_policy(connection, scopes, now, false)
}

fn blocked_with_policy(
    connection: &Connection,
    scopes: &[ProviderAdmissionScope],
    now: DateTime<Utc>,
    allow_policy_probe: bool,
) -> Result<Option<ProviderAdmissionBlocked>> {
    let mut result: Option<ProviderAdmissionBlocked> = None;
    for scope in scopes {
        if let Some(until) = read_state(connection, scope)?
            .and_then(|state| state.blocked_until_with_policy(now, allow_policy_probe))
        {
            if result
                .as_ref()
                .is_none_or(|current| until > current.not_before)
            {
                result = Some(ProviderAdmissionBlocked {
                    scope: scope.clone(),
                    not_before: until,
                });
            }
        }
    }
    Ok(result)
}

fn reserve_admission(
    tx: &Transaction<'_>,
    scopes: &[ProviderAdmissionScope],
    now: DateTime<Utc>,
    lease_duration: Duration,
    allow_policy_probe: bool,
) -> Result<ProviderAdmissionReservation> {
    ensure!(!scopes.is_empty(), "provider admission requires a scope");
    ensure!(
        lease_duration > Duration::zero(),
        "probe lease must be positive"
    );
    let until = now
        .checked_add_signed(lease_duration)
        .context("probe lease overflow")?;
    let mut scopes = scopes.to_vec();
    scopes.sort();
    scopes.dedup();
    if let Some(blocked) = blocked_with_policy(tx, &scopes, now, allow_policy_probe)? {
        return Ok(ProviderAdmissionReservation::Blocked(blocked));
    }
    let mut leases = Vec::with_capacity(scopes.len());
    for scope in &scopes {
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO {} (
                    identity_key, incident_id, revision, consecutive_failures,
                    opened_at, last_failed_at, resolved_at
                 ) VALUES (?1, ?2, 1, 0, ?3, ?3, ?3)",
                scope.table()
            ),
            params![
                scope.key(),
                uuid::Uuid::new_v4().to_string(),
                timestamp(now)
            ],
        )?;
        let state = read_state(tx, scope)?.context("provider admission row missing")?;
        let probe_token = (!state.resolved).then(|| uuid::Uuid::new_v4().to_string());
        let revision = if let Some(token) = &probe_token {
            tx.execute(
                &format!(
                    "UPDATE {} SET revision = revision + 1,
                        probe_token = ?2, probe_lease_until = ?3
                     WHERE identity_key = ?1",
                    scope.table()
                ),
                params![scope.key(), token, timestamp(until)],
            )?;
            state.revision + 1
        } else {
            state.revision
        };
        leases.push(ScopeLease {
            scope: scope.clone(),
            revision,
            probe_token,
        });
    }
    Ok(ProviderAdmissionReservation::Granted(
        ProviderAdmissionLease { scopes: leases },
    ))
}

fn lease_is_valid(
    connection: &Connection,
    lease: &ProviderAdmissionLease,
    now: DateTime<Utc>,
) -> Result<bool> {
    for scope in &lease.scopes {
        if !read_state(connection, &scope.scope)?.is_some_and(|state| state.matches(scope, now)) {
            return Ok(false);
        }
    }
    Ok(true)
}

impl RuntimeDb {
    pub fn provider_cooldown_cause(
        &self,
        scope: &ProviderAdmissionScope,
    ) -> Result<Option<ProviderCooldownCause>> {
        self.with_read(|connection| {
            let row = connection
                .query_row(
                    &format!(
                        "SELECT cause_json, last_failure_kind, last_failed_at,
                            server_not_before, incident_id
                     FROM {} WHERE identity_key = ?1 AND resolved_at IS NULL",
                        scope.table()
                    ),
                    [scope.key()],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((cause, kind, failed, server, incident)) = row else {
                return Ok(None);
            };
            let parse = |value: &str| -> Result<DateTime<Utc>> {
                Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
            };
            let mut cause = match cause {
                Some(cause) => serde_json::from_str::<ProviderCooldownCause>(&cause)?,
                None => ProviderCooldownCause {
                    failure_kind: kind.unwrap_or_else(|| "unknown".into()),
                    http_status: None,
                    occurred_at: parse(&failed)?,
                    server_not_before: server.as_deref().map(parse).transpose()?,
                    incident_id: None,
                    details_available: false,
                },
            };
            // Incident identity is local and opaque; diagnostic URLs and raw
            // provider-supplied strings cannot cross the shared-agent boundary.
            cause.incident_id = uuid::Uuid::parse_str(&incident)
                .ok()
                .map(|id| id.to_string());
            if !matches!(
                cause.failure_kind.as_str(),
                "connection"
                    | "timeout"
                    | "rate_limited"
                    | "server_error"
                    | "transport"
                    | "payment_required"
            ) {
                cause.failure_kind = "unknown".into();
                cause.details_available = false;
            }
            Ok(Some(cause))
        })
    }

    pub fn provider_admission_blocked(
        &self,
        scopes: &[ProviderAdmissionScope],
        now: DateTime<Utc>,
    ) -> Result<Option<ProviderAdmissionBlocked>> {
        self.with_read(|connection| blocked(connection, scopes, now))
    }

    /// Claim all applicable scopes atomically. Reads and rejected claims do not
    /// increment failure counters or extend deadlines.
    pub fn reserve_provider_admission(
        &self,
        scopes: &[ProviderAdmissionScope],
        now: DateTime<Utc>,
        lease_duration: Duration,
    ) -> Result<ProviderAdmissionReservation> {
        self.transaction(|tx| reserve_admission(tx, scopes, now, lease_duration, false))
    }

    /// Must be composed with durable episode charging at the dispatch boundary.
    pub fn validate_provider_admission(
        &self,
        lease: &ProviderAdmissionLease,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.with_read(|connection| lease_is_valid(connection, lease, now))
    }

    /// Only for work known not to have dispatched. No failure is inferred.
    pub fn release_provider_admission(&self, lease: &ProviderAdmissionLease) -> Result<()> {
        self.transaction(|tx| release_probes(tx, lease, None))
    }

    pub fn record_provider_admission_success(
        &self,
        lease: &ProviderAdmissionLease,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        self.transaction(|tx| {
            let mut resolved = 0;
            for scope in &lease.scopes {
                if !read_state(tx, &scope.scope)?
                    .is_some_and(|state| !state.resolved && state.matches(scope, now))
                {
                    continue;
                }
                resolved += tx.execute(
                    &format!(
                        "UPDATE {} SET revision = revision + 1, consecutive_failures = 0,
                            next_retry_at = NULL, server_not_before = NULL, cooldown_policy = NULL,
                            resolved_at = ?2,
                            probe_token = NULL, probe_lease_until = NULL WHERE identity_key = ?1",
                        scope.scope.table()
                    ),
                    params![scope.scope.key(), timestamp(now)],
                )?;
            }
            Ok(resolved)
        })
    }

    pub fn record_provider_admission_failure(
        &self,
        lease: &ProviderAdmissionLease,
        failed_scope: &ProviderAdmissionScope,
        policy: ProviderCooldownPolicy,
        now: DateTime<Utc>,
        server_not_before: Option<DateTime<Utc>>,
        model_ref: &str,
    ) -> Result<bool> {
        self.record_provider_admission_failure_with_cause(
            lease,
            failed_scope,
            policy,
            now,
            server_not_before,
            model_ref,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_provider_admission_failure_with_cause(
        &self,
        lease: &ProviderAdmissionLease,
        failed_scope: &ProviderAdmissionScope,
        policy: ProviderCooldownPolicy,
        now: DateTime<Utc>,
        server_not_before: Option<DateTime<Utc>>,
        model_ref: &str,
        cause: Option<&ProviderCooldownCause>,
    ) -> Result<bool> {
        ensure!(
            matches!(
                (failed_scope.kind, policy),
                (ScopeKind::Transport, ProviderCooldownPolicy::Transport)
                    | (
                        ScopeKind::Candidate,
                        ProviderCooldownPolicy::PaymentRequired
                    )
                    | (
                        ScopeKind::Quota | ScopeKind::RouteThrottle,
                        ProviderCooldownPolicy::TransientQuota | ProviderCooldownPolicy::HardQuota
                    )
            ),
            "provider cooldown policy does not match its scope"
        );
        let scope = lease
            .scopes
            .iter()
            .find(|scope| scope.scope == *failed_scope)
            .context("failed scope was not admitted")?;
        self.transaction(|tx| {
            let state = read_state(tx, failed_scope)?.context("provider admission row missing")?;
            if !state.matches(scope, now) {
                release_probes(tx, lease, Some(failed_scope))?;
                return Ok(false);
            }
            let failures = if state.resolved { 1 } else { state.failures.saturating_add(1) };
            let policy = if !state.resolved && state.policy == Some(ProviderCooldownPolicy::HardQuota) {
                ProviderCooldownPolicy::HardQuota
            } else {
                policy
            };
            let server = server_not_before
                .filter(|until| *until > now && *until - now <= Duration::days(35))
                .max(state.server_not_before);
            let local_deadline = now.checked_add_signed(policy.delay(failures, failed_scope.key()))
                .context("provider cooldown deadline overflow")?;
            let server_deadline = server.map(|until| -> Result<_> {
                if until - now <= Duration::seconds(30) {
                    return Ok(until);
                }
                let hash = Sha256::digest(failed_scope.key().as_bytes());
                let jitter_ms = u16::from_le_bytes([hash[0], hash[1]]) % 3_001;
                until.checked_add_signed(Duration::milliseconds(30_000 + i64::from(jitter_ms)))
                    .context("provider server deadline overflow")
            }).transpose()?.unwrap_or(now);
            let deadline = local_deadline
                .max(state.next_retry_at.unwrap_or(now))
                .max(server_deadline);
            tx.execute(
                &format!(
                    "UPDATE {} SET revision = revision + 1, consecutive_failures = ?2,
                        next_retry_at = ?3, server_not_before = ?4, last_failure_kind = ?5,
                        last_model_ref = ?6, last_failed_at = ?7, resolved_at = NULL,
                        opened_at = CASE WHEN resolved_at IS NOT NULL THEN ?7 ELSE opened_at END,
                        incident_id = CASE WHEN resolved_at IS NOT NULL THEN ?8 ELSE incident_id END,
                        probe_token = NULL, probe_lease_until = NULL, cooldown_policy = ?9,
                        cause_json = ?10
                     WHERE identity_key = ?1",
                    failed_scope.table()
                ),
                params![
                    failed_scope.key(), failures, timestamp(deadline), server.map(timestamp),
                    policy.failure_kind(), model_ref, timestamp(now), uuid::Uuid::new_v4().to_string(),
                    policy.as_str(),
                    cause.map(serde_json::to_string).transpose()?,
                ],
            )?;
            release_probes(tx, lease, Some(failed_scope))?;
            Ok(true)
        })
    }
}

fn release_probes(
    tx: &Transaction<'_>,
    lease: &ProviderAdmissionLease,
    except: Option<&ProviderAdmissionScope>,
) -> Result<()> {
    for scope in &lease.scopes {
        if except == Some(&scope.scope) || scope.probe_token.is_none() {
            continue;
        }
        tx.execute(
            &format!(
                "UPDATE {} SET revision = revision + 1, probe_token = NULL, probe_lease_until = NULL
                 WHERE identity_key = ?1 AND revision = ?2 AND probe_token = ?3",
                scope.scope.table()
            ),
            params![scope.scope.key(), scope.revision, scope.probe_token],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_cause_survives_restart_without_copying_upstream_errors() -> Result<()> {
        let (dir, db) = database()?;
        let scope = transport("shared-cause");
        let lease = reserve(&db, std::slice::from_ref(&scope), now())?;
        let deadline = now() + Duration::seconds(11);
        let cause = ProviderCooldownCause {
            failure_kind: "connection".into(),
            http_status: Some(503),
            occurred_at: now(),
            server_not_before: Some(deadline),
            incident_id: None,
            details_available: true,
        };
        assert!(db.record_provider_admission_failure_with_cause(
            &lease,
            &scope,
            ProviderCooldownPolicy::Transport,
            now(),
            Some(deadline),
            "provider/model",
            Some(&cause),
        )?);
        let observed = reopen(&dir)?.provider_cooldown_cause(&scope)?.unwrap();
        assert_eq!(observed.failure_kind, "connection");
        assert_eq!(observed.http_status, Some(503));
        assert_eq!(observed.server_not_before, Some(deadline));
        assert!(observed.details_available);
        assert!(uuid::Uuid::parse_str(observed.incident_id.as_deref().unwrap()).is_ok());
        Ok(())
    }

    #[test]
    fn historical_cooldown_cause_degrades_and_untrusted_classification_is_not_broadcast(
    ) -> Result<()> {
        let (_dir, db) = database()?;
        let scope = transport("legacy-cause");
        let lease = reserve(&db, std::slice::from_ref(&scope), now())?;
        assert!(db.record_provider_admission_failure(
            &lease,
            &scope,
            ProviderCooldownPolicy::Transport,
            now(),
            None,
            "provider/model",
        )?);
        let cause = db.provider_cooldown_cause(&scope)?.unwrap();
        assert_eq!(cause.failure_kind, "transport");
        assert!(!cause.details_available);
        assert_eq!(cause.http_status, None);
        db.transaction(|tx| {
            tx.execute(
                "UPDATE provider_transport_incidents
                 SET last_failure_kind = ?1, incident_id = ?1",
                ["https://secret.invalid/?token=secret"],
            )?;
            Ok(())
        })?;
        let cause = db.provider_cooldown_cause(&scope)?.unwrap();
        assert_eq!(cause.failure_kind, "unknown");
        assert_eq!(cause.incident_id, None);
        assert!(!serde_json::to_string(&cause)?.contains("secret"));
        Ok(())
    }
    mod payment_required;

    fn now() -> DateTime<Utc> {
        "2026-10-24T17:00:00Z".parse().unwrap()
    }

    fn database() -> Result<(tempfile::TempDir, RuntimeDb)> {
        let dir = tempfile::tempdir()?;
        let db = reopen(&dir)?;
        Ok((dir, db))
    }

    fn reopen(dir: &tempfile::TempDir) -> Result<RuntimeDb> {
        RuntimeDb::open_and_migrate(
            &dir.path().join("runtime.sqlite"),
            &dir.path().join("runtime.lock"),
        )
    }

    fn quota(value: &str) -> ProviderAdmissionScope {
        ProviderAdmissionScope::quota(&ProviderQuotaIdentity::exact("test", value).unwrap())
    }

    fn transport(value: &str) -> ProviderAdmissionScope {
        ProviderAdmissionScope::transport("connection-path", value).unwrap()
    }

    fn reserve(
        db: &RuntimeDb,
        scopes: &[ProviderAdmissionScope],
        now: DateTime<Utc>,
    ) -> Result<ProviderAdmissionLease> {
        match db.reserve_provider_admission(scopes, now, Duration::minutes(2))? {
            ProviderAdmissionReservation::Granted(lease) => Ok(lease),
            ProviderAdmissionReservation::Blocked(blocked) => anyhow::bail!("blocked: {blocked:?}"),
        }
    }

    fn fail(
        db: &RuntimeDb,
        scope: &ProviderAdmissionScope,
        at: DateTime<Utc>,
        policy: ProviderCooldownPolicy,
    ) -> Result<DateTime<Utc>> {
        let lease = reserve(db, std::slice::from_ref(scope), at)?;
        assert!(db.record_provider_admission_failure(
            &lease,
            scope,
            policy,
            at,
            None,
            "test/model",
        )?);
        Ok(db
            .provider_admission_blocked(std::slice::from_ref(scope), at)?
            .unwrap()
            .not_before)
    }

    #[test]
    fn route_throttle_deadlines_probe_fences_and_isolation_survive_restart() -> Result<()> {
        for policy in [
            ProviderCooldownPolicy::TransientQuota,
            ProviderCooldownPolicy::HardQuota,
        ] {
            let (dir, db) = database()?;
            let scope =
                ProviderAdmissionScope::route_throttle("configured/provider/endpoint/model")?;
            let connection = transport("configured/endpoint");
            let stale = reserve(&db, &[scope.clone(), connection.clone()], now())?;
            let failure = reserve(&db, &[scope.clone(), connection.clone()], now())?;
            let server = now() + Duration::hours(1);
            assert!(db.record_provider_admission_failure(
                &failure,
                &scope,
                policy,
                now(),
                Some(server),
                "test/model",
            )?);
            assert_eq!(db.record_provider_admission_success(&stale, now())?, 0);
            let deadline = db
                .provider_admission_blocked(std::slice::from_ref(&scope), now())?
                .unwrap()
                .not_before;
            assert!(deadline >= server);
            assert!(db
                .provider_admission_blocked(&[connection], now())?
                .is_none());
            assert!(db
                .provider_admission_blocked(
                    &[ProviderAdmissionScope::route_throttle(
                        "configured/provider/endpoint/other-model"
                    )?],
                    now(),
                )?
                .is_none());
            assert!(db
                .provider_admission_blocked(&[quota("new-credential")], now())?
                .is_none());
            drop(db);
            let db = reopen(&dir)?;
            let other = reopen(&dir)?;
            assert_eq!(
                db.provider_admission_blocked(std::slice::from_ref(&scope), now())?
                    .unwrap()
                    .not_before,
                deadline
            );
            let probe = reserve(&db, std::slice::from_ref(&scope), deadline)?;
            assert!(probe.is_probe());
            assert!(matches!(
                other.reserve_provider_admission(
                    std::slice::from_ref(&scope),
                    deadline,
                    Duration::minutes(2)
                )?,
                ProviderAdmissionReservation::Blocked(_)
            ));
            assert_eq!(db.record_provider_admission_success(&probe, deadline)?, 1);
            assert!(other
                .provider_admission_blocked(&[scope], deadline)?
                .is_none());
        }
        Ok(())
    }

    #[test]
    fn provider_admission_legacy_quota_and_deadline_survive_reopen() -> Result<()> {
        let (dir, db) = database()?;
        let scope = quota("legacy");
        db.record_provider_quota_failure(
            scope.key(),
            "legacy-incident",
            &timestamp(now()),
            Some(&timestamp(now() + Duration::hours(8))),
            "rate_limited",
            "test/model",
        )?;
        let reopened = reopen(&dir)?;
        let blocked = reopened
            .provider_admission_blocked(std::slice::from_ref(&scope), now())?
            .unwrap();
        assert_eq!(blocked.not_before, now() + Duration::hours(8));
        assert!(matches!(
            reopened.reserve_provider_admission(&[scope], now(), Duration::minutes(2))?,
            ProviderAdmissionReservation::Blocked(_)
        ));
        Ok(())
    }

    #[test]
    fn provider_admission_transport_is_persistent_and_probe_is_cross_connection_single_flight(
    ) -> Result<()> {
        let (dir, db) = database()?;
        let scope = transport("endpoint-a");
        let due = fail(&db, &scope, now(), ProviderCooldownPolicy::Transport)?;
        assert!(due >= now() + Duration::seconds(10));
        assert!(due <= now() + Duration::seconds(12));
        let other = reopen(&dir)?;
        let (first, second) = std::thread::scope(|threads| {
            let first = threads.spawn(|| {
                db.reserve_provider_admission(
                    std::slice::from_ref(&scope),
                    due,
                    Duration::minutes(2),
                )
            });
            let second = threads.spawn(|| {
                other.reserve_provider_admission(
                    std::slice::from_ref(&scope),
                    due,
                    Duration::minutes(2),
                )
            });
            (first.join().unwrap(), second.join().unwrap())
        });
        let results = [first?, second?];
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, ProviderAdmissionReservation::Granted(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, ProviderAdmissionReservation::Blocked(_)))
                .count(),
            1
        );
        let blocked = other.provider_admission_blocked(&[scope], due)?.unwrap();
        assert_eq!(blocked.not_before, due + Duration::minutes(2));
        Ok(())
    }

    #[test]
    fn provider_admission_checks_every_scope_before_claiming_any() -> Result<()> {
        let (_dir, db) = database()?;
        let network = transport("endpoint-a");
        let account = quota("account-a");
        let network_due = fail(&db, &network, now(), ProviderCooldownPolicy::Transport)?;
        let quota_due = fail(&db, &account, now(), ProviderCooldownPolicy::HardQuota)?;
        let blocked = db
            .provider_admission_blocked(&[network.clone(), account.clone()], now())?
            .unwrap();
        assert_eq!(blocked.not_before, quota_due);
        assert!(matches!(
            db.reserve_provider_admission(
                &[network.clone(), account],
                network_due,
                Duration::minutes(2)
            )?,
            ProviderAdmissionReservation::Blocked(_)
        ));
        let probe = reserve(&db, &[network], network_due)?;
        assert!(
            probe.is_probe(),
            "blocked composite claim must not acquire its other scope"
        );
        Ok(())
    }

    #[test]
    fn provider_admission_preflight_release_preserves_failure_sequence() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        let due = fail(&db, &scope, now(), ProviderCooldownPolicy::HardQuota)?;
        let probe = reserve(&db, std::slice::from_ref(&scope), due)?;
        assert!(probe.is_probe());
        db.release_provider_admission(&probe)?;
        assert!(!db.validate_provider_admission(&probe, due)?);
        let next = reserve(&db, std::slice::from_ref(&scope), due)?;
        assert!(db.record_provider_admission_failure(
            &next,
            &scope,
            ProviderCooldownPolicy::HardQuota,
            due,
            None,
            "test/model",
        )?);
        let incident = db.provider_quota_incident(scope.key())?.unwrap();
        assert_eq!(incident.consecutive_failures, 2);
        let next_due = db
            .provider_admission_blocked(&[scope], due)?
            .unwrap()
            .not_before;
        assert!(next_due >= due + Duration::minutes(30));
        Ok(())
    }

    #[test]
    fn provider_admission_expired_probe_cannot_settle_its_replacement() -> Result<()> {
        let (dir, db) = database()?;
        let scope = transport("endpoint");
        let due = fail(&db, &scope, now(), ProviderCooldownPolicy::Transport)?;
        let old = reserve(&db, std::slice::from_ref(&scope), due)?;
        let later = due + Duration::minutes(2);
        let reopened = reopen(&dir)?;
        let replacement = reserve(&reopened, std::slice::from_ref(&scope), later)?;
        assert_eq!(db.record_provider_admission_success(&old, later)?, 0);
        assert!(!db.record_provider_admission_failure(
            &old,
            &scope,
            ProviderCooldownPolicy::Transport,
            later,
            None,
            "old/model",
        )?);
        db.release_provider_admission(&old)?;
        assert!(reopened.validate_provider_admission(&replacement, later)?);
        assert_eq!(
            reopened.record_provider_admission_success(&replacement, later)?,
            1
        );
        assert!(reopened
            .provider_admission_blocked(&[scope], later)?
            .is_none());
        Ok(())
    }

    #[test]
    fn provider_admission_late_results_do_not_overwrite_newer_facts() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        let old = reserve(&db, std::slice::from_ref(&scope), now())?;
        let newer = reserve(&db, std::slice::from_ref(&scope), now())?;
        // Healthy successes are not incident transitions and must not fence
        // other healthy requests. A concurrent failure still opens an incident.
        assert_eq!(db.record_provider_admission_success(&newer, now())?, 0);
        assert!(db.validate_provider_admission(&old, now())?);
        assert!(db.record_provider_admission_failure(
            &old,
            &scope,
            ProviderCooldownPolicy::HardQuota,
            now(),
            None,
            "old/model",
        )?);
        let due = db
            .provider_admission_blocked(std::slice::from_ref(&scope), now())?
            .unwrap()
            .not_before;
        assert_eq!(db.record_provider_admission_success(&old, now())?, 0);
        assert_eq!(db.record_provider_admission_success(&newer, now())?, 0);
        assert_eq!(
            db.provider_admission_blocked(&[scope], now())?
                .unwrap()
                .not_before,
            due
        );
        Ok(())
    }

    #[test]
    fn provider_admission_success_is_scope_specific() -> Result<()> {
        let (_dir, db) = database()?;
        let first = transport("first");
        let second = transport("second");
        let due = fail(&db, &first, now(), ProviderCooldownPolicy::Transport)?;
        let healthy = reserve(&db, &[second], now())?;
        assert_eq!(db.record_provider_admission_success(&healthy, now())?, 0);
        assert_eq!(
            db.provider_admission_blocked(&[first], now())?
                .unwrap()
                .not_before,
            due
        );
        Ok(())
    }

    #[test]
    fn provider_admission_hard_quota_progression_is_persisted_and_capped() -> Result<()> {
        let (dir, mut db) = database()?;
        let scope = quota("account");
        let mut at = now();
        for minutes in [15, 30, 60, 120, 240, 360, 360] {
            let due = fail(&db, &scope, at, ProviderCooldownPolicy::HardQuota)?;
            let minimum = Duration::minutes(minutes);
            let maximum = (minimum + minimum / 10).min(Duration::hours(6));
            assert!(due >= at + minimum);
            assert!(due <= at + maximum);
            db = reopen(&dir)?;
            assert_eq!(
                db.provider_admission_blocked(std::slice::from_ref(&scope), at)?
                    .unwrap()
                    .not_before,
                due
            );
            at = due;
        }
        Ok(())
    }

    #[test]
    fn provider_admission_transient_probe_does_not_downgrade_hard_quota_policy() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        let due = fail(&db, &scope, now(), ProviderCooldownPolicy::HardQuota)?;
        let next = fail(&db, &scope, due, ProviderCooldownPolicy::TransientQuota)?;
        assert!(next >= due + Duration::minutes(30));
        let incident = db.provider_quota_incident(scope.key())?.unwrap();
        assert_eq!(incident.last_failure_kind.as_deref(), Some("rate_limited"));
        assert_eq!(incident.consecutive_failures, 2);
        Ok(())
    }

    #[test]
    fn provider_admission_trusted_deadline_is_not_capped_to_policy_interval() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        let lease = reserve(&db, std::slice::from_ref(&scope), now())?;
        let server = now() + Duration::days(2);
        assert!(db.record_provider_admission_failure(
            &lease,
            &scope,
            ProviderCooldownPolicy::TransientQuota,
            now(),
            Some(server),
            "test/model",
        )?);
        let blocked = db.provider_admission_blocked(&[scope], now())?.unwrap();
        assert!(blocked.not_before >= server + Duration::seconds(30));
        assert!(blocked.not_before <= server + Duration::seconds(33));
        Ok(())
    }

    #[test]
    fn provider_admission_invalid_server_horizon_uses_local_policy() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        let lease = reserve(&db, std::slice::from_ref(&scope), now())?;
        assert!(db.record_provider_admission_failure(
            &lease,
            &scope,
            ProviderCooldownPolicy::HardQuota,
            now(),
            Some(now() + Duration::days(36)),
            "test/model",
        )?);
        assert!(
            db.provider_admission_blocked(&[scope], now())?
                .unwrap()
                .not_before
                < now() + Duration::minutes(17)
        );
        Ok(())
    }

    #[test]
    fn provider_admission_bad_persisted_deadline_fails_closed() -> Result<()> {
        let (_dir, db) = database()?;
        let scope = quota("account");
        fail(&db, &scope, now(), ProviderCooldownPolicy::HardQuota)?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE provider_quota_incidents SET next_retry_at = 'not-a-date' WHERE identity_key = ?1",
                [scope.key()],
            )?;
            Ok(())
        })?;
        assert!(db
            .provider_admission_blocked(std::slice::from_ref(&scope), now())
            .is_err());
        assert!(db
            .reserve_provider_admission(&[scope], now(), Duration::minutes(2))
            .is_err());
        Ok(())
    }

    #[test]
    fn provider_admission_rejects_wrong_policy_and_never_stores_raw_transport_identity(
    ) -> Result<()> {
        let (_dir, db) = database()?;
        let scope = transport("proxy-user:proxy-secret@configured-endpoint");
        assert!(scope.key().starts_with("sha256:"));
        assert!(!scope.key().contains("secret"));
        let lease = reserve(&db, std::slice::from_ref(&scope), now())?;
        assert!(db
            .record_provider_admission_failure(
                &lease,
                &scope,
                ProviderCooldownPolicy::HardQuota,
                now(),
                None,
                "test/model",
            )
            .is_err());
        assert!(db.validate_provider_admission(&lease, now())?);
        assert!(db
            .reserve_provider_admission(&[], now(), Duration::minutes(2))
            .is_err());
        assert!(db
            .reserve_provider_admission(&[scope], now(), Duration::zero())
            .is_err());
        Ok(())
    }
}

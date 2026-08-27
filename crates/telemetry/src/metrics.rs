//! The instrument names Vault emits, and nothing else.
//!
//! Names live beside the code that increments them but in one module, so a dashboard query and an
//! emit site cannot disagree by drifting apart in two files.

/// The build-identity gauge, set once at startup. Every dashboard's "what is running" anchor.
pub const VAULT_BUILD_INFO: &str = "vault_build_info";

/// The aggregate readiness of this process: `1.0` when `/health/ready` would answer 200, else `0`.
pub const VAULT_READINESS: &str = "vault_readiness";

/// Units currently admitted or deferred by the bounded backlog planner.
pub const VAULT_REPLICATION_BACKLOG_ITEMS: &str = "vault_replication_backlog_items";
/// Bytes currently admitted or deferred by the bounded backlog planner.
pub const VAULT_REPLICATION_BACKLOG_BYTES: &str = "vault_replication_backlog_bytes";
/// Terminal replication attempts by closed outcome and failure class.
pub const VAULT_REPLICATION_ATTEMPTS_TOTAL: &str = "vault_replication_attempts_total";
/// End-to-end duration of terminal replication attempts.
pub const VAULT_REPLICATION_DURATION_SECONDS: &str = "vault_replication_duration_seconds";
/// Bytes whose remote identity was successfully re-verified.
pub const VAULT_REPLICATION_BYTES_TOTAL: &str = "vault_replication_bytes_total";
/// Age in seconds of the oldest required placement verification.
pub const VAULT_REPLICA_LAST_VERIFICATION_AGE_SECONDS: &str =
    "vault_replica_last_verification_age_seconds";
/// Terminal LFS/wiki collector attempts by closed collector, outcome, and failure class.
pub const VAULT_COLLECTOR_ATTEMPTS_TOTAL: &str = "vault_collector_attempts_total";
/// Verified LFS object count from complete collector attempts.
pub const VAULT_LFS_OBJECTS_TOTAL: &str = "vault_lfs_objects_total";
/// Verified LFS bytes from complete collector attempts.
pub const VAULT_LFS_BYTES_TOTAL: &str = "vault_lfs_bytes_total";
/// End-to-end duration of one bounded collector attempt.
pub const VAULT_COLLECTOR_DURATION_SECONDS: &str = "vault_collector_duration_seconds";

/// Closed executable collector vocabulary; provider repository names can never enter it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorDiagnosticKind {
    /// Git LFS object acquisition.
    GitLfs,
    /// Git wiki sibling discovery/mirroring.
    Wiki,
}

impl CollectorDiagnosticKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GitLfs => "git_lfs",
            Self::Wiki => "wiki",
        }
    }
}

/// Closed collector terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectorDiagnosticOutcome {
    /// Required content completed and verified.
    Complete,
    /// Wiki absence was positively confirmed.
    Absent,
    /// Required content was incomplete.
    Incomplete,
    /// Execution failed before completeness could be established.
    Failed,
}

impl CollectorDiagnosticOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Absent => "absent",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
        }
    }
}

/// Returns the only fields permitted on LFS/wiki collector metrics and events.
#[must_use]
pub fn collector_diagnostic_fields(
    target_id: &str,
    collector: CollectorDiagnosticKind,
    outcome: CollectorDiagnosticOutcome,
    failure_class: Option<&'static str>,
) -> Vec<(&'static str, String)> {
    vec![
        ("target_id", target_id.to_owned()),
        ("collector", collector.as_str().to_owned()),
        ("outcome", outcome.as_str().to_owned()),
        ("failure_class", failure_class.unwrap_or("none").to_owned()),
    ]
}

/// Emits one bounded terminal collector observation without repository/provider text labels.
pub fn record_collector_attempt(
    target_id: &str,
    collector: CollectorDiagnosticKind,
    outcome: CollectorDiagnosticOutcome,
    failure_class: Option<&'static str>,
    duration: std::time::Duration,
    objects: u64,
    bytes: u64,
) {
    let target = target_id.to_owned();
    let collector = collector.as_str().to_owned();
    let outcome = outcome.as_str().to_owned();
    let failure = failure_class.unwrap_or("none").to_owned();
    ::metrics::counter!(VAULT_COLLECTOR_ATTEMPTS_TOTAL,
        "target_id" => target.clone(), "collector" => collector.clone(),
        "outcome" => outcome.clone(), "failure_class" => failure.clone())
    .increment(1);
    ::metrics::histogram!(VAULT_COLLECTOR_DURATION_SECONDS,
        "target_id" => target.clone(), "collector" => collector,
        "outcome" => outcome, "failure_class" => failure)
    .record(duration.as_secs_f64());
    if objects > 0 {
        ::metrics::counter!(VAULT_LFS_OBJECTS_TOTAL, "target_id" => target.clone())
            .increment(objects);
    }
    if bytes > 0 {
        ::metrics::counter!(VAULT_LFS_BYTES_TOTAL, "target_id" => target).increment(bytes);
    }
}

/// Closed terminal outcome field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationDiagnosticOutcome {
    /// Remote bytes verified and placement committed.
    Verified,
    /// Bounded attempt failed.
    Failed,
}

impl ReplicationDiagnosticOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }
}

/// Closed failure field; provider-controlled text cannot enter telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationDiagnosticFailure {
    /// No failure occurred.
    None,
    /// Local bytes were unavailable or invalid.
    Local,
    /// Remote endpoint was unavailable.
    Remote,
    /// Content-addressed object was absent.
    RemoteAbsent,
    /// Transfer deadline expired.
    Timeout,
    /// Re-downloaded bytes did not match.
    Checksum,
    /// Remote body length violated the immutable reference.
    Length,
    /// Configured finite transfer ceiling was exceeded.
    SizeLimit,
    /// Cooperative shutdown cancelled the attempt.
    Cancelled,
}

impl ReplicationDiagnosticFailure {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Local => "local",
            Self::Remote => "remote",
            Self::RemoteAbsent => "remote_absent",
            Self::Timeout => "timeout",
            Self::Checksum => "checksum",
            Self::Length => "length",
            Self::SizeLimit => "size_limit",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Returns the only fields permitted on replication metrics and diagnostic events.
#[must_use]
pub fn replication_diagnostic_fields(
    target_id: &str,
    outcome: ReplicationDiagnosticOutcome,
    failure: Option<ReplicationDiagnosticFailure>,
) -> Vec<(&'static str, String)> {
    vec![
        ("target_id", target_id.to_owned()),
        ("outcome", outcome.as_str().to_owned()),
        (
            "failure_class",
            failure
                .unwrap_or(ReplicationDiagnosticFailure::None)
                .as_str()
                .to_owned(),
        ),
    ]
}

/// Emits one terminal bounded attempt using only the closed diagnostic vocabulary.
pub fn record_replication_attempt(
    target_id: &str,
    outcome: ReplicationDiagnosticOutcome,
    failure: Option<ReplicationDiagnosticFailure>,
    duration: std::time::Duration,
    verified_bytes: u64,
) {
    let target = target_id.to_owned();
    let outcome = outcome.as_str().to_owned();
    let failure = failure
        .unwrap_or(ReplicationDiagnosticFailure::None)
        .as_str()
        .to_owned();
    ::metrics::counter!(
        VAULT_REPLICATION_ATTEMPTS_TOTAL,
        "target_id" => target.clone(),
        "outcome" => outcome.clone(),
        "failure_class" => failure.clone(),
    )
    .increment(1);
    ::metrics::histogram!(
        VAULT_REPLICATION_DURATION_SECONDS,
        "target_id" => target.clone(),
        "outcome" => outcome,
        "failure_class" => failure,
    )
    .record(duration.as_secs_f64());
    if verified_bytes > 0 {
        ::metrics::counter!(VAULT_REPLICATION_BYTES_TOTAL, "target_id" => target)
            .increment(verified_bytes);
    }
}

/// Emits the finite planner window without artifact or repository identity labels.
#[expect(
    clippy::cast_precision_loss,
    reason = "Prometheus gauges are f64; bounded integer values remain authoritative in persistence"
)]
pub fn record_replication_backlog(target_id: &str, items: usize, bytes: u64) {
    let target = target_id.to_owned();
    ::metrics::gauge!(VAULT_REPLICATION_BACKLOG_ITEMS, "target_id" => target.clone())
        .set(items as f64);
    ::metrics::gauge!(VAULT_REPLICATION_BACKLOG_BYTES, "target_id" => target).set(bytes as f64);
}

/// Emits the age of the oldest required verified placement for one target.
pub fn record_last_verification_age(target_id: &str, age: std::time::Duration) {
    ::metrics::gauge!(
        VAULT_REPLICA_LAST_VERIFICATION_AGE_SECONDS,
        "target_id" => target_id.to_owned()
    )
    .set(age.as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replication_diagnostics_use_closed_fields_and_never_render_secrets() {
        const CANARY: &str = "canary-access-secret-endpoint";
        let fields = replication_diagnostic_fields(
            "018f4f7b-7b4a-7000-8000-000000000001",
            ReplicationDiagnosticOutcome::Failed,
            Some(ReplicationDiagnosticFailure::Timeout),
        );
        assert_eq!(
            fields.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["target_id", "outcome", "failure_class"]
        );
        let rendered = format!(
            "{fields:?} {VAULT_REPLICATION_BACKLOG_ITEMS} {VAULT_REPLICATION_BACKLOG_BYTES} \
             {VAULT_REPLICATION_ATTEMPTS_TOTAL} {VAULT_REPLICATION_DURATION_SECONDS} \
             {VAULT_REPLICATION_BYTES_TOTAL} {VAULT_REPLICA_LAST_VERIFICATION_AGE_SECONDS}"
        );
        assert!(!rendered.contains(CANARY));
        assert!(!rendered.contains("http"));
        assert!(!rendered.contains("access_key"));
        assert!(rendered.contains("vault_replication_attempts_total"));
        assert!(rendered.contains("timeout"));
    }

    #[test]
    fn lfs_and_wiki_metrics_are_bounded_and_never_label_repository_names() {
        let lfs = collector_diagnostic_fields(
            "018f4f7b-7b4a-7000-8000-000000000001",
            CollectorDiagnosticKind::GitLfs,
            CollectorDiagnosticOutcome::Incomplete,
            Some("vault.lfs.incomplete"),
        );
        let wiki = collector_diagnostic_fields(
            "018f4f7b-7b4a-7000-8000-000000000002",
            CollectorDiagnosticKind::Wiki,
            CollectorDiagnosticOutcome::Absent,
            None,
        );
        assert_eq!(
            lfs.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["target_id", "collector", "outcome", "failure_class"]
        );
        let rendered = format!(
            "{lfs:?}{wiki:?}{VAULT_COLLECTOR_ATTEMPTS_TOTAL}{VAULT_LFS_OBJECTS_TOTAL}\
             {VAULT_LFS_BYTES_TOTAL}{VAULT_COLLECTOR_DURATION_SECONDS}"
        );
        assert!(!rendered.contains("owner/repository"));
        assert!(!rendered.contains("github.com"));
        assert!(rendered.contains("git_lfs"));
        assert!(rendered.contains("wiki"));
    }
}

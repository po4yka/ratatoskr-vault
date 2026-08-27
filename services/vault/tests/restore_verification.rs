//! Bundle verification, scheduling, and isolated restore-drill acceptance tests.

#![allow(clippy::expect_used, reason = "test assertions")]

#[path = "../../../crates/blobstore/tests/support/mod.rs"]
#[allow(
    dead_code,
    reason = "shared fixture exposes assertions used by blobstore tests"
)]
mod s3_support;

use ratatoskr_vault::restore_verification::{
    ArtifactVerifier, DeferralReason, ReplicaRestoreCandidate, ReportOutcome, RestoreDrill,
    RestoreDrillSettings, RestoreSource, RestoreSourcePolicy, ScheduleCandidate,
    VerificationFailure, VerificationPolicy, plan_due_snapshots, select_restore_source,
};

#[test]
fn replica_aware_drill_selection_prefers_complete_verified_replica() {
    let older = Uuid::from_u128(1);
    let preferred = Uuid::from_u128(2);
    let incomplete = Uuid::from_u128(3);
    let selected = select_restore_source(
        RestoreSourcePolicy::ReplicaPreferred,
        true,
        100,
        &[
            ReplicaRestoreCandidate {
                replica_target_id: older,
                complete: true,
                verified_at: 120,
            },
            ReplicaRestoreCandidate {
                replica_target_id: incomplete,
                complete: false,
                verified_at: 200,
            },
            ReplicaRestoreCandidate {
                replica_target_id: preferred,
                complete: true,
                verified_at: 180,
            },
        ],
    )
    .expect("a complete fresh replica is eligible");

    assert_eq!(
        selected,
        RestoreSource::Replica {
            replica_target_id: preferred
        }
    );
}

#[test]
fn replica_required_selection_never_falls_back_to_local() {
    let result = select_restore_source(
        RestoreSourcePolicy::ReplicaRequired,
        true,
        100,
        &[
            ReplicaRestoreCandidate {
                replica_target_id: Uuid::from_u128(1),
                complete: false,
                verified_at: 200,
            },
            ReplicaRestoreCandidate {
                replica_target_id: Uuid::from_u128(2),
                complete: true,
                verified_at: 99,
            },
        ],
    );

    assert!(
        result.is_err(),
        "replica_required must defer explicitly instead of substituting local bytes: {result:?}"
    );
}

#[tokio::test]
async fn verified_replica_bundle_restores_exact_refs_from_downloaded_bytes() {
    let fixture = S3Fixture::start().await;
    let root = temporary_root();
    let source = create_source_repository(&root);
    let bundle_source = root.join("replica.bundle");
    git(
        &["bundle", "create", path_text(&bundle_source), "--all"],
        &source,
    );
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("fixture store must initialize");
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let lfs_blob = publish_blob(
        &store,
        &root,
        "replica-lfs-object",
        b"replica-only-lfs-object",
        "application/octet-stream",
    );
    let signer = ManifestSigningKey::from_seed([21; 32]).expect("fixture signing key");
    let refs = git_output(&["show-ref"], &source)
        .lines()
        .map(|line| {
            let (oid, name) = line.split_once(' ').expect("show-ref fixture line");
            RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            }
        })
        .collect();
    let manifest = SnapshotManifest::new(
        refs,
        vec![bundle.clone()],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        Some(LfsEvidence::new(
            "git-lfs/fixture".to_owned(),
            vec![LfsObjectEvidence {
                oid: lfs_blob.sha256.clone(),
                blob: lfs_blob.clone(),
            }],
        )),
        &signer,
    )
    .expect("signed fixture manifest");
    let manifest_ref = publish_manifest(&store, &root, &manifest);
    let verification = ArtifactVerifier::new(store.clone(), vec![signer.verification_key()], 16)
        .verify(Uuid::now_v7(), manifest_ref.clone());
    assert_eq!(verification.outcome, ReportOutcome::Passed);
    let replica_target_id = Uuid::now_v7();
    let replica = ReplicaStore::new("offsite".to_owned(), replica_target(fixture.endpoint()))
        .expect("fixture replica store");
    replicate_and_remove_local(&replica, &store, [&bundle, &manifest_ref, &lfs_blob]).await;
    let live_mirror_root = root.join("live-mirrors");
    std::fs::create_dir_all(&live_mirror_root).expect("live mirror root");
    let drill = RestoreDrill::new(
        RestoreDrillSettings {
            scratch_root: root.join("restore-scratch"),
            live_mirror_root,
            git_binary: std::path::PathBuf::from("/usr/bin/git"),
            deadline: std::time::Duration::from_secs(30),
        },
        store,
    )
    .expect("fixture drill settings");

    let report = drill
        .run_from_replica(&verification, replica_target_id, &replica)
        .await;

    assert_eq!(report.outcome, ReportOutcome::Passed);
    assert_eq!(report.source, RestoreSource::Replica { replica_target_id });
    assert_eq!(
        report.expected_ref_set_sha256,
        report.observed_ref_set_sha256
    );
    assert_eq!(report.expected_ref_count, report.observed_ref_count);
    assert_eq!(report.lfs_restored, Some(true));
    assert_eq!(report.observed_lfs_object_count, Some(1));
}
use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_blobstore::replica::ReplicaStore;
use ratatoskr_vault_core::config::ReplicaTargetConfig;
use ratatoskr_vault_core::snapshot::{
    LfsEvidence, LfsObjectEvidence, ManifestSigningKey, RefEvidence, SnapshotManifest,
};
use s3_support::S3Fixture;
use secrecy::SecretString;
use url::Url;
use uuid::Uuid;

#[test]
fn schedule_selects_oldest_due_snapshots_within_sample_and_byte_budgets() {
    let never_verified = Uuid::from_u128(1);
    let oldest = Uuid::from_u128(2);
    let smaller = Uuid::from_u128(3);
    let fresh = Uuid::from_u128(4);
    let policy = VerificationPolicy {
        verification_frequency_seconds: 100,
        drill_frequency_seconds: 200,
        sample_size: 3,
        scratch_byte_budget: 100,
        max_concurrent: 2,
        per_drill_timeout_seconds: 30,
    };
    let plan = plan_due_snapshots(
        1_000,
        policy,
        vec![
            ScheduleCandidate {
                snapshot_id: fresh,
                bundle_size_bytes: 1,
                last_verified_at: Some(950),
                last_drilled_at: Some(900),
            },
            ScheduleCandidate {
                snapshot_id: smaller,
                bundle_size_bytes: 40,
                last_verified_at: Some(100),
                last_drilled_at: Some(100),
            },
            ScheduleCandidate {
                snapshot_id: oldest,
                bundle_size_bytes: 50,
                last_verified_at: Some(0),
                last_drilled_at: Some(0),
            },
            ScheduleCandidate {
                snapshot_id: never_verified,
                bundle_size_bytes: 60,
                last_verified_at: None,
                last_drilled_at: None,
            },
        ],
    )
    .expect("positive fixture policy must be valid");

    assert_eq!(plan.selected, vec![never_verified, smaller]);
    assert_eq!(plan.drill_selected, vec![never_verified, smaller]);
    assert!(plan.deferred.iter().any(|deferred| {
        deferred.snapshot_id == oldest && deferred.reason == DeferralReason::ScratchBudget
    }));
    assert!(
        plan.deferred
            .iter()
            .all(|deferred| deferred.snapshot_id != fresh),
        "fresh snapshots are not deferred because they are not due"
    );
}

#[test]
fn stored_bundle_hash_mismatch_produces_complete_failed_verification_report() {
    let root = temporary_root();
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("fixture store must initialize");
    let bundle_source = root.join("snapshot.bundle");
    std::fs::write(&bundle_source, b"bundle-bytes").expect("fixture bundle must be writable");
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let signer = ManifestSigningKey::from_seed([11; 32]).expect("fixture signing key");
    let manifest = SnapshotManifest::new(
        vec![RefEvidence {
            name: "refs/heads/main".to_owned(),
            oid: "a".repeat(40),
        }],
        vec![bundle.clone()],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        None,
        &signer,
    )
    .expect("signed fixture manifest");
    let manifest_source = root.join("manifest.json");
    std::fs::write(
        &manifest_source,
        serde_json::to_vec(&manifest).expect("manifest serialization"),
    )
    .expect("manifest fixture must be writable");
    let manifest_ref = store
        .reference_for_file(&manifest_source, "application/json".to_owned())
        .expect("manifest reference");
    store
        .publish_file(&manifest_ref, &manifest_source)
        .expect("manifest publication");
    std::fs::write(
        store.resolve(&bundle).expect("stored bundle path"),
        b"broken-bytes",
    )
    .expect("test corruption injection");
    let snapshot_id = Uuid::now_v7();
    let report = ArtifactVerifier::new(store, vec![signer.verification_key()], 16)
        .verify(snapshot_id, manifest_ref.clone());

    assert_eq!(report.snapshot_id, snapshot_id);
    assert_eq!(report.manifest, manifest_ref);
    assert_eq!(report.outcome, ReportOutcome::Failed);
    assert_eq!(report.failure, Some(VerificationFailure::HashMismatch));
    assert_eq!(report.expected_ref_count, 1);
    assert_eq!(report.expected_ref_set_sha256, manifest.ref_set_sha256);
    assert_eq!(report.checked_artifacts, vec![bundle]);
    assert!(report.finished_at >= report.started_at);
    assert!(report.stages.iter().any(|stage| {
        stage.stage == "bundle_hash" && !stage.passed && stage.duration <= report.duration
    }));
}

#[test]
fn corrupt_stored_lfs_object_fails_without_live_fallback() {
    let root = temporary_root();
    let source = create_source_repository(&root);
    let bundle_source = root.join("lfs-corruption.bundle");
    git(
        &["bundle", "create", path_text(&bundle_source), "--all"],
        &source,
    );
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("fixture store must initialize");
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let object_source = root.join("lfs-object");
    std::fs::write(&object_source, b"immutable-lfs-bytes").expect("LFS object");
    let object = store
        .reference_for_file(&object_source, "application/octet-stream".to_owned())
        .expect("LFS reference");
    store
        .publish_file(&object, &object_source)
        .expect("LFS publication");
    let refs = git_output(&["show-ref"], &source)
        .lines()
        .map(|line| {
            let (oid, name) = line.split_once(' ').expect("show-ref line");
            RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            }
        })
        .collect();
    let signer = ManifestSigningKey::from_seed([31; 32]).expect("signing key");
    let manifest = SnapshotManifest::new(
        refs,
        vec![bundle],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        Some(LfsEvidence::new(
            "git-lfs/fixture".to_owned(),
            vec![LfsObjectEvidence {
                oid: object.sha256.clone(),
                blob: object.clone(),
            }],
        )),
        &signer,
    )
    .expect("signed LFS manifest");
    let manifest_ref = publish_manifest(&store, &root, &manifest);
    std::fs::write(store.resolve(&object).expect("stored LFS path"), b"corrupt")
        .expect("corruption injection");

    let report = ArtifactVerifier::new(store, vec![signer.verification_key()], 16)
        .verify(Uuid::now_v7(), manifest_ref);

    assert_eq!(report.outcome, ReportOutcome::Failed);
    assert_eq!(report.failure, Some(VerificationFailure::LfsInvalid));
    assert!(
        report
            .stages
            .iter()
            .any(|stage| stage.stage == "lfs_object_hash" && !stage.passed)
    );
}

#[tokio::test]
async fn valid_bundle_restores_exact_manifest_refs_without_live_mirror_access() {
    let root = temporary_root();
    let source = create_source_repository(&root);
    let bundle_source = root.join("snapshot.bundle");
    git(
        &["bundle", "create", path_text(&bundle_source), "--all"],
        &source,
    );
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("fixture store must initialize");
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let signer = ManifestSigningKey::from_seed([12; 32]).expect("fixture signing key");
    let refs = git_output(&["show-ref"], &source)
        .lines()
        .map(|line| {
            let (oid, name) = line.split_once(' ').expect("show-ref fixture line");
            RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            }
        })
        .collect();
    let manifest = SnapshotManifest::new(
        refs,
        vec![bundle],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        None,
        &signer,
    )
    .expect("signed fixture manifest");
    let manifest_ref = publish_manifest(&store, &root, &manifest);
    let snapshot_id = Uuid::now_v7();
    let verification = ArtifactVerifier::new(store.clone(), vec![signer.verification_key()], 16)
        .verify(snapshot_id, manifest_ref.clone());
    assert_eq!(verification.outcome, ReportOutcome::Passed);
    let live_mirror_root = root.join("live-mirrors");
    std::fs::create_dir_all(&live_mirror_root).expect("live mirror root");
    let sentinel = live_mirror_root.join("sentinel");
    std::fs::write(&sentinel, b"untouched").expect("live sentinel");
    let scratch_root = root.join("restore-scratch");
    let drill = RestoreDrill::new(
        RestoreDrillSettings {
            scratch_root: scratch_root.clone(),
            live_mirror_root,
            git_binary: std::path::PathBuf::from("/usr/bin/git"),
            deadline: std::time::Duration::from_secs(30),
        },
        store,
    )
    .expect("fixture drill settings");

    let report = drill.run(&verification).await;

    assert_eq!(report.outcome, ReportOutcome::Passed);
    assert_eq!(report.failure, None);
    assert_eq!(report.snapshot_id, snapshot_id);
    assert_eq!(report.manifest, manifest_ref);
    assert_eq!(report.expected_ref_count, report.observed_ref_count);
    assert_eq!(
        report.expected_ref_set_sha256,
        report.observed_ref_set_sha256
    );
    assert!(report.network_disabled);
    assert!(!report.live_mirror_accessed);
    assert!(report.finished_at >= report.started_at);
    for stage in [
        "init",
        "bundle_verify",
        "fetch",
        "fsck",
        "show_ref",
        "ref_compare",
    ] {
        assert!(report.stages.iter().any(|item| {
            item.stage == stage && item.passed && item.duration <= report.duration
        }));
    }
    assert_eq!(
        std::fs::read(sentinel).expect("live sentinel"),
        b"untouched"
    );
    assert!(
        !scratch_root.join("runs").exists()
            || std::fs::read_dir(scratch_root.join("runs"))
                .expect("runs directory")
                .next()
                .is_none(),
        "drill-owned scratch must be cleaned"
    );
}

#[tokio::test]
async fn restore_ref_mismatch_is_failed_and_live_mirror_remains_unchanged() {
    let root = temporary_root();
    let source = create_source_repository(&root);
    let bundle_source = root.join("mismatch.bundle");
    git(
        &["bundle", "create", path_text(&bundle_source), "--all"],
        &source,
    );
    let store =
        LocalBlobStore::new(root.join("blobs"), 1_000_000).expect("fixture store must initialize");
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let signer = ManifestSigningKey::from_seed([13; 32]).expect("fixture signing key");
    let mut refs: Vec<_> = git_output(&["show-ref"], &source)
        .lines()
        .map(|line| {
            let (oid, name) = line.split_once(' ').expect("show-ref fixture line");
            RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            }
        })
        .collect();
    refs.first_mut().expect("fixture has refs").oid = "f".repeat(40);
    let manifest = SnapshotManifest::new(
        refs,
        vec![bundle],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        None,
        &signer,
    )
    .expect("signed fixture manifest");
    let manifest_ref = publish_manifest(&store, &root, &manifest);
    let snapshot_id = Uuid::now_v7();
    let verification = ArtifactVerifier::new(store.clone(), vec![signer.verification_key()], 16)
        .verify(snapshot_id, manifest_ref);
    assert_eq!(verification.outcome, ReportOutcome::Passed);
    let live_mirror_root = root.join("live-mirrors");
    std::fs::create_dir_all(&live_mirror_root).expect("live mirror root");
    let sentinel = live_mirror_root.join("sentinel");
    std::fs::write(&sentinel, b"untouched").expect("live sentinel");
    let scratch_root = root.join("restore-scratch");
    let drill = RestoreDrill::new(
        RestoreDrillSettings {
            scratch_root: scratch_root.clone(),
            live_mirror_root,
            git_binary: std::path::PathBuf::from("/usr/bin/git"),
            deadline: std::time::Duration::from_secs(30),
        },
        store,
    )
    .expect("fixture drill settings");

    let report = drill.run(&verification).await;

    assert_eq!(report.outcome, ReportOutcome::Failed);
    assert_eq!(report.failure, Some(VerificationFailure::RefMismatch));
    assert_eq!(report.observed_ref_count, 2);
    assert_ne!(
        report.expected_ref_set_sha256,
        report.observed_ref_set_sha256
    );
    assert!(report.stages.iter().any(|stage| {
        stage.stage == "ref_compare" && !stage.passed && stage.duration <= report.duration
    }));
    assert_eq!(
        std::fs::read(sentinel).expect("live sentinel"),
        b"untouched"
    );
    assert!(
        !scratch_root.join("runs").exists()
            || std::fs::read_dir(scratch_root.join("runs"))
                .expect("runs directory")
                .next()
                .is_none()
    );
}

fn publish_manifest(
    store: &LocalBlobStore,
    root: &std::path::Path,
    manifest: &SnapshotManifest,
) -> ratatoskr_vault_core::snapshot::BlobRef {
    let source = root.join(format!("manifest-{}.json", Uuid::now_v7()));
    std::fs::write(
        &source,
        serde_json::to_vec(manifest).expect("manifest serialization"),
    )
    .expect("manifest fixture must be writable");
    let reference = store
        .reference_for_file(&source, "application/json".to_owned())
        .expect("manifest reference");
    store
        .publish_file(&reference, &source)
        .expect("manifest publication");
    reference
}

fn publish_blob(
    store: &LocalBlobStore,
    root: &std::path::Path,
    name: &str,
    bytes: &[u8],
    media_type: &str,
) -> ratatoskr_vault_core::snapshot::BlobRef {
    let source = root.join(name);
    std::fs::write(&source, bytes).expect("fixture blob source");
    let reference = store
        .reference_for_file(&source, media_type.to_owned())
        .expect("fixture blob reference");
    store
        .publish_file(&reference, &source)
        .expect("fixture blob publication");
    reference
}

async fn replicate_and_remove_local<const N: usize>(
    replica: &ReplicaStore,
    store: &LocalBlobStore,
    references: [&ratatoskr_vault_core::snapshot::BlobRef; N],
) {
    for reference in references {
        let source = store.resolve(reference).expect("local replica source");
        replica
            .upload_and_verify(reference, &source)
            .await
            .expect("replicated fixture object");
        std::fs::remove_file(source).expect("remove local replica source");
    }
}

fn replica_target(endpoint: &str) -> ReplicaTargetConfig {
    ReplicaTargetConfig {
        endpoint: Url::parse(endpoint).expect("fixture endpoint"),
        bucket: "vault-fixtures".to_owned(),
        region: "fixture-1".to_owned(),
        key_prefix: "restore".to_owned(),
        enabled: true,
        required: true,
        access_key: SecretString::from("fixture-access"),
        secret_access_key: SecretString::from("fixture-secret"),
        session_token: None,
        connect_timeout_seconds: 2,
        request_timeout_seconds: 10,
        attempt_timeout_seconds: 5,
        max_object_bytes: 1_000_000,
        max_backlog_items: 8,
        max_backlog_bytes: 8_000_000,
        max_concurrent: 2,
    }
}

fn create_source_repository(root: &std::path::Path) -> std::path::PathBuf {
    let source = root.join("source-repository");
    std::fs::create_dir_all(&source).expect("source root");
    git(&["init", "-q", "--initial-branch=main"], &source);
    std::fs::write(source.join("README"), b"restorable\n").expect("fixture file");
    git(&["add", "README"], &source);
    git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
        &source,
    );
    git(&["tag", "v1"], &source);
    source
}

fn git(args: &[&str], cwd: &std::path::Path) {
    let output = std::process::Command::new("/usr/bin/git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("fixture Git must start");
    assert!(
        output.status.success(),
        "fixture Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(args: &[&str], cwd: &std::path::Path) -> String {
    let output = std::process::Command::new("/usr/bin/git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("fixture Git must start");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("Git output must be UTF-8")
}

fn path_text(path: &std::path::Path) -> &str {
    path.to_str().expect("fixture paths must be UTF-8")
}

fn temporary_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("vault-restore-verification-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("temporary root must be creatable");
    root
}

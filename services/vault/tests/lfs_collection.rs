//! Real local Git LFS collection into immutable Vault blobs.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatoskr_vault::lfs_collection::{LfsCollectionError, LfsCollectionSettings, LfsCollector};
use ratatoskr_vault::mirror_lifecycle::{MirrorLifecycle, MirrorLifecycleSettings, MirrorRequest};
use ratatoskr_vault::restore_verification::{
    ArtifactVerifier, ReportOutcome, RestoreDrill, RestoreDrillSettings,
};
use ratatoskr_vault_blobstore::LocalBlobStore;
use ratatoskr_vault_core::snapshot::{ManifestSigningKey, RefEvidence, SnapshotManifest};
use ratatoskr_vault_gitrunner::LfsTransferMode;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

fn git_binary() -> PathBuf {
    ["/usr/bin/git", "/opt/homebrew/bin/git"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .expect("Git is installed")
}

fn lfs_binary() -> PathBuf {
    [
        "/opt/homebrew/bin/git-lfs",
        "/usr/local/bin/git-lfs",
        "/usr/bin/git-lfs",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
    .expect("Git LFS is a gate prerequisite")
}

fn git(cwd: &Path, args: &[&str]) {
    let path = format!(
        "{}:/usr/bin:/bin",
        lfs_binary()
            .parent()
            .expect("LFS binary directory")
            .display()
    );
    let output = std::process::Command::new(git_binary())
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("fixture Git starts");
    assert!(
        output.status.success(),
        "fixture Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(root: &Path) -> (PathBuf, Vec<Vec<u8>>) {
    let source = root.join("source");
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&source).expect("source directory");
    git(&source, &["init", "-q", "--initial-branch=main"]);
    git(&source, &["lfs", "install", "--local"]);
    git(&source, &["lfs", "track", "*.bin"]);
    let first = b"first immutable LFS object\n".to_vec();
    let second = b"second immutable LFS object on another ref\n".to_vec();
    std::fs::write(source.join("asset.bin"), &first).expect("first LFS object");
    git(&source, &["add", ".gitattributes", "asset.bin"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "first",
        ],
    );
    git(&source, &["checkout", "-q", "-b", "archive"]);
    std::fs::write(source.join("asset.bin"), &second).expect("second LFS object");
    git(&source, &["add", "asset.bin"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "second",
        ],
    );
    git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            remote.to_str().expect("UTF-8 remote"),
        ],
    );
    let remote_url = format!("file://{}", remote.display());
    git(&source, &["remote", "add", "origin", &remote_url]);
    git(&source, &["push", "-q", "origin", "--all"]);
    git(&source, &["lfs", "push", "--all", "origin"]);
    (remote, vec![first, second])
}

#[tokio::test]
async fn enabled_fixture_collects_every_reachable_lfs_object_once() {
    let root = std::env::temp_dir().join(format!("vault-lfs-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (remote, contents) = fixture(&root);
    let mirror_root = root.join("mirrors-root");
    let relative = PathBuf::from("mirrors/aa/repository.git");
    let mirror = mirror_root.join(&relative);
    std::fs::create_dir_all(mirror.parent().expect("mirror parent")).expect("mirror parent");
    git(
        &root,
        &[
            "clone",
            "-q",
            "--mirror",
            &format!("file://{}", remote.display()),
            mirror.to_str().expect("UTF-8 mirror"),
        ],
    );
    let store = LocalBlobStore::new(root.join("blobs"), 1024 * 1024).expect("blob store");
    let collector = LfsCollector::new(
        LfsCollectionSettings {
            lfs_binary: lfs_binary(),
            mirror_root,
            work_root: root.join("work"),
            stage_max_bytes: 1024 * 1024,
            max_objects: 16,
            deadline: Duration::from_secs(10),
            transfer_mode: LfsTransferMode::StandaloneFile,
        },
        store.clone(),
    )
    .expect("collector settings");

    let collection = collector
        .collect(&relative)
        .await
        .expect("complete collection");
    let mut expected: Vec<String> = contents
        .iter()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
        .collect();
    expected.sort();
    let observed: Vec<_> = collection
        .evidence
        .objects
        .iter()
        .map(|object| object.oid.clone())
        .collect();
    assert_eq!(
        observed, expected,
        "every reachable object is collected once"
    );
    assert_eq!(
        collection.evidence.total_bytes,
        contents.iter().map(Vec::len).sum::<usize>() as u64
    );
    for object in &collection.evidence.objects {
        store
            .verify(&object.blob)
            .expect("published object verifies");
    }

    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn lfs_over_quota_fails_without_publishing_objects() {
    let root = std::env::temp_dir().join(format!("vault-lfs-quota-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (remote, _) = fixture(&root);
    let mirror_root = root.join("mirrors-root");
    let relative = PathBuf::from("mirrors/aa/repository.git");
    let mirror = mirror_root.join(&relative);
    std::fs::create_dir_all(mirror.parent().expect("mirror parent")).expect("mirror parent");
    git(
        &root,
        &[
            "clone",
            "-q",
            "--mirror",
            &format!("file://{}", remote.display()),
            mirror.to_str().expect("UTF-8 mirror"),
        ],
    );
    let blob_root = root.join("blobs");
    let store = LocalBlobStore::new(blob_root.clone(), 1024 * 1024).expect("blob store");
    let collector = collector(&root, mirror_root, store, 1);

    let failure = collector
        .collect(&relative)
        .await
        .expect_err("LFS staging exceeds the admitted limit");
    assert!(matches!(failure, LfsCollectionError::QuotaExceeded));
    assert_eq!(regular_file_count(&blob_root), 0, "no object was published");
    assert_eq!(
        std::fs::read_dir(root.join("work").join("runs")).map_or(0, std::iter::Iterator::count),
        0,
        "the failed run leaves no run-owned child"
    );
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn lfs_enabled_fixture_collects_manifests_and_restores_without_source_or_mirror() {
    let root = std::env::temp_dir().join(format!("vault-lfs-restore-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (remote, contents) = fixture(&root);
    let (mirror_root, relative, mirror) = clone_mirror(&root, &remote);
    let store = LocalBlobStore::new(root.join("blobs"), 1024 * 1024).expect("blob store");
    let collection = collector(&root, mirror_root.clone(), store.clone(), 1024 * 1024)
        .collect(&relative)
        .await
        .expect("complete LFS collection");

    let (bundle, refs) = publish_bundle_and_refs(&root, &mirror, &store);
    let signer = ManifestSigningKey::from_seed([42; 32]).expect("signing key");
    let manifest = SnapshotManifest::new(
        refs,
        vec![bundle],
        None,
        "2026-08-27T00:00:00Z".to_owned(),
        Some(collection.evidence.clone()),
        &signer,
    )
    .expect("LFS manifest");
    let manifest_source = root.join("manifest.json");
    std::fs::write(
        &manifest_source,
        serde_json::to_vec(&manifest).expect("manifest JSON"),
    )
    .expect("manifest source");
    let manifest_ref = store
        .reference_for_file(&manifest_source, "application/json".to_owned())
        .expect("manifest reference");
    store
        .publish_file(&manifest_ref, &manifest_source)
        .expect("manifest publication");
    let verification = ArtifactVerifier::new(store.clone(), vec![signer.verification_key()], 16)
        .verify(Uuid::now_v7(), manifest_ref);
    assert_eq!(verification.outcome, ReportOutcome::Passed);

    std::fs::remove_dir_all(root.join("source")).expect("remove source");
    std::fs::remove_dir_all(remote).expect("remove remote");
    std::fs::remove_dir_all(&mirror_root).expect("remove live mirror");
    std::fs::create_dir_all(&mirror_root).expect("empty denied live root");
    let drill = RestoreDrill::new(
        RestoreDrillSettings {
            scratch_root: root.join("restore"),
            live_mirror_root: mirror_root,
            git_binary: git_binary(),
            deadline: Duration::from_secs(30),
        },
        store,
    )
    .expect("restore settings");
    let report = drill.run(&verification).await;

    assert_eq!(report.outcome, ReportOutcome::Passed);
    assert_eq!(report.lfs_restored, Some(true));
    assert_eq!(report.observed_lfs_object_count, Some(contents.len()));
    assert_eq!(
        report.expected_lfs_aggregate_sha256,
        report.observed_lfs_aggregate_sha256
    );
    assert!(
        report
            .stages
            .iter()
            .any(|stage| stage.stage == "lfs_objects" && stage.passed)
    );
    assert!(report.network_disabled);
    assert!(!report.live_mirror_accessed);
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn explicit_lfs_policy_controls_collection_and_quota_degrades_truthfully() {
    let fixture_db = TestDatabase::create().await.expect("disposable database");
    let root = std::env::temp_dir().join(format!("vault-lfs-policy-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (remote, _) = fixture(&root);
    let published = root.join("published");
    let blob_root = root.join("blobs");
    let store = LocalBlobStore::new(blob_root.clone(), 1024 * 1024).expect("blob store");
    let lifecycle = MirrorLifecycle::new(
        fixture_db.database.clone(),
        MirrorLifecycleSettings::for_test(
            published.clone(),
            root.join("mirror-work"),
            git_binary(),
        ),
    )
    .expect("lifecycle")
    .with_lfs_collector(collector(&root, published, store, 1));

    let git_only_target = enrolled_target(&fixture_db, "git-only").await;
    let git_only = lifecycle
        .run(MirrorRequest::new(
            git_only_target,
            format!("file://{}", remote.display()),
            4096,
        ))
        .await
        .expect("Git-only mirror");
    assert!(git_only.is_success());
    assert!(git_only.lfs().is_none());
    let git_only_collectors: i64 =
        sqlx::query_scalar("select count(*) from git_vault.collector_runs where target_id = $1")
            .bind(git_only_target)
            .fetch_one(fixture_db.pool())
            .await
            .expect("collector count");
    assert_eq!(git_only_collectors, 0, "Git-only policy never invokes LFS");

    let lfs_target = enrolled_target(&fixture_db, "lfs-required").await;
    let lfs = lifecycle
        .run(
            MirrorRequest::new(lfs_target, format!("file://{}", remote.display()), 4096).with_lfs(),
        )
        .await
        .expect("terminal LFS outcome");
    assert!(!lfs.is_success());
    assert_eq!(
        lfs.lfs_failure_class(),
        Some("vault.storage.quota_exceeded")
    );
    let terminal: (String, String, Option<String>) = sqlx::query_as(
        "select targets.status, collector_runs.outcome, collector_runs.failure_class
         from git_vault.targets join git_vault.collector_runs using (target_id)
         where targets.target_id = $1",
    )
    .bind(lfs_target)
    .fetch_one(fixture_db.pool())
    .await
    .expect("terminal LFS evidence");
    assert_eq!(
        terminal,
        (
            "degraded".to_owned(),
            "failed".to_owned(),
            Some("vault.storage.quota_exceeded".to_owned())
        )
    );
    assert_eq!(
        regular_file_count(&blob_root),
        0,
        "quota failure publishes no LFS objects"
    );

    fixture_db.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn lfs_reservation_is_part_of_mirror_admission() {
    let fixture_db = TestDatabase::create().await.expect("disposable database");
    let root = std::env::temp_dir().join(format!("vault-lfs-admission-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    let (remote, _) = fixture(&root);
    let published = root.join("published");
    let store = LocalBlobStore::new(root.join("blobs"), 1024 * 1024).expect("blob store");
    let lifecycle = MirrorLifecycle::new(
        fixture_db.database.clone(),
        MirrorLifecycleSettings::for_test(
            published.clone(),
            root.join("mirror-work"),
            git_binary(),
        )
        .with_budgets(4096, 4096),
    )
    .expect("lifecycle")
    .with_lfs_collector(collector(&root, published, store, 1));
    let target_id = enrolled_target(&fixture_db, "lfs-admission").await;

    let outcome = lifecycle
        .run(MirrorRequest::new(target_id, format!("file://{}", remote.display()), 4096).with_lfs())
        .await
        .expect("quota refusal evidence");

    assert_eq!(
        outcome.result(),
        ratatoskr_vault_core::mirror::MirrorResult::QuotaRefused
    );
    assert!(
        !lifecycle
            .mirror_path(target_id)
            .expect("mirror path")
            .exists(),
        "combined Git plus LFS reservation is refused before Git starts"
    );
    fixture_db.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

fn collector(
    root: &Path,
    mirror_root: PathBuf,
    store: LocalBlobStore,
    stage_max_bytes: u64,
) -> LfsCollector {
    LfsCollector::new(
        LfsCollectionSettings {
            lfs_binary: lfs_binary(),
            mirror_root,
            work_root: root.join("work"),
            stage_max_bytes,
            max_objects: 16,
            deadline: Duration::from_secs(10),
            transfer_mode: LfsTransferMode::StandaloneFile,
        },
        store,
    )
    .expect("collector settings")
}

fn regular_file_count(root: &Path) -> usize {
    std::fs::read_dir(root).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    regular_file_count(&path)
                } else {
                    usize::from(path.is_file())
                }
            })
            .sum()
    })
}

fn clone_mirror(root: &Path, remote: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let mirror_root = root.join("mirrors-root");
    let relative = PathBuf::from("mirrors/aa/repository.git");
    let mirror = mirror_root.join(&relative);
    std::fs::create_dir_all(mirror.parent().expect("mirror parent")).expect("mirror parent");
    git(
        root,
        &[
            "clone",
            "-q",
            "--mirror",
            &format!("file://{}", remote.display()),
            mirror.to_str().expect("UTF-8 mirror"),
        ],
    );
    (mirror_root, relative, mirror)
}

fn publish_bundle_and_refs(
    root: &Path,
    mirror: &Path,
    store: &LocalBlobStore,
) -> (ratatoskr_vault_core::snapshot::BlobRef, Vec<RefEvidence>) {
    let bundle_source = root.join("snapshot.bundle");
    git(
        mirror,
        &[
            "bundle",
            "create",
            bundle_source.to_str().expect("UTF-8 bundle"),
            "--all",
        ],
    );
    let bundle = store
        .reference_for_file(&bundle_source, "application/vnd.git.bundle".to_owned())
        .expect("bundle reference");
    store
        .publish_file(&bundle, &bundle_source)
        .expect("bundle publication");
    let output = std::process::Command::new(git_binary())
        .arg("show-ref")
        .current_dir(mirror)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("show-ref");
    assert!(output.status.success());
    let refs = String::from_utf8(output.stdout)
        .expect("UTF-8 refs")
        .lines()
        .map(|line| {
            let (oid, name) = line.split_once(' ').expect("show-ref line");
            RefEvidence {
                name: name.to_owned(),
                oid: oid.to_owned(),
            }
        })
        .collect();
    (bundle, refs)
}

async fn enrolled_target(fixture: &TestDatabase, external_id: &str) -> Uuid {
    let target_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'requested', now(), now())",
    )
    .bind(target_id)
    .bind(external_id)
    .execute(fixture.pool())
    .await
    .expect("target");
    target_id
}

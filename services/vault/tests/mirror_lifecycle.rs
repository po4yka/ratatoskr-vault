//! Real-Git mirror lifecycle integration tests. Fixture setup is local only; no network remote.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::io::{Seek as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ratatoskr_vault::mirror_lifecycle::{MirrorLifecycle, MirrorLifecycleSettings, MirrorRequest};
use ratatoskr_vault_core::mirror::MirrorResult;
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

fn git_binary() -> PathBuf {
    [
        "/usr/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.exists())
    .expect("system Git is installed")
}

fn git(args: &[&str], cwd: &Path) {
    let output = std::process::Command::new(git_binary())
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
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

fn git_stdout(args: &[&str], cwd: &Path) -> String {
    let output = std::process::Command::new(git_binary())
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("fixture Git starts");
    assert!(output.status.success(), "fixture Git {args:?} succeeds");
    String::from_utf8(output.stdout)
        .expect("Git's object id is UTF-8")
        .trim()
        .to_owned()
}

fn source_fixture(root: &Path) -> PathBuf {
    let source = root.join("source");
    std::fs::create_dir_all(&source).expect("source root");
    git(&["init", "-q", "--initial-branch=main"], &source);
    std::fs::write(source.join("README"), "first\n").expect("fixture file");
    git(&["add", "README"], &source);
    git(
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "initial",
        ]
        .as_slice(),
        &source,
    );
    source
}

fn scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vault-mirror-lifecycle-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

async fn enrolled_target(fixture: &TestDatabase) -> Uuid {
    let target_id = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', $2, 'requested', now(), now())",
    )
    .bind(target_id)
    .bind(Uuid::now_v7().simple().to_string())
    .execute(fixture.pool())
    .await
    .expect("target inserts");
    target_id
}

#[tokio::test]
async fn initial_clone_of_local_fixture_publishes_verified_mirror_and_transitions_ready() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let target_id = enrolled_target(&fixture).await;
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary()),
    )
    .expect("valid lifecycle settings");

    let outcome = lifecycle
        .run(MirrorRequest::new(
            target_id,
            format!("file://{}", source.display()),
            4096,
        ))
        .await
        .expect("the local fixture clone succeeds");
    assert!(outcome.is_success());

    let (status, object_count, bytes): (String, i64, i64) = sqlx::query_as(
        "select t.status, r.object_count, r.bytes_on_disk
         from git_vault.targets t join git_vault.mirror_lifecycle_runs r on r.target_id = t.target_id
         where t.target_id = $1",
    )
    .bind(target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("successful evidence");
    assert_eq!(status, "ready");
    assert!(object_count > 0);
    assert!(bytes > 0);
    assert!(lifecycle.mirror_path(target_id).expect("path").exists());

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn per_mirror_quota_refusal_does_not_spawn_or_publish_a_clone() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let target_id = enrolled_target(&fixture).await;
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary())
            .with_budgets(10, 100),
    )
    .expect("lifecycle");

    let outcome = lifecycle
        .run(MirrorRequest::new(
            target_id,
            format!("file://{}", source.display()),
            11,
        ))
        .await
        .expect("quota refusal is a recorded outcome");
    assert_eq!(outcome.result(), MirrorResult::QuotaRefused);
    assert!(!lifecycle.mirror_path(target_id).expect("path").exists());

    let (status, failure): (String, Option<String>) = sqlx::query_as(
        "select t.status, r.failure_class from git_vault.targets t
         join git_vault.mirror_lifecycle_runs r on r.target_id = t.target_id
         where t.target_id = $1",
    )
    .bind(target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("refusal evidence");
    assert_eq!(status, "degraded");
    assert_eq!(failure.as_deref(), Some("vault.storage.quota_exceeded"));

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn cancelled_clone_removes_staging_and_releases_capacity() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let target_id = enrolled_target(&fixture).await;
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary()),
    )
    .expect("lifecycle");
    let (cancel, mut cancellation) = tokio::sync::watch::channel(true);

    let outcome = lifecycle
        .run_cancellable(
            MirrorRequest::new(target_id, format!("file://{}", source.display()), 4096),
            &mut cancellation,
        )
        .await
        .expect("cancellation records an outcome");
    drop(cancel);
    assert!(matches!(outcome.result(), MirrorResult::Interrupted { .. }));
    assert!(!lifecycle.mirror_path(target_id).expect("path").exists());
    assert!(
        !root.join("work").join("runs").exists(),
        "all clone staging is owned by and removed with its run"
    );
    let reservations: i64 = sqlx::query_scalar(
        "select count(*) from git_vault.mirror_quota_reservations where target_id = $1",
    )
    .bind(target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("reservation count");
    assert_eq!(reservations, 0);

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn cancelled_fetch_keeps_prior_mirror_and_next_run_fetches_pending_commit() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let target_id = enrolled_target(&fixture).await;
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary()),
    )
    .expect("lifecycle");
    let request = MirrorRequest::new(target_id, format!("file://{}", source.display()), 4096);
    lifecycle.run(request.clone()).await.expect("initial clone");

    std::fs::write(source.join("README"), "second\n").expect("second source content");
    git(&["add", "README"], &source);
    git(
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "second",
        ]
        .as_slice(),
        &source,
    );
    let expected = git_stdout(&["rev-parse", "HEAD"], &source);
    let (_cancel, mut cancellation) = tokio::sync::watch::channel(true);
    let interrupted = lifecycle
        .run_cancellable(request.clone(), &mut cancellation)
        .await
        .expect("cancelled fetch is recorded");
    assert!(matches!(
        interrupted.result(),
        MirrorResult::Interrupted { .. }
    ));
    let mirror = lifecycle.mirror_path(target_id).expect("mirror path");
    assert!(
        mirror.exists(),
        "the prior verified mirror remains published"
    );

    let checkpoint: String = sqlx::query_scalar(
        "select checkpoint from git_vault.mirror_lifecycle_runs
         where target_id = $1 and outcome = 'interrupted'",
    )
    .bind(target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("fetch checkpoint");
    assert_eq!(checkpoint, "fetch_pending");

    let retried = lifecycle.run(request).await.expect("normal fetch retries");
    assert!(retried.is_success());
    let mirrored = git_stdout(
        [
            "--git-dir",
            mirror.to_str().expect("UTF-8 mirror path"),
            "rev-parse",
            "refs/heads/main",
        ]
        .as_slice(),
        &root,
    );
    assert_eq!(
        mirrored, expected,
        "retry fetched the pending source commit"
    );

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn integrity_check_failure_marks_target_degraded_and_preserves_last_success() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let target_id = enrolled_target(&fixture).await;
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary()),
    )
    .expect("lifecycle");
    let request = MirrorRequest::new(target_id, format!("file://{}", source.display()), 4096);
    lifecycle.run(request.clone()).await.expect("initial clone");
    let mirror = lifecycle.mirror_path(target_id).expect("mirror path");
    let bytes_before: i64 =
        sqlx::query_scalar("select bytes_on_disk from git_vault.mirrors where target_id = $1")
            .bind(target_id)
            .fetch_one(fixture.pool())
            .await
            .expect("prior observation");
    let pack = std::fs::read_dir(mirror.join("objects/pack"))
        .expect("pack directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "pack")
        })
        .expect("fixture mirror has one pack");
    std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o600))
        .expect("test owns its fixture pack");
    let mut corrupt = std::fs::OpenOptions::new()
        .write(true)
        .open(pack)
        .expect("pack opens for intentional corruption");
    corrupt
        .seek(std::io::SeekFrom::Start(0))
        .expect("seek pack");
    corrupt.write_all(&[0]).expect("corrupt one byte");

    let outcome = lifecycle
        .run(request)
        .await
        .expect("integrity failure is recorded");
    assert_eq!(outcome.result(), MirrorResult::IntegrityFailed);
    let (status, fsck, bytes_after, failure): (String, String, i64, Option<String>) =
        sqlx::query_as(
            "select t.status, m.fsck_result, m.bytes_on_disk, r.failure_class
         from git_vault.targets t
         join git_vault.mirrors m on m.target_id = t.target_id
         join git_vault.mirror_lifecycle_runs r on r.target_id = t.target_id
         where t.target_id = $1 and r.outcome = 'integrity_failed'",
        )
        .bind(target_id)
        .fetch_one(fixture.pool())
        .await
        .expect("integrity evidence");
    assert_eq!(status, "degraded");
    assert_eq!(fsck, "failed");
    assert_eq!(
        bytes_after, bytes_before,
        "a failed refresh never replaces the last success"
    );
    assert_eq!(failure.as_deref(), Some("vault.mirror.corrupt"));

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

#[tokio::test]
async fn fifth_mirror_operation_waits_for_one_of_four_admissions() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let source = source_fixture(&root);
    let targets = [
        enrolled_target(&fixture).await,
        enrolled_target(&fixture).await,
        enrolled_target(&fixture).await,
        enrolled_target(&fixture).await,
        enrolled_target(&fixture).await,
    ];
    let (started_send, mut started_receive) = tokio::sync::mpsc::unbounded_channel();
    let (release_send, release_receive) = tokio::sync::watch::channel(false);
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(root.join("published"), root.join("work"), git_binary())
            .with_admission_test_hooks(started_send, release_receive),
    )
    .expect("lifecycle");
    let source_url = format!("file://{}", source.display());
    let mut first_four = Vec::new();
    for target_id in targets.iter().take(4) {
        let lifecycle = lifecycle.clone();
        let request = MirrorRequest::new(*target_id, source_url.clone(), 4096);
        first_four.push(tokio::spawn(async move { lifecycle.run(request).await }));
    }
    for _ in 0..4 {
        started_receive
            .recv()
            .await
            .expect("each admitted operation reports before Git starts");
    }

    let fifth = {
        let lifecycle = lifecycle.clone();
        let request = MirrorRequest::new(targets[4], source_url, 4096);
        tokio::spawn(async move { lifecycle.run(request).await })
    };
    tokio::task::yield_now().await;
    let fifth_status: String =
        sqlx::query_scalar("select status from git_vault.targets where target_id = $1")
            .bind(targets[4])
            .fetch_one(fixture.pool())
            .await
            .expect("fifth target status");
    assert_eq!(
        fifth_status, "requested",
        "the fifth operation has no permit yet"
    );

    release_send.send(true).expect("release receiver lives");

    for task in first_four {
        task.await.expect("task join").expect("first four succeed");
    }
    fifth
        .await
        .expect("fifth join")
        .expect("fifth succeeds after release");

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

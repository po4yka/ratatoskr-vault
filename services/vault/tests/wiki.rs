//! Local wiki sibling discovery and independent mirror acceptance.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatoskr_vault::mirror_lifecycle::{MirrorLifecycle, MirrorLifecycleSettings};
use ratatoskr_vault::wiki::{WikiCoordinator, WikiMirrorOutcome};
use ratatoskr_vault_gitrunner::{GitRunner, RunConfig, Subcommand};
use ratatoskr_vault_persistence::test_support::TestDatabase;
use uuid::Uuid;

#[tokio::test]
async fn present_wiki_is_mirrored_as_an_independent_sibling() {
    let fixture = TestDatabase::create().await.expect("disposable database");
    let root = scratch();
    let parent = seed_parent(&fixture).await;
    let wiki = source_fixture(&root);
    let sibling = root.join("repository.wiki.git");
    git(
        &[
            "clone",
            "-q",
            "--mirror",
            path_text(&wiki),
            path_text(&sibling),
        ],
        &root,
    );
    let lifecycle = MirrorLifecycle::new(
        fixture.database.clone(),
        MirrorLifecycleSettings::for_test(
            root.join("published"),
            root.join("mirror-work"),
            git_binary(),
        ),
    )
    .expect("mirror lifecycle");
    let probe = GitRunner::new(RunConfig {
        git_binary: git_binary(),
        allowed: vec![Subcommand::LsRemote],
        working_directory: root.clone(),
        run_home: root.join("probe-home"),
        deadline: Duration::from_secs(10),
        stdout_cap_bytes: 64 * 1024,
        stderr_cap_bytes: 64 * 1024,
        credential_helper: PathBuf::from("/usr/bin/false"),
    });
    let coordinator = WikiCoordinator::new(fixture.database.clone(), probe, lifecycle.clone());

    let outcome = coordinator
        .discover_and_mirror(
            parent,
            &format!("file://{}", root.join("repository.git").display()),
            4096,
        )
        .await
        .expect("wiki discovery and mirror");
    let WikiMirrorOutcome::Mirrored {
        child_target_id,
        lifecycle: child_run,
    } = outcome
    else {
        panic!("present wiki was not mirrored: {outcome:?}");
    };
    assert!(child_run.is_success());
    assert_ne!(child_target_id, parent);
    assert!(
        lifecycle
            .mirror_path(child_target_id)
            .expect("child path")
            .exists()
    );
    assert!(!lifecycle.mirror_path(parent).expect("parent path").exists());
    let relation: (Uuid, String) = sqlx::query_as(
        "select parent_target_id, status from git_vault.targets where target_id = $1",
    )
    .bind(child_target_id)
    .fetch_one(fixture.pool())
    .await
    .expect("wiki child");
    assert_eq!(relation, (parent, "ready".to_owned()));

    fixture.cleanup().await.expect("cleanup");
    std::fs::remove_dir_all(root).expect("owned scratch cleanup");
}

fn git_binary() -> PathBuf {
    ["/usr/bin/git", "/opt/homebrew/bin/git"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
        .expect("system Git")
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

fn source_fixture(root: &Path) -> PathBuf {
    let source = root.join("wiki-source");
    std::fs::create_dir_all(&source).expect("source root");
    git(&["init", "-q", "--initial-branch=main"], &source);
    std::fs::write(source.join("Home.md"), "preserved wiki\n").expect("wiki page");
    git(&["add", "Home.md"], &source);
    git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@ratatoskr.invalid",
            "commit",
            "-q",
            "-m",
            "wiki",
        ],
        &source,
    );
    source
}

async fn seed_parent(fixture: &TestDatabase) -> Uuid {
    let parent = Uuid::now_v7();
    sqlx::query(
        "insert into git_vault.targets
             (target_id, provider, external_repository_id, status, created_at, updated_at)
         values ($1, 'github', 'wiki-parent', 'ready', now(), now())",
    )
    .bind(parent)
    .execute(fixture.pool())
    .await
    .expect("parent");
    sqlx::query(
        "insert into git_vault.desired_state_revisions
             (revision_id, target_id, policy_revision, preservation_level, include_wiki,
              correlation_id, received_at)
         values ($1, $2, 1, 'complete_archive', true, $3, now())",
    )
    .bind(Uuid::now_v7())
    .bind(parent)
    .bind(Uuid::now_v7())
    .execute(fixture.pool())
    .await
    .expect("parent policy");
    parent
}

fn scratch() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vault-wiki-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 fixture path")
}

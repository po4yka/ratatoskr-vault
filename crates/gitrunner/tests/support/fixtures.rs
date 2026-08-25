//! Generated fixtures for the hostile-repository suite.
//!
//! Fixture *setup* drives plain `git` directly through [`std::process::Command`] — setup is not
//! under test and never goes through the runner. Every operation the suite *asserts about* runs
//! through [`ratatoskr_vault_gitrunner::GitRunner`].

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Locates the system Git binary across the usual macOS/Linux homes.
pub(crate) fn system_git() -> PathBuf {
    for candidate in [
        "/usr/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return path;
        }
    }
    panic!("system git not found in the usual locations");
}

fn git(args: &[&str], cwd: Option<&Path>) {
    git_with_env(args, cwd, &[]);
}

/// Runs one fixture Git command with extra environment entries.
///
/// `env_clear` plus the explicit guards here are safe: this is fixture *setup*, not the runner
/// under test.
fn git_with_env(args: &[&str], cwd: Option<&Path>, extra: &[(&str, &str)]) {
    let mut command = std::process::Command::new(system_git());
    command.args(args).env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    for (key, value) in extra {
        command.env(key, value);
    }
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let output = command.output().expect("fixture git command must spawn");
    assert!(
        output.status.success(),
        "fixture git command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository prepared for exactly one fetch that changes a ref.
pub(crate) struct HookedMirror {
    /// The bare mirror whose configuration points `core.hooksPath` at the hostile hook.
    pub mirror: PathBuf,
    /// The source URL the mirror was cloned from (`file://` form).
    pub source_url: String,
    /// Where the hook writes its marker when it executes.
    pub marker: PathBuf,
}

/// Builds a bare mirror carrying `core.hooksPath` pointing at a `reference-transaction` hook,
/// plus a source repo holding one commit the mirror does not have yet — so a single fetch must
/// update a ref, which is exactly what fires the transaction hook.
pub(crate) fn hooked_mirror(root: &Path) -> HookedMirror {
    let src = root.join("source");
    let mirror = root.join("mirrors").join("t.git");
    let marker = root.join("hook-ran.txt");

    std::fs::create_dir_all(&src).expect("source worktree must be created");
    std::fs::create_dir_all(root.join("mirrors")).expect("mirrors dir must be created");

    git(&["init", "-q", "--initial-branch=main"], Some(&src));
    std::fs::write(src.join("f.txt"), "one\n").expect("seed file must be writable");
    git(&["add", "f.txt"], Some(&src));
    git(
        &[
            "-c",
            "user.name=Fixer",
            "-c",
            "user.email=fix@example.com",
            "commit",
            "-q",
            "-m",
            "one",
        ],
        Some(&src),
    );

    git(
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            mirror.to_str().unwrap(),
        ],
        None,
    );

    // The commit the mirror is missing: fetching it must move `refs/heads/main`.
    std::fs::write(src.join("f.txt"), "two\n").expect("second seed file must be writable");
    git(&["add", "f.txt"], Some(&src));
    git(
        &[
            "-c",
            "user.name=Fixer",
            "-c",
            "user.email=fix@example.com",
            "commit",
            "-q",
            "-m",
            "two",
        ],
        Some(&src),
    );

    // The hostile part: any ref update must leave a marker behind.
    let hooks = root.join("hostile-hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir must be created");
    let hook = hooks.join("reference-transaction");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf 'ran\\n' >> '{}'\nexit 0\n",
            marker.display()
        ),
    )
    .expect("hook script must be writable");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
        .expect("hook script must become executable");

    let mut configured = std::process::Command::new(system_git());
    configured
        .args(["config", "core.hooksPath"])
        .arg(&hooks)
        .current_dir(&mirror)
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    let status = configured.status().expect("config must spawn");
    assert!(status.success(), "setting core.hooksPath must succeed");

    HookedMirror {
        source_url: format!("file://{}", src.display()),
        mirror,
        marker,
    }
}

/// A small history whose object ids are meant to be reproducible across builds.
pub(crate) struct DeterministicHistory {
    /// The worktree holding the history (a normal repository, not bare).
    pub source: PathBuf,
}

/// Environment additions making every fixture commit reproducible.
const DETERMINISM_ENVS: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "Ratatoskr Fixture"),
    ("GIT_AUTHOR_EMAIL", "fixture@ratatoskr.invalid"),
    ("GIT_COMMITTER_NAME", "Ratatoskr Fixture"),
    ("GIT_COMMITTER_EMAIL", "fixture@ratatoskr.invalid"),
    // One fixed instant for every commit; only content differentiates them.
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00+00:00"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00+00:00"),
];

/// Builds a two-commit repository into `root/deterministic-src`.
///
/// Identities and dates are pinned, so two builds produce identical object ids; the committed
/// contents alone differentiate commits.
pub(crate) fn deterministic_history(root: &Path) -> DeterministicHistory {
    let src = root.join("deterministic-src");
    std::fs::create_dir_all(&src).expect("deterministic source must be created");

    git(&["init", "-q", "--initial-branch=main"], Some(&src));
    for (index, content) in ["first\n", "second\n"].iter().enumerate() {
        std::fs::write(src.join("file.txt"), content).expect("seed file must be writable");
        git_with_env(&["add", "file.txt"], Some(&src), &DETERMINISM_ENVS);
        git_with_env(
            &[
                "-c",
                "user.name=Ratatoskr Fixture",
                "-c",
                "user.email=fixture@ratatoskr.invalid",
                "commit",
                "-q",
                "-m",
                &format!("commit {index}"),
            ],
            Some(&src),
            &DETERMINISM_ENVS,
        );
    }

    DeterministicHistory { source: src }
}

/// A repository whose refs carry names crafted to attack shell-based tooling.
pub(crate) struct HostileRefnames {
    /// The worktree holding the hostile refs.
    pub source: PathBuf,
    /// Every ref short-name the fixture created.
    pub names: Vec<String>,
}

/// Builds a repository with branches and tags whose names mix shell metacharacters, Unicode,
/// and a maximum-length segment.
///
/// Git itself refuses two shapes used by real attackers: dash-leading names (option injection,
/// covered by `SourceUrl`) and `$(` command-substitution spellings (rejected by modern Git's own
/// ref syntax). Everything else shell-shaped stays legal in refs and must survive as data.
pub(crate) fn hostile_refnames(root: &Path) -> HostileRefnames {
    let src = root.join("hostile-refnames-src");
    std::fs::create_dir_all(&src).expect("hostile source must be created");

    let long_segment = "x".repeat(180);
    let names: Vec<String> = vec![
        "back`tick`;semi&colon|dollar$pwned-marker".to_owned(),
        "(parens)and${brace}expansion".to_owned(),
        "feature/日本語-ünïcode".to_owned(),
        format!("long/{long_segment}"),
        "plain-main".to_owned(),
    ];

    git(&["init", "-q", "--initial-branch=plain-main"], Some(&src));
    std::fs::write(src.join("f.txt"), "content\n").expect("seed must be writable");
    git(&["add", "f.txt"], Some(&src));
    git(
        &[
            "-c",
            "user.name=Fixer",
            "-c",
            "user.email=fix@example.com",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
        Some(&src),
    );

    // The checked-out branch already exists from init; every other name becomes a branch, and
    // each non-initial name also gets a tag twin.
    for name in &names {
        if name != "plain-main" {
            git(&["branch", name], Some(&src));
        }
        if name != "plain-main" && !name.starts_with("back") {
            git(&["tag", &format!("tag-{name}")], Some(&src));
        }
    }

    HostileRefnames { source: src, names }
}

/// A repository carrying one multi-megabyte blob.
pub(crate) struct LargeBlobRepo {
    /// The worktree holding the large object.
    pub source: PathBuf,
}

/// Builds a repository with an ~8 MiB repeating-pattern blob.
///
/// The pattern compresses well, keeping fixture build and clone fast while still exercising
/// bulk-object plumbing end to end.
pub(crate) fn large_blob_repo(root: &Path) -> LargeBlobRepo {
    let src = root.join("large-blob-src");
    std::fs::create_dir_all(&src).expect("large-blob source must be created");

    git(&["init", "-q", "--initial-branch=main"], Some(&src));

    // 8 MiB of incompressible-but-deterministic data (32-bit LCG), so the bulk-object plumbing
    // moves real bytes while two builds still produce identical content.
    let mut state: u32 = 0x7256_6175;
    let mut blob = Vec::with_capacity(8 * 1024 * 1024);
    while blob.len() < 8 * 1024 * 1024 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        blob.extend_from_slice(&state.to_le_bytes());
    }
    std::fs::write(src.join("big.bin"), &blob).expect("blob must be writable");
    git(&["add", "big.bin"], Some(&src));
    git(
        &[
            "-c",
            "user.name=Fixer",
            "-c",
            "user.email=fix@example.com",
            "commit",
            "-q",
            "-m",
            "large object",
        ],
        Some(&src),
    );

    LargeBlobRepo { source: src }
}

/// A mirror marked as a partial (promisor) clone whose promised objects are genuinely absent.
pub(crate) struct PromisorMirror {
    /// The bare mirror with `remote.origin.promisor=true` and a wiped object store.
    pub mirror: PathBuf,
}

/// Builds the fixture Git refuses to build directly: a mirror that claims promisor backing
/// while its object store is empty.
///
/// Steps: full mirror clone, then promisor configuration, then removal of the pack directory.
/// Modern Git ignores `--filter` on `--mirror` clones and its `fsck` tolerates *present* partial
/// clones, so the honest way to obtain "promisor + actually missing" is to withdraw the objects
/// after the fact — exactly what happens when an upstream silently stops delivering history.
pub(crate) fn promisor_mirror(root: &Path) -> PromisorMirror {
    let src = large_blob_repo(root);
    let mirrors = root.join("mirrors");
    std::fs::create_dir_all(&mirrors).expect("mirrors dir");
    let mirror = mirrors.join("p.git");

    git(
        &[
            "clone",
            "-q",
            "--mirror",
            &format!("file://{}", src.source.display()),
            mirror.to_str().expect("mirror path"),
        ],
        None,
    );
    git(&["config", "remote.origin.promisor", "true"], Some(&mirror));
    git(
        &["config", "extensions.partialclone", "origin"],
        Some(&mirror),
    );

    let packs = mirror.join("objects").join("pack");
    if packs.exists() {
        std::fs::remove_dir_all(&packs).expect("pack dir must be removable");
    }

    PromisorMirror { mirror }
}

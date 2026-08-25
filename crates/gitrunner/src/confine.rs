//! Filesystem confinement for every path handed to Git.
//!
//! A path is only usable when, resolved at use time, it stays inside a Vault-owned root. Checks
//! canonicalize immediately before the value is consumed, so `..` components and intermediate
//! symlinks leaving the root are refused rather than followed.

use std::path::{Path, PathBuf};

use crate::error::GitRunnerError;

/// A filesystem path proven to resolve inside its Vault-owned root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinedPath {
    root: PathBuf,
    resolved: PathBuf,
}

impl ConfinedPath {
    /// Validates that `relative`, joined onto `root`, resolves inside `root` itself.
    ///
    /// # Errors
    ///
    /// Returns [`GitRunnerError::PathOutsideConfinement`] when the joined path escapes the root
    /// through `..` components or intermediate symlinks.
    pub fn new(root: &Path, relative: &Path) -> Result<Self, GitRunnerError> {
        if relative.as_os_str().is_empty() {
            return Err(GitRunnerError::PathOutsideConfinement {
                reason: "relative path is empty".to_owned(),
            });
        }

        let canonical_root =
            root.canonicalize()
                .map_err(|source| GitRunnerError::PathOutsideConfinement {
                    reason: format!("root {} does not resolve: {source}", root.display()),
                })?;

        let normalized = normalize_lexically(relative).ok_or_else(|| {
            GitRunnerError::PathOutsideConfinement {
                reason: format!(
                    "`{}` escapes the root through parent components",
                    relative.display()
                ),
            }
        })?;

        // Walk down from the root over the normalized segments. Every segment that already
        // exists must be a real directory inside the root - never a symlink - so an attacker
        // cannot pivot out through an intermediate hop.
        let mut resolved = canonical_root.clone();
        for segment in &normalized {
            let candidate = resolved.join(segment);
            match std::fs::symlink_metadata(&candidate) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(GitRunnerError::PathOutsideConfinement {
                            reason: format!(
                                "path traverses symlinked component `{}`",
                                candidate.display()
                            ),
                        });
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(GitRunnerError::PathOutsideConfinement {
                        reason: format!("cannot inspect {}: {source}", candidate.display()),
                    });
                }
            }
            resolved = candidate;
        }

        debug_assert!(resolved.starts_with(&canonical_root));
        Ok(Self {
            root: canonical_root,
            resolved,
        })
    }

    /// The root this path was confined against (canonicalized).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The fully resolved path, safe to hand to Git as a destination or operand.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.resolved
    }
}

impl core::fmt::Display for ConfinedPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.resolved.display())
    }
}

/// Collapses `.` and `..` segments lexically.
///
/// Returns `None` when a parent component would climb above the root, which is an escape attempt
/// regardless of what the filesystem says. Absolute components are rejected by construction:
/// callers pass relative paths only, so a leading `/` surfaces as [`std::path::Component::RootDir`]
/// here.
fn normalize_lexically(relative: &Path) -> Option<Vec<std::ffi::OsString>> {
    use std::path::Component;

    let mut segments: Vec<std::ffi::OsString> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => segments.push(part.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                segments.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(segments)
}

/// The identity of an enrolled backup target, as Vault stores it internally.
///
/// `display_name` is provider-supplied hostile input kept for operator presentation only. It is
/// carried on this type so that no downstream code can accidentally reintroduce a
/// name-derived-path API without touching this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetIdentity {
    /// The internal identifier; the only input allowed to shape filesystem layout.
    pub id: uuid::Uuid,
    /// The provider's display name; never used for paths, keys, or command arguments.
    pub display_name: String,
}

impl TargetIdentity {
    /// Builds the identifier-derived mirror location for this target under `mirrors_root`:
    /// `<root>/mirrors/<first-two-hex-of-id>/<id>.git`.
    ///
    /// # Errors
    ///
    /// Returns [`GitRunnerError::PathOutsideConfinement`] when the mirrors root itself does not
    /// resolve inside confinement.
    pub fn mirror_path(&self, mirrors_root: &Path) -> Result<ConfinedPath, GitRunnerError> {
        let hex = self.id.as_simple().to_string();
        // First two hex characters as the shard. Built without slicing: `string_slice` is
        // denied workspace-wide even where the slice provably cannot fail.
        let mut shard = String::with_capacity(2);
        shard.extend(hex.chars().take(2));
        let file = format!("{hex}.git");
        ConfinedPath::new(
            mirrors_root,
            Path::new("mirrors").join(shard).join(file).as_path(),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "assertions in a test binary"
    )]

    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::ConfinedPath;
    use crate::error::GitRunnerError;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vault-confine-{label}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch root must be created");
        dir
    }

    #[test]
    fn destination_outside_root_is_refused() {
        let root = scratch("outside");
        let error = ConfinedPath::new(&root, Path::new("../escape"))
            .expect_err("a `..` escape must be refused");
        assert!(
            matches!(error, GitRunnerError::PathOutsideConfinement { .. }),
            "expected PathOutsideConfinement, got {error:?}"
        );
        assert!(
            !root.parent().unwrap_or(&root).join("escape").exists(),
            "the escaped location must not have been created"
        );
        let _ignored = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinked_intermediate_component_is_refused() {
        let root = scratch("symlink");
        let outside = scratch("symlink-outside");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("intermediate dir must be created");
        symlink(&outside, sub.join("hop")).expect("symlink must be created");

        let error = ConfinedPath::new(&root, Path::new("sub/hop/steal.git"))
            .expect_err("an intermediate symlink leaving the root must be refused");
        assert!(
            matches!(error, GitRunnerError::PathOutsideConfinement { .. }),
            "expected PathOutsideConfinement, got {error:?}"
        );

        let _ignored = std::fs::remove_dir_all(&root);
        let _ignored = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn destination_inside_root_is_accepted_and_resolved() {
        let root = scratch("inside");
        let confined = ConfinedPath::new(&root, Path::new("mirrors/abc.git"))
            .expect("inside-root path must pass");
        // The resolved path is anchored at the *canonical* root (`/var` is a symlink on macOS),
        // so compare against the confined root rather than the raw scratch path.
        assert_eq!(confined.as_path(), confined.root().join("mirrors/abc.git"));
        let _ignored = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn provider_names_never_become_paths() {
        use super::TargetIdentity;

        let root = scratch("mirror-layout");
        let id_a = uuid::Uuid::now_v7();
        let id_b = uuid::Uuid::now_v7();

        for name in ["../../pwned", "/etc/passwd", "na\0me"] {
            let identity = TargetIdentity {
                id: id_a,
                display_name: name.to_owned(),
            };
            let path = identity
                .mirror_path(&root)
                .unwrap_or_else(|error| panic!("id derivation must ignore {name:?}: {error}"));

            assert!(
                path.as_path().starts_with(path.root()),
                "name {name:?} produced a path outside confinement: {}",
                path.as_path().display()
            );
            assert!(
                !path.as_path().to_string_lossy().contains(name),
                "hostile display name leaked into the mirror path"
            );
            assert_eq!(
                path.as_path().file_name(),
                Some(std::ffi::OsStr::new(&format!("{}.git", id_a.simple()))),
                "the mirror file name must be the internal id, not provider data"
            );
        }

        let first = TargetIdentity {
            id: id_a,
            display_name: "../../pwned".to_owned(),
        }
        .mirror_path(&root)
        .expect("first derivation must succeed");
        let second = TargetIdentity {
            id: id_b,
            display_name: "../../pwned".to_owned(),
        }
        .mirror_path(&root)
        .expect("second derivation must succeed");
        assert_ne!(
            first.as_path(),
            second.as_path(),
            "distinct ids must map to distinct mirror locations"
        );

        let _ignored = std::fs::remove_dir_all(&root);
    }
}

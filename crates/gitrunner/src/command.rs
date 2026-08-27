//! The closed vocabulary of Git subcommands Vault may execute.
//!
//! The set is deliberately tiny and grows only when a plan item names a new operation. Because
//! construction is an enum rather than strings, a subcommand outside the allowlist is
//! unrepresentable at the type level; [`Subcommand::ALL`] feeds the runtime allowlist check in
//! [`crate::RunConfig`].

/// A Git subcommand the runner knows how to build arguments for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Subcommand {
    /// `git version` — the capability probe used by startup checks and tests.
    Version,
    /// `git fsck` — object-database integrity verification.
    Fsck,
    /// `git fetch` — update an existing mirror from a validated source.
    Fetch,
    /// `git rev-list` — enumerate commits for ref-set evidence.
    RevList,
    /// `git clone` — create a mirror of a validated source at a confined destination.
    CloneMirror,
    /// `git show-ref` — list every ref in the current repository.
    ShowRef,
    /// `git bundle` — create a portable snapshot bundle.
    Bundle,
}

impl Subcommand {
    /// Every subcommand, so the allowlist can never grow silently. The array length is the
    /// documented count; adding a variant without extending it does not compile.
    pub const ALL: [Self; 7] = [
        Self::Version,
        Self::Fsck,
        Self::Fetch,
        Self::RevList,
        Self::CloneMirror,
        Self::ShowRef,
        Self::Bundle,
    ];

    /// The literal argument word handed to the Git binary.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Fsck => "fsck",
            Self::Fetch => "fetch",
            Self::RevList => "rev-list",
            Self::CloneMirror => "clone",
            Self::ShowRef => "show-ref",
            Self::Bundle => "bundle",
        }
    }
}

/// One fully described Git invocation: subcommand plus typed arguments.
///
/// Arguments are produced only by the builders in this module; there is no public constructor
/// accepting raw strings for the operations Vault runs, so shell-shaped or option-shaped input
/// cannot enter an argument vector by accident.
#[derive(Debug, Clone)]
pub struct GitOperation {
    subcommand: Subcommand,
    arguments: Vec<std::ffi::OsString>,
    credentials: Option<crate::credentials::Credentials>,
}

impl GitOperation {
    /// The subcommand this operation executes (the allowlist gate reads it).
    #[must_use]
    pub const fn subcommand(&self) -> Subcommand {
        self.subcommand
    }

    /// The typed arguments following the subcommand word.
    #[must_use]
    pub fn arguments(&self) -> &[std::ffi::OsString] {
        &self.arguments
    }

    /// Attaches credential material delivered out of band at run time.
    #[must_use]
    pub fn with_credentials(mut self, credentials: crate::Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }
    pub(crate) const fn credentials(&self) -> Option<&crate::credentials::Credentials> {
        self.credentials.as_ref()
    }

    /// `git version`.
    #[must_use]
    pub fn version() -> Self {
        Self {
            subcommand: Subcommand::Version,
            arguments: Vec::new(),
            credentials: None,
        }
    }

    /// `git fsck --full` — full object-database connectivity and validity check.
    #[must_use]
    pub fn fsck_full() -> Self {
        Self {
            subcommand: Subcommand::Fsck,
            arguments: vec![std::ffi::OsString::from("--full")],
            credentials: None,
        }
    }

    /// `git fetch <source> +refs/*:refs/*` — update a mirror with every ref from `source`.
    #[must_use]
    pub fn fetch_all(source: &crate::url::SourceUrl) -> Self {
        Self {
            subcommand: Subcommand::Fetch,
            arguments: vec![
                std::ffi::OsString::from(source.as_str()),
                std::ffi::OsString::from("+refs/*:refs/*"),
            ],
            credentials: None,
        }
    }

    /// `git rev-list --all` — every commit reachable from any ref.
    #[must_use]
    pub fn rev_list_all() -> Self {
        Self {
            subcommand: Subcommand::RevList,
            arguments: vec![std::ffi::OsString::from("--all")],
            credentials: None,
        }
    }

    /// `git rev-list --objects --all` — every object with its path, for completeness evidence.
    #[must_use]
    pub fn rev_list_all_objects() -> Self {
        Self {
            subcommand: Subcommand::RevList,
            arguments: vec![
                std::ffi::OsString::from("--objects"),
                std::ffi::OsString::from("--all"),
            ],
            credentials: None,
        }
    }

    /// `git clone --mirror <source> <destination>` — the initial mirror operation.
    ///
    /// Both inputs arrive pre-validated: `source` through [`crate::SourceUrl`], `destination`
    /// through [`crate::ConfinedPath`].
    #[must_use]
    pub fn clone_mirror(
        source: &crate::url::SourceUrl,
        destination: &crate::confine::ConfinedPath,
    ) -> Self {
        Self {
            subcommand: Subcommand::CloneMirror,
            arguments: vec![
                std::ffi::OsString::from("--mirror"),
                std::ffi::OsString::from(source.as_str()),
                destination.as_path().as_os_str().to_os_string(),
            ],
            credentials: None,
        }
    }

    /// `git show-ref` — every ref with its object id, one `oid refname` pair per line.
    #[must_use]
    pub fn show_ref() -> Self {
        Self {
            subcommand: Subcommand::ShowRef,
            arguments: Vec::new(),
            credentials: None,
        }
    }

    /// `git bundle create <destination> --all` — full portable evidence for every mirror ref.
    #[must_use]
    pub fn bundle_create(destination: &crate::ConfinedPath) -> Self {
        Self {
            subcommand: Subcommand::Bundle,
            arguments: vec![
                std::ffi::OsString::from("create"),
                destination.as_path().as_os_str().to_os_string(),
                std::ffi::OsString::from("--all"),
            ],
            credentials: None,
        }
    }
}

impl core::fmt::Display for Subcommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

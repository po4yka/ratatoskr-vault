//! An authorization-enabled broker built from the shipped identity fragment
//! (`deploy/nats/identity.conf`, the VAULT stanza of XR-021 CONTRACTS.md section S03), with the
//! seeds of its two identities: an administrator that stands in for Edge, and Vault.
//!
//! The tests that prove the identity's permissions and the tests that watch the lane under a
//! refusal share this scaffolding.

use std::path::{Path, PathBuf};
use std::time::Duration;

use uuid::Uuid;

use super::Broker;

/// The subject whose permission the refusal tests withdraw and restore.
const ACKNOWLEDGEMENT_SUBJECT_LINE: &str = "\"evt.vault.backup_policy.acknowledged.v1\"";

/// Where the shipped fragment lives.
#[must_use]
pub fn fragment_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/nats/identity.conf")
}

/// The shipped fragment.
///
/// # Panics
///
/// When the fragment is missing or unreadable: Vault must carry its NATS identity (S03).
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "test-only scaffolding: a missing fragment is a test failure"
)]
pub fn fragment() -> String {
    let path = fragment_path();
    assert!(
        path.is_file(),
        "{} is missing: Vault must carry its NATS identity fragment (S03)",
        path.display()
    );
    std::fs::read_to_string(&path).expect("the fragment is readable")
}

/// Replaces the placeholder with a real public key so the server accepts the identity.
#[must_use]
pub fn with_public_key(fragment: &str, public_key: &str) -> String {
    fragment
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("nkey:") {
                format!("    nkey: {public_key}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fragment without the line that permits the acknowledgement subject.
///
/// # Panics
///
/// When the fragment no longer names the subject on a line of its own.
#[must_use]
pub fn without_acknowledgement_permission(fragment: &str) -> String {
    let kept: Vec<&str> = fragment
        .lines()
        .filter(|line| !line.trim_start().starts_with(ACKNOWLEDGEMENT_SUBJECT_LINE))
        .collect();
    assert!(
        kept.len() < fragment.lines().count(),
        "the fragment does not list the acknowledgement subject on its own line"
    );
    kept.join("\n")
}

/// Connects with an nkey seed.
///
/// # Panics
///
/// When the identity does not connect.
#[expect(
    clippy::expect_used,
    reason = "test-only scaffolding: a refused identity is a test failure"
)]
pub async fn connect(url: &str, seed: &str, request_timeout: Duration) -> async_nats::Client {
    async_nats::ConnectOptions::with_nkey(seed.to_owned())
        .request_timeout(Some(request_timeout))
        .connect(url)
        .await
        .expect("the nkey identity connects")
}

/// A broker that enforces a Vault stanza, with the seeds of the two identities it knows.
#[derive(Debug)]
pub struct Authorized {
    /// The running server.
    pub broker: Broker,
    /// The scratch directory holding its configuration, log and stream store.
    pub directory: PathBuf,
    /// The seed of the identity that stands in for Edge and may do anything.
    pub admin_seed: String,
    /// The seed of the Vault identity.
    pub vault_seed: String,
    admin_public_key: String,
    vault_public_key: String,
}

impl Authorized {
    /// Starts a broker that enforces `stanza` (a fragment text) for Vault, under a fresh scratch
    /// directory inside `scratch`.
    ///
    /// # Panics
    ///
    /// When the scratch directory cannot be made or the server does not start.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test-only scaffolding: a broker that does not start is a test failure"
    )]
    pub fn spawn(scratch: &Path, stanza: &str) -> Self {
        let administrator = nkeys::KeyPair::new_user();
        let vault = nkeys::KeyPair::new_user();
        let directory = scratch.join(format!("authz-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        let admin_public_key = administrator.public_key();
        let vault_public_key = vault.public_key();
        let authorization = authorization(&admin_public_key, &vault_public_key, stanza);
        let broker = Broker::spawn(&directory, &authorization).expect("an authorizing broker");
        Self {
            broker,
            directory,
            admin_seed: administrator.seed().expect("an admin seed"),
            vault_seed: vault.seed().expect("a vault seed"),
            admin_public_key,
            vault_public_key,
        }
    }

    /// The shipped fragment, enforced.
    #[must_use]
    pub fn shipped(scratch: &Path) -> Self {
        Self::spawn(scratch, &fragment())
    }

    /// Replaces the enforced stanza on the running server (a configuration reload), keeping the
    /// same identities. Connected clients keep their connection and get the new permissions.
    ///
    /// # Errors
    ///
    /// A description of why the reload could not be requested.
    pub fn enforce(&mut self, stanza: &str) -> Result<(), String> {
        let authorization = authorization(&self.admin_public_key, &self.vault_public_key, stanza);
        self.broker.reload(&authorization)
    }
}

/// The `authorization` block of a server configuration: the administrator may do anything and
/// the Vault stanza is enforced as shipped, with its placeholder key replaced.
fn authorization(admin_public_key: &str, vault_public_key: &str, stanza: &str) -> String {
    let stanza = with_public_key(stanza, vault_public_key);
    format!(
        "authorization {{\n  users: [\n    {{ nkey: {admin_public_key}\n      permissions: {{\n        \
         publish: {{ allow: [\">\"] }}\n        subscribe: {{ allow: [\">\"] }}\n      }} }},\n\
         {stanza}\n  ]\n}}\n",
    )
}

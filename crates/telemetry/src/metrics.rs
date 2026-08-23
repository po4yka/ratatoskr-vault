//! The instrument names Vault emits, and nothing else.
//!
//! Names live beside the code that increments them but in one module, so a dashboard query and an
//! emit site cannot disagree by drifting apart in two files.

/// The build-identity gauge, set once at startup. Every dashboard's "what is running" anchor.
pub const VAULT_BUILD_INFO: &str = "vault_build_info";

/// The aggregate readiness of this process: `1.0` when `/health/ready` would answer 200, else `0`.
pub const VAULT_READINESS: &str = "vault_readiness";

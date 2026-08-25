//! Construction of the hardened child environment.
//!
//! Children never inherit Vault's process environment. Every invocation gets exactly the map
//! built here: a minimal `PATH`, a confined `HOME`, deterministic locale, and the Git guards
//! that close off system/global configuration, interactive prompting, askpass programs, and
//! pagers. The closed key set lives in [`CHILD_ENV_KEYS`]; the bounds tests print the child's
//! full environment through the probe and refuse anything outside it.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

/// Every environment key a confined child may observe.
pub const CHILD_ENV_KEYS: [&str; 11] = [
    "PATH",
    "HOME",
    "LANG",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PAGER",
    "PAGER",
];

/// Builds the exact environment for one child process.
///
/// `git_dir` contributes the directory holding the trusted Git binary to `PATH` so Git can find
/// its own helpers (credential helpers are invoked by path, never by bare name). `home` becomes
/// `HOME`; it must already exist inside confinement.
#[must_use]
pub fn child_environment(git_dir: &Path, home: &Path) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();

    let path_value = std::env::join_paths([
        git_dir.to_path_buf(),
        Path::new("/usr/bin").to_path_buf(),
        Path::new("/bin").to_path_buf(),
    ])
    .unwrap_or_else(|_| OsString::from("/usr/bin:/bin"));

    // Order mirrors the documentation in lib.rs: identity, config exclusion, prompt closure,
    // determinism.
    env.insert(OsString::from("PATH"), path_value);
    env.insert(OsString::from("HOME"), home.as_os_str().to_os_string());
    env.insert(OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1"));
    env.insert(
        OsString::from("GIT_CONFIG_GLOBAL"),
        OsString::from("/dev/null"),
    );
    env.insert(
        OsString::from("GIT_CONFIG_SYSTEM"),
        OsString::from("/dev/null"),
    );
    env.insert(OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0"));
    env.insert(OsString::from("GIT_ASKPASS"), OsString::from("/dev/null"));
    env.insert(OsString::from("SSH_ASKPASS"), OsString::from("/dev/null"));
    env.insert(OsString::from("GIT_PAGER"), OsString::from("cat"));
    env.insert(OsString::from("PAGER"), OsString::from("cat"));
    env.insert(OsString::from("LANG"), OsString::from("C.UTF-8"));

    debug_assert_eq!(env.len(), CHILD_ENV_KEYS.len());
    env
}

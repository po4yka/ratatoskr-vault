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
pub const CHILD_ENV_KEYS: [&str; 12] = [
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
    "GIT_ALLOW_PROTOCOL",
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
    env.insert(OsString::from("GIT_ALLOW_PROTOCOL"), OsString::from("file"));
    env.insert(OsString::from("PAGER"), OsString::from("cat"));
    env.insert(OsString::from("LANG"), OsString::from("C.UTF-8"));

    debug_assert_eq!(env.len(), CHILD_ENV_KEYS.len());
    env
}

/// Builds the fixed Git LFS child environment without inheriting repository-controlled filters
/// or transfer adapters.
#[must_use]
pub fn lfs_child_environment(
    lfs_dir: &Path,
    home: &Path,
    mirror: &Path,
    storage: &Path,
    standalone_file: bool,
) -> BTreeMap<OsString, OsString> {
    let mut env = child_environment(lfs_dir, home);
    env.insert(OsString::from("GIT_DIR"), mirror.as_os_str().to_os_string());
    env.insert(OsString::from("GIT_LFS_SKIP_SMUDGE"), OsString::from("1"));

    let mut fixed_config = vec![
        ("core.hooksPath", OsString::from("/dev/null")),
        ("lfs.fetchinclude", OsString::new()),
        ("lfs.fetchexclude", OsString::new()),
        ("filter.lfs.process", OsString::new()),
        ("filter.lfs.smudge", OsString::new()),
        ("filter.lfs.clean", OsString::new()),
    ];
    fixed_config.push(if standalone_file {
        (
            "lfs.standalonetransferagent",
            OsString::from("lfs-standalone-file"),
        )
    } else {
        ("lfs.basictransfersonly", OsString::from("true"))
    });
    if !standalone_file {
        fixed_config.push(("lfs.storage", storage.as_os_str().to_os_string()));
    }
    env.insert(
        OsString::from("GIT_CONFIG_COUNT"),
        OsString::from(fixed_config.len().to_string()),
    );
    for (index, (key, value)) in fixed_config.into_iter().enumerate() {
        env.insert(
            OsString::from(format!("GIT_CONFIG_KEY_{index}")),
            OsString::from(key),
        );
        env.insert(OsString::from(format!("GIT_CONFIG_VALUE_{index}")), value);
    }
    env
}

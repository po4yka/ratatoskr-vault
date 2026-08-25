//! Validation of the source URLs Vault clones and fetches from.
//!
//! A source URL is hostile input. Only allowlisted transports may run, and an option-shaped
//! string (leading `-`) must never reach Git's argument parser, where it would be read as flags
//! rather than as a location.

use crate::error::GitRunnerError;

/// The transport schemes a source URL may use.
///
/// `https` covers real sources; `file` exists for local fixture servers in tests and for
/// same-host mirrors. Everything else — `http` plaintext, `ext::`, `ssh::`, `git::`,
/// `git+ssh::`, and friends — is refused before a process starts.
pub const ALLOWED_SCHEMES: [&str; 2] = ["https", "file"];

/// A validated source location for clone and fetch operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUrl(String);

impl SourceUrl {
    /// Validates `raw` as a source URL.
    ///
    /// # Errors
    ///
    /// Returns [`GitRunnerError::InvalidSourceUrl`] when the string begins with `-` (option
    /// injection) or carries a scheme outside [`ALLOWED_SCHEMES`].
    pub fn parse(raw: &str) -> Result<Self, GitRunnerError> {
        if raw.starts_with('-') {
            return Err(GitRunnerError::InvalidSourceUrl {
                reason: "URL begins with `-` and could be parsed as a Git option".to_owned(),
            });
        }

        let Some((scheme, _rest)) = raw.split_once("://") else {
            return Err(GitRunnerError::InvalidSourceUrl {
                reason: "URL carries no `scheme://` transport".to_owned(),
            });
        };

        let scheme = scheme.to_ascii_lowercase();
        if !ALLOWED_SCHEMES.contains(&scheme.as_str()) {
            return Err(GitRunnerError::InvalidSourceUrl {
                reason: format!(
                    "transport `{scheme}` is outside the allowed set {ALLOWED_SCHEMES:?}"
                ),
            });
        }

        Ok(Self(raw.to_owned()))
    }

    /// The validated URL, exactly as it will be passed to Git.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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

    use super::SourceUrl;
    use crate::error::GitRunnerError;

    #[test]
    fn dash_leading_url_is_refused() {
        for hostile in [
            "--upload-pack=touch /tmp/pwned",
            "-c core.hooksPath=/tmp/evil",
        ] {
            let error = SourceUrl::parse(hostile)
                .expect_err("an option-shaped string must never parse as a URL");
            assert!(
                matches!(error, GitRunnerError::InvalidSourceUrl { .. }),
                "expected InvalidSourceUrl for {hostile}, got {error:?}"
            );
        }
    }

    #[test]
    fn exotic_transport_scheme_is_refused() {
        for hostile in [
            "ext::sh -c touch /tmp/pwned",
            "gitserver::example.com/repo",
            "http://insecure.example.com/repo.git",
            "ssh://git@example.com/repo.git",
            "git://example.com/repo.git",
        ] {
            let error = SourceUrl::parse(hostile)
                .expect_err("a transport outside the allowlist must be refused");
            assert!(
                matches!(error, GitRunnerError::InvalidSourceUrl { .. }),
                "expected InvalidSourceUrl for {hostile}, got {error:?}"
            );
        }
    }

    #[test]
    fn allowlisted_schemes_are_accepted() {
        for good in [
            "https://github.com/owner/repository.git",
            "https://127.0.0.1:8443/mirror.git",
            "file:///srv/vault/fixtures/source.git",
        ] {
            let parsed =
                SourceUrl::parse(good).unwrap_or_else(|error| panic!("{good} must parse: {error}"));
            assert_eq!(parsed.as_str(), good);
        }
    }
}

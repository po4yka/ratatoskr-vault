//! Typed wiki discovery result boundary.

/// Result of probing one validated sibling wiki Git source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiProbeResult {
    /// The remote returned at least one syntactically valid ref.
    Present {
        /// Number of valid refs observed in bounded output.
        ref_count: usize,
    },
    /// The sibling was confirmed absent after successful parent access.
    Absent,
    /// The provider rejected the same credential boundary that accessed the parent.
    AuthenticationFailed {
        /// Bounded redacted diagnostic.
        excerpt: String,
    },
    /// The bounded probe exceeded its deadline.
    TimedOut,
    /// The outcome was neither presence nor confirmed absence.
    OtherFailure {
        /// Bounded redacted diagnostic.
        excerpt: String,
    },
}

impl crate::GitRunner {
    /// Probes the sibling wiki without treating an unclassified failure as absence.
    pub async fn probe_wiki(
        &self,
        parent: &crate::SourceUrl,
        parent_access_verified: bool,
    ) -> WikiProbeResult {
        let sibling = match parent.wiki_sibling() {
            Ok(sibling) => sibling,
            Err(error) => {
                return WikiProbeResult::OtherFailure {
                    excerpt: error.to_string(),
                };
            }
        };
        let outcome = match self.run(&crate::GitOperation::ls_remote(&sibling)).await {
            Ok(outcome) => outcome,
            Err(crate::GitRunnerError::Timeout { .. }) => return WikiProbeResult::TimedOut,
            Err(error) => {
                return WikiProbeResult::OtherFailure {
                    excerpt: error.to_string(),
                };
            }
        };
        classify(&sibling, parent_access_verified, &outcome)
    }
}

fn classify(
    sibling: &crate::SourceUrl,
    parent_access_verified: bool,
    outcome: &crate::RunOutcome,
) -> WikiProbeResult {
    if outcome.exit_code == 0 {
        let ref_count = String::from_utf8_lossy(&outcome.stdout)
            .lines()
            .filter(|line| valid_ref_line(line))
            .count();
        return if ref_count > 0 {
            WikiProbeResult::Present { ref_count }
        } else {
            WikiProbeResult::OtherFailure {
                excerpt: "wiki probe returned no valid refs".to_owned(),
            }
        };
    }

    let excerpt = crate::outcome::bounded_excerpt(&outcome.stderr, 512);
    let folded = excerpt.to_ascii_lowercase();
    if folded.contains("authentication failed")
        || folded.contains("could not read username")
        || folded.contains("access denied")
    {
        return WikiProbeResult::AuthenticationFailed { excerpt };
    }
    let confirmed_not_found = sibling.is_file()
        && folded.contains("does not appear to be a git repository")
        || folded.contains("repository not found");
    if parent_access_verified && confirmed_not_found {
        WikiProbeResult::Absent
    } else {
        WikiProbeResult::OtherFailure { excerpt }
    }
}

fn valid_ref_line(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let Some(oid) = words.next() else {
        return false;
    };
    let Some(reference) = words.next() else {
        return false;
    };
    words.next().is_none()
        && matches!(oid.len(), 40 | 64)
        && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        && reference.starts_with("refs/")
        && !reference.contains(['\n', '\r', '\0'])
}

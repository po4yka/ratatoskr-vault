//! Bounded capture of a finished Git process.
//!
//! A run outcome is the only evidence that leaves this crate: exit status plus whatever the
//! per-stream caps retained. Later sections extend it with truncation flags, duration, and
//! redaction; the type exists from the start so callers never see raw process handles.

/// Which standard stream a bound refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl core::fmt::Display for Stream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Stdout => f.write_str("stdout"),
            Self::Stderr => f.write_str("stderr"),
        }
    }
}

/// The placeholder substituted for every occurrence of active secret material.
pub const REDACTED: &[u8] = b"[redacted]";

/// Replaces every occurrence of every `secrets` byte string in `bytes` with [`REDACTED`].
///
/// Applied to both captured streams before a result leaves the runner, so a chatty child cannot
/// echo credential material into logs or manifests.
#[must_use]
pub fn redact_secrets(mut bytes: Vec<u8>, secrets: &[Vec<u8>]) -> Vec<u8> {
    for secret in secrets {
        if secret.is_empty() || bytes.len() < secret.len() {
            continue;
        }
        let mut replaced = Vec::with_capacity(bytes.len());
        let mut rest = bytes.as_slice();
        while let Some(position) = rest
            .windows(secret.len())
            .position(|w| w == secret.as_slice())
        {
            let (head, tail) = rest.split_at(position);
            replaced.extend_from_slice(head);
            replaced.extend_from_slice(REDACTED);
            let (_, remainder) = tail.split_at(secret.len());
            rest = remainder;
        }
        replaced.extend_from_slice(rest);
        bytes = replaced;
    }
    bytes
}

/// The first `max_bytes` of `captured` as UTF-8, lossy and cut at a char boundary.
///
/// Typed failures carry a bounded excerpt so diagnostics survive without shipping megabytes of
/// tool output into an error variant.
#[must_use]
pub fn bounded_excerpt(captured: &[u8], max_bytes: usize) -> String {
    let end = captured.len().min(max_bytes);
    let longest_valid = (0..=end)
        .rev()
        .find(|cut| std::str::from_utf8(captured.get(..*cut).unwrap_or(&[])).is_ok());
    match longest_valid {
        Some(cut) if cut > 0 => {
            String::from_utf8_lossy(captured.get(..cut).unwrap_or(&[])).into_owned()
        }
        _ => String::from_utf8_lossy(captured.get(..end).unwrap_or(&[])).into_owned(),
    }
}

/// What one Git invocation produced.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The child's exit code, when it exited normally (`-1` when killed by a signal).
    pub exit_code: i32,
    /// Captured standard output, as retained under the configured cap.
    pub stdout: Vec<u8>,
    /// Captured standard error, as retained under the configured cap.
    pub stderr: Vec<u8>,
    /// True when standard output hit the cap and bytes were dropped.
    pub stdout_truncated: bool,
    /// True when standard error hit the cap and bytes were dropped.
    pub stderr_truncated: bool,
    /// How long the invocation took from spawn to completion or kill.
    pub duration: std::time::Duration,
}

//! Spawning, bounding, and killing Git children.
//!
//! This module owns every dangerous decision around the process itself: piped standard streams,
//! a held-open stdin so children that read cannot exit spuriously, per-stream caps enforced by
//! concurrent readers (a stream past its cap terminates the run), one wall-clock deadline
//! enforced by [`tokio::time::timeout`], and process-group SIGKILL through the safe `nix`
//! wrappers - raw `libc` calls are impossible here because the workspace forbids `unsafe`.
use std::pin::Pin;

use tokio::io::AsyncRead;

/// Kills the whole process group of `pid`, best effort.
///
/// The child was started with `process_group(0)`, so its group id equals its pid. Killing an
/// already-dead group yields `ESRCH`, which is ignored; the direct child is also signalled so a
/// race between exit and signal still terminates.
pub(crate) fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let raw = i32::try_from(pid).unwrap_or(-1);
        let _ignored = kill(Pid::from_raw(-raw), Signal::SIGKILL);
        let _ignored = kill(Pid::from_raw(raw), Signal::SIGKILL);
    }
}

/// Guards against orphaned descendants if the future itself is dropped mid-flight.
///
/// Armed right after spawn and never disarmed: on every completed path the group is already
/// gone, so the final kill is a harmless no-op; on cancellation it is what stops grandchildren.
pub(crate) struct GroupGuard(pub(crate) Option<u32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        kill_group(self.0);
    }
}

/// Boxes a taken child pipe for a spawned reader task, substituting an empty stream when the
/// pipe was already gone (which cannot happen under `piped()`, but must not panic if it does).
pub(crate) fn boxed_reader<R>(pipe: Option<R>) -> Pin<Box<dyn AsyncRead + Send>>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    match pipe {
        Some(readable) => Box::pin(readable),
        None => Box::pin(tokio::io::empty()),
    }
}

/// Reads `pipe` to completion, retaining at most `cap` bytes plus an overflow flag.
///
/// Reading stops at the first byte past the cap; the pipe is dropped, which closes the read end
/// and typically ends the writer with `EPIPE`.
pub(crate) async fn read_capped<R: AsyncRead + Unpin>(mut pipe: R, cap: usize) -> (Vec<u8>, bool) {
    let mut retained = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match tokio::io::AsyncReadExt::read(&mut pipe, &mut chunk).await {
            Ok(0) | Err(_) => return (retained, false),
            Ok(n) => {
                let take = n.min(cap.saturating_sub(retained.len()));
                let (readable, _) = chunk.split_at(take);
                retained.extend_from_slice(readable);
                if n > take {
                    return (retained, true);
                }
            }
        }
    }
}

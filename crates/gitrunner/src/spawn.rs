//! Spawning, bounding, and killing Git children.
//!
//! This module owns every dangerous decision around the process itself: piped standard streams,
//! a held-open stdin so children that read cannot exit spuriously, per-stream caps enforced by
//! concurrent readers (a stream past its cap terminates the run), one wall-clock deadline
//! enforced by [`tokio::time::timeout`], and process-group SIGKILL through the safe `nix`
//! wrappers - raw `libc` calls are impossible here because the workspace forbids `unsafe`.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;

use tokio::io::AsyncRead;
use tokio::process::Command;

/// Fully constructed process input shared by typed runners.
pub(crate) struct ProcessConfig {
    pub(crate) binary: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) working_directory: PathBuf,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) deadline: std::time::Duration,
    pub(crate) stdout_cap_bytes: usize,
    pub(crate) stderr_cap_bytes: usize,
    pub(crate) secrets: Vec<Vec<u8>>,
}

/// Process-level failures mapped into each typed runner's public error vocabulary.
pub(crate) enum ProcessError {
    Spawn(std::io::Error),
    Timeout(std::time::Duration),
    Interrupted,
    OutputLimit {
        stream: crate::outcome::Stream,
        retained: usize,
    },
}

enum WaitResult {
    Completed(std::io::Result<std::process::ExitStatus>),
    Deadline,
    Interrupted,
}

/// Executes one already typed and validated process with shared bounds and cancellation safety.
pub(crate) async fn run_bounded(config: ProcessConfig) -> Result<crate::RunOutcome, ProcessError> {
    run_bounded_inner(config, None).await
}

/// Executes a bounded process and reaps it before reporting cooperative cancellation.
pub(crate) async fn run_bounded_cancellable(
    config: ProcessConfig,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<crate::RunOutcome, ProcessError> {
    run_bounded_inner(config, Some(cancellation)).await
}

async fn run_bounded_inner(
    config: ProcessConfig,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<crate::RunOutcome, ProcessError> {
    let mut command = Command::new(&config.binary);
    command.args(&config.arguments);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&config.working_directory)
        .env_clear()
        .envs(&config.environment)
        .kill_on_drop(true);

    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(ProcessError::Spawn)?;
    let pid = child.id();
    let started = std::time::Instant::now();
    let _group_guard = GroupGuard(pid);
    let _stdin_hold = child.stdin.take();

    let stdout_reader = tokio::spawn(read_capped(
        boxed_reader(child.stdout.take()),
        config.stdout_cap_bytes,
    ));
    let stderr_reader = tokio::spawn(read_capped(
        boxed_reader(child.stderr.take()),
        config.stderr_cap_bytes,
    ));

    let deadline = tokio::time::sleep(config.deadline);
    tokio::pin!(deadline);
    let waited = loop {
        if cancellation
            .as_deref_mut()
            .is_some_and(|receiver| *receiver.borrow_and_update())
        {
            break WaitResult::Interrupted;
        }
        if let Some(receiver) = cancellation.as_deref_mut() {
            tokio::select! {
                result = child.wait() => break WaitResult::Completed(result),
                () = &mut deadline => break WaitResult::Deadline,
                changed = receiver.changed() => match changed {
                    Ok(()) if *receiver.borrow_and_update() => break WaitResult::Interrupted,
                    Ok(()) => {}
                    Err(_closed) => break WaitResult::Interrupted,
                }
            }
        } else {
            tokio::select! {
                result = child.wait() => break WaitResult::Completed(result),
                () = &mut deadline => break WaitResult::Deadline,
            }
        }
    };
    match waited {
        WaitResult::Deadline => {
            kill_group(pid);
            let _reaped = child.wait().await;
            Err(ProcessError::Timeout(config.deadline))
        }
        WaitResult::Interrupted => {
            kill_group(pid);
            let _reaped = child.wait().await;
            Err(ProcessError::Interrupted)
        }
        WaitResult::Completed(Err(source)) => Err(ProcessError::Spawn(source)),
        WaitResult::Completed(Ok(status)) => {
            let reader_failed =
                || ProcessError::Spawn(std::io::Error::other("stream reader task failed"));
            let (stdout, stdout_overflow) = stdout_reader.await.map_err(|_| reader_failed())?;
            let (stderr, stderr_overflow) = stderr_reader.await.map_err(|_| reader_failed())?;
            if stdout_overflow || stderr_overflow {
                kill_group(pid);
                let _reaped = child.wait().await;
                let (stream, retained) = if stdout_overflow {
                    (crate::outcome::Stream::Stdout, stdout.len())
                } else {
                    (crate::outcome::Stream::Stderr, stderr.len())
                };
                return Err(ProcessError::OutputLimit { stream, retained });
            }
            Ok(crate::RunOutcome {
                exit_code: status.code().unwrap_or(-1),
                stdout: crate::outcome::redact_secrets(stdout, &config.secrets),
                stderr: crate::outcome::redact_secrets(stderr, &config.secrets),
                stdout_truncated: false,
                stderr_truncated: false,
                duration: started.elapsed(),
            })
        }
    }
}

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

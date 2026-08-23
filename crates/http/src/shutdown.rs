//! Signal, drain, close, flush — the part everyone gets wrong, spelled out.

use std::future::{Future, IntoFuture as _};
use std::time::Duration;

use axum::Router;
use ratatoskr_vault_core::config::ShutdownConfig;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::lifecycle::RuntimeState;

/// One listener being served, and the trigger that stops it accepting.
#[derive(Debug)]
pub struct Served {
    /// Resolves the server's graceful-shutdown future.
    close: oneshot::Sender<()>,
    /// Completes when every in-flight request on this listener has finished.
    task: JoinHandle<std::io::Result<()>>,
}

/// Serves `router` on `listener` until [`drain_and_close`] closes it.
///
/// # Panics
///
/// Never itself; the spawned task panics only if axum's accept loop does.
#[must_use]
pub fn serve(listener: TcpListener, router: Router) -> Served {
    let (close, closed) = oneshot::channel();
    let task = tokio::spawn(
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                // A dropped sender means the process is going down anyway.
                let _ = closed.await;
            })
            .into_future(),
    );
    Served { close, task }
}

/// What the shutdown sequence did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ShutdownOutcome {
    /// Whether every in-flight request finished inside the grace window.
    pub graceful: bool,
    /// Whether a second signal short-circuited the sequence.
    pub interrupted: bool,
}

impl ShutdownOutcome {
    /// The value the boot test greps for: a graceful stop is observable, not assumed.
    #[must_use]
    pub const fn graceful(&self) -> bool {
        self.graceful
    }
}

/// The stop sequence. Every step exists because skipping it produces a specific observed failure:
///
/// 1. A signal arrived. Open `vault.shutdown`; log at INFO.
/// 2. [`RuntimeState::begin_draining`]. **Readiness returns 503 immediately. The listeners stay
///    open.**
/// 3. Sleep `drain_seconds`. Existing and brand-new requests still succeed. This is the window in
///    which a supervisor or router removes the endpoint; skipping it is the direct cause of failed
///    probes on every deploy.
/// 4. Every server completes its graceful shutdown: stop accepting, let in-flight requests finish,
///    bounded by `grace_seconds`.
/// 5. If the grace window expires, log WARN and continue anyway. A stop is never blocked by one
///    stuck request.
/// 6. Return; the caller aborts tasks, closes pools and flushes telemetry.
///
/// `interrupt` is the second signal: when it resolves first, the sequence skips straight to step
/// 6. `/health/live` answers 200 throughout.
pub async fn drain_and_close(
    state: &RuntimeState,
    config: &ShutdownConfig,
    servers: Vec<Served>,
    interrupt: impl Future<Output = ()> + Send,
) -> ShutdownOutcome {
    let span = tracing::info_span!(
        "vault.shutdown",
        drain_seconds = config.drain_seconds,
        graceful = Empty,
        interrupted = Empty,
    );

    async move {
        tracing::info!("a shutdown signal arrived; draining");
        state.begin_draining();

        tokio::pin!(interrupt);
        let mut interrupted = tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(config.drain_seconds)) => false,
            () = &mut interrupt => true,
        };

        let mut tasks = Vec::with_capacity(servers.len());
        for served in servers {
            // A closed receiver means that server already stopped.
            let _ = served.close.send(());
            tasks.push(served.task);
        }

        let graceful = if interrupted {
            false
        } else {
            // The grace window is part of the sequence, so a second signal short-circuits it too.
            // Without the second arm a second Ctrl-C during step 4 is ignored and the operator
            // waits out the whole `grace_seconds`.
            tokio::select! {
                closed = tokio::time::timeout(
                    Duration::from_secs(config.grace_seconds),
                    wait_for_all(&mut tasks),
                ) => closed.is_ok(),
                () = &mut interrupt => {
                    interrupted = true;
                    false
                }
            }
        };
        if !graceful && interrupted {
            for task in &tasks {
                task.abort();
            }
            tracing::warn!("a second signal arrived; closing without waiting for in-flight work");
        } else if !graceful {
            tracing::warn!(
                "the grace window expired with requests still in flight; exiting anyway"
            );
        }

        let outcome = ShutdownOutcome {
            graceful,
            interrupted,
        };
        tracing::info!(graceful, interrupted, "shutdown complete");
        outcome
    }
    .instrument(span)
    .await
}

/// Completes when every server task has finished. Borrows rather than consumes, so the tasks are
/// still there to abort if a second signal cancels this future.
async fn wait_for_all(tasks: &mut [JoinHandle<std::io::Result<()>>]) {
    for task in tasks {
        // A server task that failed has already logged; shutdown continues.
        let _ = task.await;
    }
}

/// Resolves on the first SIGTERM or SIGINT.
///
/// A process that cannot register a handler waits forever rather than exiting: an unkillable pod
/// is visible to an operator, a pod that exits at startup for an unrelated reason is not.
pub(crate) async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {},
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            tracing::error!(%error, "the interrupt handler could not be installed");
                        }
                    },
                }
            }
            Err(error) => {
                tracing::error!(%error, "the termination handler could not be installed");
                std::future::pending::<()>().await;
            }
        }
    }

    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "the interrupt handler could not be installed");
        std::future::pending::<()>().await;
    }
}

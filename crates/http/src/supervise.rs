//! Supervision of the background tasks a deployable runs beside its listeners.
//!
//! A task that returns before a shutdown signal has stopped doing its work. Leaving the process
//! ready beside it is the failure XR-021 CONTRACTS.md section S02 rule 5 forbids, so the process
//! learns about it here and stops.

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;

use tokio::task::JoinHandle;

/// Why the process stopped waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A shutdown signal arrived.
    Signal,
    /// A supervised task returned, or panicked, before any signal: its work has stopped.
    TaskExited,
}

/// Waits for a shutdown signal or for any supervised task to finish, whichever comes first.
///
/// The tasks are borrowed, not consumed, so the caller can still abort the survivors. A signal
/// that is already pending wins over a task that has also finished, which is the orderly case.
pub async fn wait_for_stop(
    tasks: &mut [JoinHandle<()>],
    signal: impl Future<Output = ()>,
) -> StopReason {
    let mut signal = pin!(signal);
    poll_fn(|context| {
        if signal.as_mut().poll(context).is_ready() {
            return Poll::Ready(StopReason::Signal);
        }
        for task in tasks.iter_mut() {
            if Pin::new(task).poll(context).is_ready() {
                return Poll::Ready(StopReason::TaskExited);
            }
        }
        Poll::Pending
    })
    .await
}

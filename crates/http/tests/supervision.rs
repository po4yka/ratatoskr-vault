//! Task supervision: a background task that returns before a shutdown signal has stopped doing
//! its work, and the process must notice instead of staying ready (XR-021 CONTRACTS.md S02
//! rule 5).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "assertions in a test binary"
)]

use std::future::pending;
use std::time::Duration;

use ratatoskr_vault_http::{StopReason, wait_for_stop};

/// How long a wait may take before the test calls it hung.
const PATIENCE: Duration = Duration::from_secs(5);

/// The first task to return ends the wait even though no signal ever arrives, and the tasks that
/// are still running stay owned by the caller.
#[tokio::test]
async fn the_first_task_to_return_ends_the_wait_before_a_signal() {
    let mut tasks = vec![
        tokio::spawn(async { pending::<()>().await }),
        tokio::spawn(async {}),
    ];

    let reason = tokio::time::timeout(PATIENCE, wait_for_stop(&mut tasks, pending())).await;

    assert_eq!(reason, Ok(StopReason::TaskExited));
    assert!(
        !tasks[0].is_finished(),
        "the survivor is still the caller's to abort"
    );
    tasks[0].abort();
}

/// A task that panicked has stopped doing its work just as surely as one that returned.
#[tokio::test]
async fn a_task_that_panicked_ends_the_wait_too() {
    #[allow(clippy::panic, reason = "the test needs a task that panics")]
    let mut tasks = vec![tokio::spawn(async { panic!("the worker died") })];

    let reason = tokio::time::timeout(PATIENCE, wait_for_stop(&mut tasks, pending())).await;

    assert_eq!(reason, Ok(StopReason::TaskExited));
}

/// With every task running, the signal ends the wait; with no tasks at all, only the signal can.
#[tokio::test]
async fn a_signal_ends_the_wait_when_no_task_has_returned() {
    let mut running = vec![tokio::spawn(async { pending::<()>().await })];
    let reason = tokio::time::timeout(PATIENCE, wait_for_stop(&mut running, async {})).await;
    assert_eq!(reason, Ok(StopReason::Signal));
    running[0].abort();

    let reason = tokio::time::timeout(PATIENCE, wait_for_stop(&mut [], async {})).await;
    assert_eq!(reason, Ok(StopReason::Signal));
}

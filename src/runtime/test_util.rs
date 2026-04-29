//! Test utilities for controlling race conditions in tests

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

/// Atomic flag to enable checkpoint mechanism
static CHECKPOINT_ENABLED: AtomicBool = AtomicBool::new(false);

/// Atomic flag to track whether emit has reached the checkpoint
static EMIT_AT_CHECKPOINT: AtomicBool = AtomicBool::new(false);

/// Counter to track which test iteration we're on
static CHECKPOINT_GENERATION: AtomicUsize = AtomicUsize::new(0);

/// Notify to signal that emit thread has reached the checkpoint
static REACHED_CHECKPOINT: Notify = Notify::const_new();

/// Notify to signal that the test has completed its work and emit can continue
static ALLOW_CONTINUE: Notify = Notify::const_new();

/// Enable the checkpoint mechanism for testing
pub fn enable_checkpoint() {
    CHECKPOINT_ENABLED.store(true, Ordering::SeqCst);
    CHECKPOINT_GENERATION.fetch_add(1, Ordering::SeqCst);
    EMIT_AT_CHECKPOINT.store(false, Ordering::SeqCst);
}

/// Disable the checkpoint mechanism
pub fn disable_checkpoint() {
    CHECKPOINT_ENABLED.store(false, Ordering::SeqCst);
}

/// Wait at the checkpoint in production code (only enabled in tests)
///
/// This is called after `emit_system_tick_from_wake_hint` completes but before
/// we reacquire the lock to compare and clear the pending hint.
pub async fn wait_at_checkpoint() {
    // Only wait if checkpoint is enabled
    if !CHECKPOINT_ENABLED.load(Ordering::SeqCst) {
        return;
    }

    // Record which generation we're in
    let my_gen = CHECKPOINT_GENERATION.load(Ordering::SeqCst);

    // Signal that we've reached the checkpoint
    EMIT_AT_CHECKPOINT.store(true, Ordering::SeqCst);
    REACHED_CHECKPOINT.notify_one();

    // Wait for the test to signal it's ready for us to continue
    ALLOW_CONTINUE.notified().await;

    // Reset for next test if we're still in the same generation
    if CHECKPOINT_GENERATION.load(Ordering::SeqCst) == my_gen {
        EMIT_AT_CHECKPOINT.store(false, Ordering::SeqCst);
    }
}

/// Wait for the emit thread to reach the checkpoint
///
/// This should be called by the test harness after spawning the emit task
/// to block until the critical window is reached.
pub async fn wait_for_emit_at_checkpoint() {
    // Wait until emit thread signals it's at the checkpoint
    loop {
        if EMIT_AT_CHECKPOINT.load(Ordering::SeqCst) {
            break;
        }
        // Yield to avoid busy-waiting
        tokio::task::yield_now().await;
    }
}

/// Release the checkpoint, allowing the emit thread to continue
pub fn release_checkpoint() {
    // Notify the emit thread to continue
    ALLOW_CONTINUE.notify_one();
}

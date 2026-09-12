// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Threading utilities for the app shell: a latest-wins worker queue and a
//! timeout-bounded closure runner.
//!
//! Both exist for the RDP freeze hardening (diagnosis:
//! `.coding/reviews/2026-08-18-rdp-freeze-diagnosis.md`). WebView2 controller
//! calls (`set_size` & friends) can block indefinitely on a stalled
//! compositor when an RDP session is switched (upstream WebView2Feedback
//! #3581), so they run on a dedicated worker thread — never the UI event
//! loop — and shutdown must not wait indefinitely on such a call.

use std::sync::{Arc, Condvar, Mutex};

/// Shared one-slot mailbox between producers ([`LatestWinsWorker::submit`])
/// and the consumer thread.
struct Slot<T> {
    /// The pending value; at most one is held — a new submission overwrites
    /// an undrained one (latest-wins coalescing).
    pending: Mutex<Option<T>>,
    /// Signalled on every submission so the consumer wakes up.
    condvar: Condvar,
}

/// The producer handle for a latest-wins worker spawned by
/// [`spawn_latest_wins`]. Submissions are O(1) and never block on the
/// consumer.
pub struct LatestWinsWorker<T> {
    slot: Arc<Slot<T>>,
}

impl<T> LatestWinsWorker<T> {
    /// Submit `v` for processing. If a previous value is still pending (the
    /// consumer is busy or wedged), it is replaced — only the newest value
    /// is ever processed. Never blocks on the consumer.
    pub fn submit(&self, v: T) {
        *self.slot.pending.lock().expect("latest-wins slot poisoned") = Some(v);
        self.slot.condvar.notify_one();
    }
}

/// Spawn a dedicated thread that drains a one-slot queue, calling `sink`
/// with each value. Intermediate values submitted while `sink` is running
/// (or wedged) are coalesced: only the latest is kept. A panicking `sink` is
/// caught and logged — the worker keeps processing later values instead of
/// silently dying (review F4, 2026-08-18). The thread is detached — a wedged
/// `sink` parks it forever without blocking producers or process exit.
pub fn spawn_latest_wins<T, F>(mut sink: F) -> LatestWinsWorker<T>
where
    T: Send + 'static,
    F: FnMut(T) + Send + 'static,
{
    let slot = Arc::new(Slot {
        pending: Mutex::new(None),
        condvar: Condvar::new(),
    });
    let worker_slot = Arc::clone(&slot);
    std::thread::spawn(move || loop {
        // Take the pending value, then drop the guard BEFORE calling sink:
        // the lock is never held across user code, so a panicking or wedged
        // sink can neither poison the slot nor block producers.
        let v = {
            let mut pending = worker_slot
                .pending
                .lock()
                .expect("latest-wins slot poisoned");
            loop {
                if let Some(v) = pending.take() {
                    break v;
                }
                pending = worker_slot
                    .condvar
                    .wait(pending)
                    .expect("latest-wins slot poisoned");
            }
        };
        // catch_unwind so a sink bug degrades one value, not the whole
        // worker: without it the thread dies and every later submission is
        // silently dropped for the rest of the session.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(v))).is_err() {
            eprintln!("mnemo: latest-wins worker sink panicked — continuing");
        }
    });
    LatestWinsWorker { slot }
}

/// Run `f` on a detached thread, returning `true` if it completed within
/// `timeout`. On timeout the closure keeps running in the background (there
/// is no safe way to kill a wedged thread) and `false` is returned — the
/// caller is expected to proceed anyway (e.g. let process exit reap the
/// resources). Used to bound shutdown teardown that may call into a wedged
/// WebView2 compositor.
pub fn run_with_timeout<F>(timeout: std::time::Duration, f: F) -> bool
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(timeout).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Values are delivered in submission order when the consumer keeps up.
    #[test]
    fn delivers_values_in_order() {
        let (tx, rx) = mpsc::channel();
        let worker = spawn_latest_wins(move |v: u32| tx.send(v).unwrap());
        worker.submit(1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        worker.submit(2);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        worker.submit(3);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
    }

    /// While the sink is busy, pending submissions coalesce to the latest.
    /// Regression for the RDP freeze (R1): a wedged sink must never let
    /// stale values queue up — only the newest is processed after it.
    #[test]
    fn coalesces_to_latest_while_sink_is_busy() {
        let (seen_tx, seen_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker = spawn_latest_wins(move |v: u32| {
            seen_tx.send(v).unwrap();
            if v == 1 {
                // Signal the sink is busy, then stay busy until released.
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
        });
        worker.submit(1);
        // Wait until the sink has taken 1 and is blocked inside it.
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // These two land while the sink is busy: 2 must be overwritten by 3.
        worker.submit(2);
        worker.submit(3);
        release_tx.send(()).unwrap();
        assert_eq!(seen_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(seen_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 3);
    }

    /// A fast closure completes within the timeout and its side effect runs.
    #[test]
    fn run_with_timeout_completes() {
        let (tx, rx) = mpsc::channel();
        assert!(run_with_timeout(Duration::from_secs(2), move || {
            tx.send(42).unwrap();
        }));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 42);
    }

    /// A closure sleeping past the timeout reports `false` and the caller is
    /// not blocked for the closure's full duration.
    #[test]
    fn run_with_timeout_times_out() {
        assert!(!run_with_timeout(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
        }));
    }

    /// A panicking sink must NOT kill the worker (review F4, 2026-08-18):
    /// the panic is caught and later values are still processed — otherwise
    /// resize handling would silently stop for the rest of the session.
    #[test]
    fn worker_survives_sink_panic() {
        let (tx, rx) = mpsc::channel();
        let worker = spawn_latest_wins(move |v: u32| {
            if v == 1 {
                panic!("sink boom");
            }
            tx.send(v).unwrap();
        });
        worker.submit(1);
        worker.submit(2);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            2,
            "worker must survive a sink panic and process later values"
        );
    }
}

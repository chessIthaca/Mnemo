//! The project's startup indexing passes, as one status the UI can render.
//!
//! The engine runs Mnemo1's two startup passes on open: the code graph index
//! and the derived-memory reconcile. Both report on the engine's host-event
//! bus; this module folds them into an [`IndexingSnapshot`] and rebroadcasts
//! it. Every open shows its code pass (the engine's own stream stays silent
//! for a pass under a second, so this view starts the pass itself). A project
//! with no code index yet is a *first run*: the UI blocks until that first
//! pass ends. Every later open stays interactive and shows the passes as a
//! quiet progress indicator.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mnemo::headless::{Engine, HostEvent, RecvError};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// The Tauri event carrying each new [`IndexingSnapshot`].
pub const INDEXING_CHANNEL: &str = "mnemo2://indexing";

/// How often the first-run watchdog asks the graph whether it is still indexing.
const WATCHDOG_EVERY: Duration = Duration::from_millis(750);
/// Consecutive idle polls before the watchdog calls a silent pass finished.
const WATCHDOG_IDLE_POLLS: u32 = 3;

/// Where one pass stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassState {
    Running,
    Done,
    Failed,
}

/// One indexing pass: the code graph or the memory reconcile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexPass {
    pub state: PassState,
    /// Units processed so far (files for code, sources for memory).
    pub done: usize,
    /// Units the pass will process; 0 while it is still counting.
    pub total: usize,
    /// The summary on success, the error on failure.
    pub detail: Option<String>,
}

impl IndexPass {
    fn running() -> Self {
        Self {
            state: PassState::Running,
            done: 0,
            total: 0,
            detail: None,
        }
    }
}

/// Both startup passes, as the UI renders them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexingSnapshot {
    /// Bumped on every change, so a late snapshot never replaces a newer one.
    pub revision: u64,
    /// The project had no code index when it opened: the UI blocks until the
    /// first pass ends.
    pub first_run: bool,
    pub code: Option<IndexPass>,
    pub memory: Option<IndexPass>,
}

/// Which pass a host event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    Code,
    Memory,
}

/// Fold one pass event (the `"type"`-tagged wire shape both Mnemo1 streams
/// share: `started`, `progress {done,total}`, `done {summary}`,
/// `failed {error}`) into the pass. Unknown shapes leave it unchanged.
pub fn apply_pass_event(pass: Option<IndexPass>, event: &Value) -> Option<IndexPass> {
    let count = |key: &str| event.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
    let text = |key: &str| event.get(key).and_then(Value::as_str).map(str::to_string);
    match event.get("type").and_then(Value::as_str) {
        Some("started") => Some(IndexPass::running()),
        Some("progress") => Some(IndexPass {
            state: PassState::Running,
            done: count("done"),
            total: count("total"),
            detail: None,
        }),
        // A lone `done` closes a pass this view never saw start: the code
        // pass always ends with one, even when a fast pass sent no ticks.
        Some("done") => pass.map(|pass| IndexPass {
            state: PassState::Done,
            done: pass.total.max(pass.done),
            detail: text("summary"),
            ..pass
        }),
        Some("failed") => Some(IndexPass {
            state: PassState::Failed,
            detail: text("error"),
            ..pass.unwrap_or_else(IndexPass::running)
        }),
        _ => pass,
    }
}

/// The live indexing status of one open project.
pub struct IndexingTracker {
    snapshot: Mutex<IndexingSnapshot>,
    events: broadcast::Sender<IndexingSnapshot>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl IndexingTracker {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        Arc::new(Self {
            snapshot: Mutex::new(IndexingSnapshot::default()),
            events,
            tasks: Mutex::new(Vec::new()),
        })
    }

    pub fn snapshot(&self) -> IndexingSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<IndexingSnapshot> {
        self.events.subscribe()
    }

    fn update(&self, change: impl FnOnce(&mut IndexingSnapshot)) {
        let next = {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            let before = snapshot.clone();
            change(&mut snapshot);
            if *snapshot == before {
                return;
            }
            snapshot.revision += 1;
            snapshot.clone()
        };
        let _ = self.events.send(next);
    }

    /// Fold one pass event into the status.
    pub fn apply(&self, kind: PassKind, event: &Value) {
        self.update(|snapshot| {
            let pass = match kind {
                PassKind::Code => &mut snapshot.code,
                PassKind::Memory => &mut snapshot.memory,
            };
            *pass = apply_pass_event(pass.take(), event);
        });
    }

    /// The engine has started its code pass: show it before its first event.
    /// `first_run` holds the project until the pass ends.
    pub fn begin_code_pass(&self, first_run: bool) {
        self.update(|snapshot| {
            snapshot.first_run = first_run;
            snapshot.code = Some(IndexPass::running());
        });
    }

    fn code_running(&self) -> bool {
        matches!(
            self.snapshot().code,
            Some(IndexPass {
                state: PassState::Running,
                ..
            })
        )
    }

    /// Follow the engine's startup passes until the engine stops.
    pub fn watch(self: &Arc<Self>, engine: Arc<Engine>) {
        let mut host_events = engine.host_events();
        let tracker = self.clone();
        let forward = tokio::spawn(async move {
            loop {
                let (kind, event) = match host_events.recv().await {
                    Ok(HostEvent::IndexProgress(event)) => {
                        (PassKind::Code, serde_json::to_value(&event))
                    }
                    Ok(HostEvent::Reconcile(event)) => {
                        (PassKind::Memory, serde_json::to_value(&event))
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                if let Ok(event) = event {
                    tracker.apply(kind, &event);
                }
            }
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.push(forward);
        if self.code_running() {
            tasks.push(self.spawn_watchdog(engine));
        }
    }

    /// A pass that ends without an event (a fast pass whose terminal event
    /// raced the subscription, or a silent failure) must not stay "running":
    /// once the graph stops indexing, close the pass from its own counts. A
    /// graph that never opened has no pass to show.
    fn spawn_watchdog(self: &Arc<Self>, engine: Arc<Engine>) -> JoinHandle<()> {
        let tracker = self.clone();
        tokio::spawn(async move {
            let mut idle = 0;
            while tracker.code_running() {
                tokio::time::sleep(WATCHDOG_EVERY).await;
                let status = match engine.codegraph().status().await {
                    Ok(status) => status,
                    Err(_) => break,
                };
                if !status.available {
                    tracker.update(|snapshot| {
                        snapshot.code = None;
                        snapshot.first_run = false;
                    });
                    break;
                }
                idle = if status.indexing { 0 } else { idle + 1 };
                if idle >= WATCHDOG_IDLE_POLLS {
                    let summary = format!(
                        "{} files indexed · {} symbols · {} edges",
                        status.files, status.symbols, status.edges
                    );
                    tracker.apply(
                        PassKind::Code,
                        &serde_json::json!({"type": "done", "summary": summary}),
                    );
                    break;
                }
            }
        })
    }

    /// Stop following the engine.
    pub fn stop(&self) {
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folds_a_pass_from_start_to_done() {
        let pass = apply_pass_event(None, &json!({"type": "started"}));
        assert_eq!(pass, Some(IndexPass::running()));
        let pass = apply_pass_event(pass, &json!({"type": "progress", "done": 3, "total": 9}));
        assert_eq!(pass.as_ref().map(|p| (p.done, p.total)), Some((3, 9)));
        let pass = apply_pass_event(pass, &json!({"type": "done", "summary": "9 files indexed"}))
            .expect("pass");
        assert_eq!(pass.state, PassState::Done);
        assert_eq!(pass.done, 9);
        assert_eq!(pass.detail.as_deref(), Some("9 files indexed"));
    }

    #[test]
    fn a_lone_done_stays_invisible_and_a_failure_shows() {
        assert_eq!(
            apply_pass_event(None, &json!({"type": "done", "summary": "x"})),
            None
        );
        let failed =
            apply_pass_event(None, &json!({"type": "failed", "error": "boom"})).expect("pass");
        assert_eq!(failed.state, PassState::Failed);
        assert_eq!(failed.detail.as_deref(), Some("boom"));
    }

    #[test]
    fn a_first_run_blocks_until_its_pass_ends() {
        let tracker = IndexingTracker::new();
        let mut events = tracker.subscribe();
        tracker.begin_code_pass(true);
        assert!(tracker.snapshot().first_run);
        assert!(tracker.code_running());
        tracker.apply(PassKind::Code, &json!({"type": "done", "summary": "ok"}));
        assert!(!tracker.code_running());
        assert_eq!(
            events.try_recv().expect("first run").code.map(|p| p.state),
            Some(PassState::Running)
        );
        assert_eq!(
            events.try_recv().expect("done").code.map(|p| p.state),
            Some(PassState::Done)
        );
    }

    #[test]
    fn unchanged_snapshots_are_not_rebroadcast() {
        let tracker = IndexingTracker::new();
        let mut events = tracker.subscribe();
        tracker.apply(
            PassKind::Memory,
            &json!({"type": "done", "summary": "in sync"}),
        );
        assert!(events.try_recv().is_err());
    }
}

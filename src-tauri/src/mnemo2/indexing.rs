//! The project's startup indexing passes, as one status the UI can render.
//!
//! The engine runs the kernel's two startup passes on open: the code graph
//! index and the derived-memory reconcile. Both report on the engine's host-event
//! bus; this module folds them into an [`IndexingSnapshot`] and rebroadcasts
//! it. Every open shows its code pass (the engine's own stream stays silent
//! for a pass under a second, so this view starts the pass itself). A project
//! with no code index yet is a *first run*: the UI blocks until that first
//! pass ends. Every later open stays interactive and shows the passes as a
//! quiet progress indicator.
//!
//! The automatic Laya routing check rides the same status as a third pass
//! (`laya`): it is not an engine pass, but it wants the same progress line.
//! It never touches `first_run` or the code pass.

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
/// How often the memory check watches the reconcile for a quiet window.
const HEALTH_POLL_EVERY: Duration = Duration::from_millis(250);
/// Consecutive quiet polls before the memory check treats the reconcile as
/// settled (1.5 s). A reconcile over an unchanged corpus stays silent — the
/// engine's `return` before `Started` — so "after the reconcile" is a quiet
/// window, not an event.
const HEALTH_QUIET_POLLS: u32 = 6;
/// The Memory status line's prefix for the memory check's counts.
const HEALTH_PREFIX: &str = "health: ";

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
    /// The Laya routing check, started by the app on open (not the engine).
    pub laya: Option<IndexPass>,
}

/// Which pass a host event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassKind {
    Code,
    Memory,
    Laya,
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
                PassKind::Laya => &mut snapshot.laya,
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

    /// Claim the Laya check's pass: `true` when the caller now owns the check,
    /// `false` when one is already running. The check and the claim share one
    /// lock, so two callers can never both start a check.
    pub fn begin_laya(&self) -> bool {
        let next = {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            let running = matches!(
                snapshot.laya,
                Some(IndexPass {
                    state: PassState::Running,
                    ..
                })
            );
            if running {
                return false;
            }
            // A pass that was not running always differs from the fresh one,
            // so the change is guaranteed and only this revision is bumped.
            snapshot.laya = Some(IndexPass::running());
            snapshot.revision += 1;
            snapshot.clone()
        };
        let _ = self.events.send(next);
        true
    }

    /// Forget the Laya check's pass (a check that left nothing to show).
    pub fn clear_laya(&self) {
        self.update(|snapshot| snapshot.laya = None);
    }

    /// Track a task so [`stop`](Self::stop) aborts it with the rest.
    pub fn add_task(&self, task: JoinHandle<()>) {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(task);
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

    fn memory_running(&self) -> bool {
        matches!(
            self.snapshot().memory,
            Some(IndexPass {
                state: PassState::Running,
                ..
            })
        )
    }

    /// Run the memory check pass once the reconcile has settled and fold its
    /// counts into the Memory status line (plan 373dec00 step 5).
    ///
    /// The engine spawns the reconcile detached and an in-sync corpus emits no
    /// event at all, so "after the reconcile" is a quiet window, not an event.
    /// Never blocks the open: the scan runs on the blocking pool, and a failure
    /// is reported in the pass's detail (plus one warning), never as a failed
    /// open.
    fn spawn_memory_health(self: &Arc<Self>, engine: Arc<Engine>) -> JoinHandle<()> {
        let tracker = self.clone();
        tokio::spawn(async move {
            let mut quiet = 0;
            while quiet < HEALTH_QUIET_POLLS {
                quiet = if tracker.memory_running() {
                    0
                } else {
                    quiet + 1
                };
                if quiet < HEALTH_QUIET_POLLS {
                    tokio::time::sleep(HEALTH_POLL_EVERY).await;
                }
            }
            let Some(store) = engine.memory_store_handle() else {
                return;
            };
            let root = engine.workspace_root().to_path_buf();
            // The UI only shows a pass it saw running, so the check enters as
            // Running and leaves as Done: the counts ride the finished line.
            tracker.update(health_running);
            let counts = match mnemo::memory::health::run(store.as_ref(), &root).await {
                Ok(report) => report.summary(),
                Err(e) => {
                    eprintln!("warning: memory health pass failed: {e}");
                    format!("failed — {e}")
                }
            };
            let detail = format!("{HEALTH_PREFIX}{counts}");
            tracker.update(|snapshot| health_done(snapshot, detail));
        })
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
            tasks.push(self.spawn_watchdog(engine.clone()));
        }
        tasks.push(self.spawn_memory_health(engine));
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

/// Flip the Memory status line to Running while the memory check runs.
///
/// The UI only shows a pass it saw running (`IndexingStatus.svelte`), so a
/// check that only ever wrote `Done` would be invisible on an in-sync open.
/// An existing pass keeps its own detail; [`health_done`] appends to it. A
/// FAILED reconcile is left untouched: the window shows its detail as the
/// error's title, and an advisory check must never hide it.
fn health_running(snapshot: &mut IndexingSnapshot) {
    match snapshot.memory.as_mut() {
        Some(pass) if pass.state == PassState::Failed => {}
        Some(pass) => {
            pass.state = PassState::Running;
            pass.done = 0;
            pass.total = 0;
        }
        None => {
            snapshot.memory = Some(IndexPass {
                state: PassState::Running,
                done: 0,
                total: 0,
                detail: None,
            });
        }
    }
}

/// Close the Memory status line with the check's counts joined onto the pass's
/// own detail (` · `), so the finished item's title carries them. A failed
/// CHECK still ends `Done` with `health: failed — …` (advisory, never "Memory
/// update failed"); a failed RECONCILE keeps its `Failed` state and its own
/// detail, which the window renders as the error's title.
fn health_done(snapshot: &mut IndexingSnapshot, detail: String) {
    match snapshot.memory.as_mut() {
        Some(pass) if pass.state == PassState::Failed => {}
        Some(pass) => {
            pass.state = PassState::Done;
            pass.detail = Some(match pass.detail.take() {
                Some(existing) if !existing.is_empty() => format!("{existing} · {detail}"),
                _ => detail,
            });
        }
        None => {
            snapshot.memory = Some(IndexPass {
                state: PassState::Done,
                done: 0,
                total: 0,
                detail: Some(detail),
            });
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

    #[test]
    fn begin_laya_is_single_flight() {
        let tracker = IndexingTracker::new();
        assert!(tracker.begin_laya());
        assert!(!tracker.begin_laya(), "a running check is not started twice");
        assert_eq!(
            tracker.snapshot().laya.map(|pass| pass.state),
            Some(PassState::Running)
        );
        tracker.apply(PassKind::Laya, &json!({"type": "done", "summary": "ok"}));
        assert!(tracker.begin_laya(), "a finished check can run again");
    }

    #[test]
    fn laya_pass_folds_progress_and_done() {
        let tracker = IndexingTracker::new();
        assert!(tracker.begin_laya());
        tracker.apply(
            PassKind::Laya,
            &json!({"type": "progress", "done": 3, "total": 9}),
        );
        let running = tracker.snapshot().laya.expect("running laya pass");
        assert_eq!((running.done, running.total), (3, 9));
        assert_eq!(running.state, PassState::Running);
        tracker.apply(
            PassKind::Laya,
            &json!({"type": "done", "summary": "9 checks"}),
        );
        let done = tracker.snapshot().laya.expect("done laya pass");
        assert_eq!(done.state, PassState::Done);
        assert_eq!(done.done, 9);
        assert_eq!(done.detail.as_deref(), Some("9 checks"));
    }

    #[test]
    fn a_running_laya_pass_does_not_touch_the_code_pass_or_first_run() {
        let tracker = IndexingTracker::new();
        tracker.begin_code_pass(true);
        tracker.apply(PassKind::Code, &json!({"type": "done", "summary": "12 files"}));
        let before = tracker.snapshot();
        assert!(tracker.begin_laya());
        let after = tracker.snapshot();
        assert_eq!(after.code, before.code);
        assert_eq!(after.memory, before.memory);
        assert_eq!(after.first_run, before.first_run);
        assert_eq!(after.laya.map(|pass| pass.state), Some(PassState::Running));
        tracker.clear_laya();
        assert_eq!(tracker.snapshot().laya, None);
    }

    #[test]
    fn the_health_pass_shows_running_then_done_with_its_counts() {
        // An in-sync reconcile leaves no Memory pass at all: the check's line
        // must still appear, running first and then done with the counts.
        let mut snapshot = IndexingSnapshot::default();
        health_running(&mut snapshot);
        let pass = snapshot.memory.clone().expect("running memory pass");
        assert_eq!(pass.state, PassState::Running);
        assert_eq!((pass.done, pass.total), (0, 0));
        health_done(&mut snapshot, "health: 3 records cite missing paths".into());
        let pass = snapshot.memory.clone().expect("done memory pass");
        assert_eq!(pass.state, PassState::Done);
        assert_eq!(
            pass.detail.as_deref(),
            Some("health: 3 records cite missing paths")
        );

        // After a reconcile the pass's own summary survives, joined with ` · `.
        let mut snapshot = IndexingSnapshot {
            memory: Some(IndexPass {
                state: PassState::Done,
                done: 7,
                total: 7,
                detail: Some("derived index in sync".into()),
            }),
            ..IndexingSnapshot::default()
        };
        health_running(&mut snapshot);
        let pass = snapshot.memory.clone().expect("running");
        assert_eq!(pass.state, PassState::Running);
        assert_eq!(pass.detail.as_deref(), Some("derived index in sync"));
        health_done(&mut snapshot, "health: 0 records cite missing paths".into());
        let pass = snapshot.memory.expect("done");
        assert_eq!(pass.state, PassState::Done);
        assert_eq!(
            pass.detail.as_deref(),
            Some("derived index in sync · health: 0 records cite missing paths")
        );
    }

    #[test]
    fn the_health_pass_never_hides_a_failed_reconcile() {
        let mut snapshot = IndexingSnapshot {
            memory: Some(IndexPass {
                state: PassState::Failed,
                done: 3,
                total: 7,
                detail: Some("boom".into()),
            }),
            ..IndexingSnapshot::default()
        };
        // The window renders the Failed pass's detail as the error's title, so
        // the advisory check must not touch either field.
        health_running(&mut snapshot);
        let pass = snapshot.memory.clone().expect("failed memory pass");
        assert_eq!(pass.state, PassState::Failed);
        assert_eq!(pass.detail.as_deref(), Some("boom"));
        health_done(&mut snapshot, "health: 0 records cite missing paths".into());
        let pass = snapshot.memory.expect("failed memory pass");
        assert_eq!(pass.state, PassState::Failed);
        assert_eq!(pass.detail.as_deref(), Some("boom"));
        assert_eq!((pass.done, pass.total), (3, 7));
    }
}

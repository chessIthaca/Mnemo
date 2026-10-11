# Job 1 — move the brain and the spawn path into `mnemo::host`

Parent spec: `.coding/specs/headless-free-agent.md` §5.1, §14. Tier: **deepseek**
for every step below. Anchors are from commit `f82ecfe`; match on the quoted
text, not the line number. Each step is its own ALF job, reviewed and merged
before the next starts.

**Rules for the worker (all steps).**
- Move code, don't rewrite it. Copy the quoted regions verbatim. The only
  edits allowed are the ones listed in that step's replacement table.
- Do not rename anything, rewrite doc comments, add `#[allow(...)]`, delete or
  weaken a test, or touch a file that is not in the step's file list.
- Both crates have `#![deny(warnings)]`. After a move, `cargo build` will report
  `unused import` errors at the old location. Fix each by deleting exactly that
  import (or that name inside a `use {...}` list). That is the only
  compiler-driven edit allowed.
- Run each verify command once, in order. After 3 failed fix attempts on any
  command, stop and report.
- **If any quoted anchor is not found exactly once, or the code you find does
  not match this file, stop and report what differs. Don't redesign.**
- Tests stay out of the real repo (a temp git repo). Leak check before and after:
  `git worktree list`, `git branch --list "alf/*"`, `git status --short`
  must be identical.
- Report: files changed (one line each), each verify command with pass/fail,
  every deviation from this file and why.

---

## Step 1.1 — move the pure startup / maintenance / index-progress helpers

Goal: three app-crate helper groups move into the lib unchanged; the app keeps
compiling through `pub(crate) use` re-exports. No behaviour change.

Files: NEW `src/host/mod.rs`, `src/host/startup.rs`, `src/host/maintenance.rs`,
`src/host/index_progress.rs`; EDIT `src/lib.rs`, `src-tauri/src/startup.rs`,
`src-tauri/src/ipc/memory_maintenance.rs`, `src-tauri/src/ipc/codegraph_cmds.rs`,
`src-tauri/src/ipc/error.rs`. No other file.

### 1.1.a `src/lib.rs`
After the line `pub mod error;` add the line `pub mod host;` (keep the module
list alphabetical: it goes between `error` and `index_staleness`).

### 1.1.b `src/host/mod.rs` (new, type verbatim)
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Host-side wiring shared by every frontend (the Tauri GUI, the console, and
//! the headless API): startup helpers, the maintenance busy-flag, and the
//! startup code-graph index progress. Nothing in this module may depend on
//! Tauri or on the app crate.

pub mod index_progress;
pub mod maintenance;
pub mod startup;
```

### 1.1.c `src/host/startup.rs` (new)
Header (verbatim):
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Startup helpers shared by the GUI, the console and the headless API —
//! pure functions over config and process arguments.

use crate::config::{Config, Endpoint};
```
Then move from `src-tauri/src/startup.rs`, each with the `///` doc comment
directly above it, in this order:
1. `pub(crate) fn project_flag_from_args(` … through its closing `}`.
2. `pub(crate) fn resolve_startup_provider(config: &Config) -> (Option<&Endpoint>, String) {` … `}`.
3. `pub(crate) fn should_reembed_at_startup(` … `}`.

Replacement table (inside the moved text only):

| old | new | expected count |
|---|---|---|
| `pub(crate) fn` | `pub fn` | 3 |
| `mnemo::memory::embedder::EmbedderStatus` | `crate::memory::embedder::EmbedderStatus` | 2 |
| `mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH` | `crate::config::EMBEDDING_MODEL_SENTINEL_HASH` | 1 |

In `src-tauri/src/startup.rs`: delete the three moved functions (and their doc
comments) and add, directly after the `use mnemo::config::{Config, Endpoint};`
line:
```rust
pub(crate) use mnemo::host::startup::{
    project_flag_from_args, resolve_startup_provider, should_reembed_at_startup,
};
```
Leave `console_mode_requested`, `window_title`, `instance_pid_alive` and the
whole `mod tests` untouched (the tests reach the moved functions through
`use super::*`).

### 1.1.d `src/host/maintenance.rs` (new)
Header (verbatim):
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The memory-maintenance single-operation guard, the reconcile event the
//! startup derived-index pass reports, and the progress-forwarding throttle.
//! Moved from the app crate so the brain builder can live in the lib.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
```
Then move from `src-tauri/src/ipc/memory_maintenance.rs`, each with its doc
comment, in this order:
1. `pub const RECONCILE_CHANNEL: &str = "memory://reconcile";`
2. the `pub enum ReconcileEvent { … }` with its `#[derive]` and `#[serde]` lines.
3. `static BUSY: AtomicBool = AtomicBool::new(false);`
4. `pub(crate) struct BusyGuard;` and the `impl Drop for BusyGuard { … }`.
5. `pub(crate) fn claim_busy() -> Result<(), IpcError> { … }`.
6. `pub(crate) const PROGRESS_EVERY: usize = 50;`
7. `pub(crate) fn should_forward_progress(done: usize, total: usize, every: usize) -> bool { … }`.

Then add this new type directly ABOVE `claim_busy` (verbatim):
```rust
/// Returned by [`claim_busy`] when another maintenance operation holds the
/// flag. Carries the user-facing message the app surfaces as an IPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceBusy {
    /// The message to show.
    pub message: String,
}

impl std::fmt::Display for MaintenanceBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MaintenanceBusy {}
```

Replacement table (inside the moved text only):

| old | new | expected count |
|---|---|---|
| `pub(crate) struct BusyGuard;` | `pub struct BusyGuard;` | 1 |
| `pub(crate) fn claim_busy() -> Result<(), IpcError> {` | `pub fn claim_busy() -> Result<(), MaintenanceBusy> {` | 1 |
| `Err(IpcError::msg(` | `Err(MaintenanceBusy { message: String::from(` | 1 |
| the `))` that closes that `Err(IpcError::msg(...))` | `) })` | 1 |
| `pub(crate) const PROGRESS_EVERY` | `pub const PROGRESS_EVERY` | 1 |
| `pub(crate) fn should_forward_progress` | `pub fn should_forward_progress` | 1 |
| `#[derive(Debug, Clone, Serialize)]` on `ReconcileEvent` | `#[derive(Debug, Clone, Serialize, Deserialize)]` | 1 |

The `#[serde(tag = "type", rename_all = "kebab-case")]` line on
`ReconcileEvent` stays exactly as it is (the wire format must not change).

In `src-tauri/src/ipc/memory_maintenance.rs`: delete the seven moved items and
add, after the last `use crate::...;` line:
```rust
pub(crate) use mnemo::host::maintenance::{
    claim_busy, should_forward_progress, BusyGuard, ReconcileEvent, PROGRESS_EVERY,
    RECONCILE_CHANNEL,
};
```
In `src-tauri/src/ipc/error.rs`, after the existing `impl From<&str> for IpcError { … }`
block, add (verbatim):
```rust
impl From<mnemo::host::maintenance::MaintenanceBusy> for IpcError {
    fn from(e: mnemo::host::maintenance::MaintenanceBusy) -> Self {
        IpcError::msg(e.message)
    }
}
```
(`claim_busy()?;` in `memory_maintenance.rs` then compiles unchanged.)

### 1.1.e `src/host/index_progress.rs` (new)
Header (verbatim):
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Startup code-graph index progress: the event the host forwards to its UI
//! and the process-wide snapshot the `get_index_progress` command reads.
//! Moved from the app crate so the brain builder can live in the lib.

use serde::{Deserialize, Serialize};

use crate::host::maintenance::{should_forward_progress, PROGRESS_EVERY};
```
Then move from `src-tauri/src/ipc/codegraph_cmds.rs`, each with its doc
comment, in this order:
1. `pub const INDEX_PROGRESS_CHANNEL: &str = "codegraph://index-progress";`
2. the `pub enum IndexProgressEvent { … }` with its `#[derive]` and `#[serde]` lines.
3. `pub(crate) fn startup_should_forward(` … `}`.
4. the three statics `STARTUP_INDEX_ACTIVE`, `STARTUP_INDEX_DONE`, `STARTUP_INDEX_TOTAL`.
5. `pub(crate) fn mark_startup_index_active(switched: bool) { … }`
6. `pub(crate) fn ensure_startup_index_visible() { … }`
7. `pub(crate) fn record_startup_index_progress(done: usize, total: usize) { … }`
8. `pub(crate) fn clear_startup_index() { … }`
9. `pub(crate) fn startup_index_snapshot() -> Option<(usize, usize)> { … }`

`emit_index_progress`, `IndexProgressSnapshot`, `get_index_progress`,
`ReindexEvent`, `emit_reindex` and everything else in `codegraph_cmds.rs` stay.

Replacement table (inside the moved text only):

| old | new | expected count |
|---|---|---|
| `pub(crate) fn` | `pub fn` | 6 |
| `#[derive(Debug, Clone, Serialize)]` on `IndexProgressEvent` | `#[derive(Debug, Clone, Serialize, Deserialize)]` | 1 |

In `src-tauri/src/ipc/codegraph_cmds.rs`: delete the moved items and add, after
the line `use crate::ipc::memory_maintenance::{should_forward_progress, PROGRESS_EVERY};`:
```rust
pub(crate) use mnemo::host::index_progress::{
    clear_startup_index, ensure_startup_index_visible, mark_startup_index_active,
    record_startup_index_progress, startup_index_snapshot, startup_should_forward,
    IndexProgressEvent, INDEX_PROGRESS_CHANNEL,
};
```

### Verify (run once each, in order)
```
cargo build --workspace
cargo test --workspace
cargo test -p mnemo
cargo clippy --workspace --all-targets -- -D warnings
```
Mechanical acceptance checks (all must hold):
- `grep -c "tauri" src/host/mod.rs src/host/startup.rs src/host/maintenance.rs src/host/index_progress.rs` → `0` for every file.
- `grep -c "fn project_flag_from_args\|fn resolve_startup_provider\|fn should_reembed_at_startup" src-tauri/src/startup.rs` → `0`.
- `grep -c "fn claim_busy\|struct BusyGuard\|enum ReconcileEvent\|fn should_forward_progress" src-tauri/src/ipc/memory_maintenance.rs` → `0`.
- `grep -c "STARTUP_INDEX_\|enum IndexProgressEvent" src-tauri/src/ipc/codegraph_cmds.rs` → `0`.
- These existing tests pass unchanged: `project_flag_*`, `provider_*`, `reembed_*` (src-tauri/src/startup.rs) and `startup_snapshot_records_and_clears` (src-tauri/src/ipc/codegraph_cmds.rs).
- `git diff --stat` touches only the nine files listed.

---

## Step 1.2 — the notifier seam; `Brain` + `build_brain` move into the lib

Goal: `build_brain` lives in `mnemo::host::brain` and talks to its host through
`HostNotifier` instead of `tauri::AppHandle`. The managed Laya classifier
(app-owned) is attached by the app after the build. Wire events to the webview
are unchanged (same channels, same payload types).

Files: NEW `src/host/notifier.rs`, `src/host/brain.rs`; EDIT `src/host/mod.rs`,
`src-tauri/src/main.rs`, `src-tauri/src/console.rs`. No other file.

### 1.2.a `src/host/notifier.rs` (new, verbatim)
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The seam between the brain and whatever hosts it (the Tauri GUI, the
//! console, the headless API). The brain reports background startup work
//! here; the host decides what to do with it (the GUI forwards each variant
//! to the webview channel it already listens on).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::host::index_progress::IndexProgressEvent;
use crate::host::maintenance::ReconcileEvent;

/// A background event the brain reports to its host. Each variant maps 1:1 to
/// a webview channel the Tauri app already uses: `Reconcile` →
/// `memory://reconcile`, `IndexProgress` → `codegraph://index-progress`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", content = "event", rename_all = "snake_case")]
pub enum HostEvent {
    /// The startup derived-index reconcile (memory maintenance).
    Reconcile(ReconcileEvent),
    /// The startup code-graph index pass.
    IndexProgress(IndexProgressEvent),
}

/// Receives [`HostEvent`]s from the brain. Implemented by each frontend.
pub trait HostNotifier: Send + Sync + 'static {
    /// Deliver one event. Must not block.
    fn notify(&self, event: HostEvent);
}

/// The shared handle the brain holds.
pub type SharedNotifier = Arc<dyn HostNotifier>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_event_serde_roundtrip() {
        let events = vec![
            HostEvent::Reconcile(ReconcileEvent::Started),
            HostEvent::Reconcile(ReconcileEvent::Progress { done: 1, total: 3 }),
            HostEvent::Reconcile(ReconcileEvent::Done {
                summary: "ok".to_string(),
            }),
            HostEvent::Reconcile(ReconcileEvent::Failed {
                error: "boom".to_string(),
            }),
            HostEvent::IndexProgress(IndexProgressEvent::Started),
            HostEvent::IndexProgress(IndexProgressEvent::Progress { done: 2, total: 5 }),
            HostEvent::IndexProgress(IndexProgressEvent::Done {
                summary: "ok".to_string(),
            }),
            HostEvent::IndexProgress(IndexProgressEvent::Failed {
                error: "boom".to_string(),
            }),
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).expect("serialize");
            let back: HostEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                serde_json::to_string(&back).expect("re-serialize"),
                json,
                "round-trip must be byte-stable"
            );
        }
    }
}
```

### 1.2.b `src/host/mod.rs`
Replace the module list with:
```rust
#[cfg(all(feature = "browser", feature = "embeddings"))]
pub mod brain;
pub mod index_progress;
pub mod maintenance;
pub mod notifier;
pub mod startup;
```
(The brain references `crate::browser::BrowserManager`, which only exists
under the `browser` feature; the app enables both features. Un-gating is a
later job, recorded as deferral D1 in the parent spec.)

### 1.2.c `src/host/brain.rs` (new)
Header (verbatim):
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The brain: config, project, memory store, provider, model resolver, safety,
//! code graph, MCP, skills and the agent factory — built once per process and
//! shared by every agent. Moved from the Tauri app's `main.rs`; frontends pass
//! a [`HostNotifier`] for background startup events and a Tokio handle for the
//! background tasks the build spawns.

use std::sync::{Arc, RwLock};

use crate::agent::context::ContextManager;
use crate::agent::factory::AgentLoopFactory;
use crate::config::{global_config_dir, Config, SafetyMode};
use crate::host::index_progress::{
    clear_startup_index, ensure_startup_index_visible, mark_startup_index_active,
    record_startup_index_progress, startup_should_forward, IndexProgressEvent,
};
use crate::host::maintenance::{claim_busy, BusyGuard, ReconcileEvent};
use crate::host::notifier::{HostEvent, SharedNotifier};
use crate::host::startup::{
    project_flag_from_args, resolve_startup_provider, should_reembed_at_startup,
};
use crate::memory::embedder::HashEmbedder;
use crate::memory::MemoryStore;
use crate::project::Project;
use crate::provider::trace::LlmRequestLog;
use crate::provider::{LlmClient, ProviderKind};
use crate::safety_rules::SafetyRules;
use crate::tool::agent::sandbox::Sandbox;
```
Then ALSO copy, from the top of `src-tauri/src/main.rs`, the two import groups
that start with `use mnemo::provider::client_factory::{` and
`use mnemo::provider::openai::{`, verbatim, replacing `mnemo::` with `crate::`.
(If `cargo build` later reports any of these imports unused, delete that name.)

Then move from `src-tauri/src/main.rs`, each with the doc comment directly
above it, in this order:
1. `pub(crate) struct Brain { … }` (the whole struct).
2. `pub(crate) enum BrainOutcome { … }`.
3. `pub(crate) fn build_brain(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> { … }`.
4. `fn build_brain_inner(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> { … }` (the whole function, ~1000 lines, up to and including the `}` that follows `Ok(BrainOutcome::Ready(Brain { … }))`).

Do NOT move `mod switch_restart_focus_contract` (the test module after it).

Replacement table (inside the moved text only). Counts are exact; if a count
differs, stop and report.

| # | old | new | count |
|---|---|---|---|
| R1 | `pub(crate) struct Brain {` | `pub struct Brain {` | 1 |
| R2 | `pub(crate) enum BrainOutcome {` | `pub enum BrainOutcome {` | 1 |
| R3 | every `    pub(crate) ` field prefix inside `Brain` | `    pub ` | 11 |
| R4 | these six private fields inside `Brain`: `    sandbox: Arc<Sandbox>,`, `    safety_rules: Arc<SafetyRules>,`, `    safety_mode: Arc<RwLock<SafetyMode>>,`, `    model_resolver: Arc<mnemo::model_resolver::ConfigModelResolver>,`, `    pending_model_download: Option<String>,`, `    _graph_watcher: Option<mnemo::codegraph::watcher::GraphWatcher>,` | the same line with `    pub ` in front | 6 |
| R5 | the two fields `    pub(crate) laya: Arc<ipc::laya::LayaManager>,` and `    pub(crate) pending_laya_start: Option<(String, u16)>,` (with their doc comments) | delete | 2 |
| R6 | `pub(crate) fn build_brain(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> {` | `pub fn build_brain(notifier: Option<SharedNotifier>, rt: tokio::runtime::Handle) -> anyhow::Result<BrainOutcome> {` | 1 |
| R7 | `let outcome = build_brain_inner(app);` | `let outcome = build_brain_inner(notifier, rt);` | 1 |
| R8 | `fn build_brain_inner(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> {` | `fn build_brain_inner(notifier: Option<SharedNotifier>, rt: tokio::runtime::Handle) -> anyhow::Result<BrainOutcome> {` | 1 |
| R9 | `startup::project_flag_from_args(` | `project_flag_from_args(` | 1 |
| R10 | `startup::resolve_startup_provider(` | `resolve_startup_provider(` | 1 |
| R11 | `startup::should_reembed_at_startup(` | `should_reembed_at_startup(` | 1 |
| R12 | the block from the line `    let laya = Arc::new(ipc::laya::LayaManager::new(classifier_status.clone()));` through the line `    };` that closes `let classifier = if config.general.general.laya.enabled {` (it ends `} else {` / `None` / `};`) — copy this block aside first, step 1.2.d needs it verbatim | `    let classifier: Option<Arc<dyn crate::memory::classifier::Classifier>> = None;` | 1 block |
| R13 | `ipc::memory_maintenance::claim_busy()` | `claim_busy()` | 2 |
| R14 | `ipc::memory_maintenance::BusyGuard` | `BusyGuard` | 2 |
| R15 | `let app_handle = app.clone();` | `let notifier = notifier.clone();` | 2 |
| R16 | `let Some(app_handle) = app_handle else {` | `let Some(notifier) = notifier else {` | 1 |
| R17 | `tauri::async_runtime::spawn(async move {` | `rt.spawn(async move {` | 3 |
| R18 | each `let _ = app_handle.emit(` / `ipc::memory_maintenance::RECONCILE_CHANNEL,` / `&ipc::memory_maintenance::ReconcileEvent::X…,` / `);` call | `notifier.notify(HostEvent::Reconcile(ReconcileEvent::X…));` with the same payload expression | 4 (Started, Progress { done, total }, Done { summary }, Failed { error: e.to_string() }) |
| R19 | the three lines `    if let Some(app) = app.as_ref() {` / `        attach_child_ensurer(&browser, app.clone());` / `    }` | delete | 1 block |
| R20 | `match app_handle {` | `match notifier {` | 1 |
| R21 | `Some(handle) => {` | `Some(notifier) => {` | 1 |
| R22 | `ipc::codegraph_cmds::mark_startup_index_active(` | `mark_startup_index_active(` | 1 |
| R23 | `ipc::codegraph_cmds::record_startup_index_progress(` | `record_startup_index_progress(` | 1 |
| R24 | `ipc::codegraph_cmds::startup_should_forward(` | `startup_should_forward(` | 1 |
| R25 | `ipc::codegraph_cmds::ensure_startup_index_visible();` | `ensure_startup_index_visible();` | 1 |
| R26 | `ipc::codegraph_cmds::clear_startup_index();` | `clear_startup_index();` | 1 |
| R27 | `let cb_app = handle.clone();` | `let cb_notifier = notifier.clone();` | 1 |
| R28 | each `ipc::codegraph_cmds::emit_index_progress(` / `&handle,` or `&cb_app,` / `&ipc::codegraph_cmds::IndexProgressEvent::X { … },` / `);` call | `notifier.notify(HostEvent::IndexProgress(IndexProgressEvent::X { … }));` — use `cb_notifier.notify(` for the one that used `&cb_app` | 5 (Started; Progress { done, total }; Done { summary }; Failed { error } ×2) |
| R29 | every remaining `mnemo::` | `crate::` | all |
| R30 | in the `Brain { … }` literal at the end of `build_brain_inner`: the two lines `        laya,` and `        pending_laya_start,` | delete | 2 |

After R29 there must be no `tauri::`, `ipc::`, `startup::`, `app_handle`,
`app.` or `mnemo::` left in `src/host/brain.rs` (acceptance greps below).

### 1.2.d `src-tauri/src/main.rs`
1. Delete the four moved items (`Brain`, `BrainOutcome`, `build_brain`,
   `build_brain_inner`) with their doc comments. Keep
   `mod switch_restart_focus_contract`.
2. After the line `use ipc::PendingQuestions;` add:
   ```rust
   pub(crate) use mnemo::host::brain::{build_brain, Brain, BrainOutcome};
   use mnemo::host::notifier::{HostEvent, HostNotifier};
   ```
3. Directly above `fn attach_child_ensurer(` add (verbatim):
   ```rust
   /// Forwards the brain's background events to the webview, on the channels
   /// the frontend already listens on (unchanged payloads).
   struct TauriNotifier(tauri::AppHandle);

   impl HostNotifier for TauriNotifier {
       fn notify(&self, event: HostEvent) {
           match event {
               HostEvent::Reconcile(ev) => {
                   let _ = self.0.emit(ipc::memory_maintenance::RECONCILE_CHANNEL, &ev);
               }
               HostEvent::IndexProgress(ev) => {
                   ipc::codegraph_cmds::emit_index_progress(&self.0, &ev);
               }
           }
       }
   }

   /// The managed Laya classifier is app-owned (its runtime talks to the
   /// webview), so the kernel brain leaves the classifier slot empty and this
   /// fills it before the first agent spawns. The body is the block that used
   /// to live in `build_brain_inner`, unchanged.
   pub(crate) fn attach_laya(
       brain: &Brain,
       laya_cfg: &mnemo::config::LayaConfig,
   ) -> (Arc<ipc::laya::LayaManager>, Option<(String, u16)>) {
       let classifier_status = brain.classifier_status.clone();
       // <<< paste the block copied aside in R12 here, verbatim >>>
       *brain
           .classifier_slot
           .write()
           .expect("classifier slot lock poisoned") = classifier;
       (laya, pending_laya_start)
   }
   ```
   Inside the pasted block make exactly one edit: `config.general.general.laya.enabled`
   → `laya_cfg.enabled` (count 1).
4. Replace `let brain_result = build_brain(Some(app.handle().clone()));` with:
   ```rust
   let brain_result = build_brain(
       Some(Arc::new(TauriNotifier(app.handle().clone()))),
       tauri::async_runtime::handle().inner().clone(),
   );
   ```
5. In the arm `Ok(BrainOutcome::Ready(brain)) => {` that contains
   `let mut mgr = AgentManager::new(256);`, insert as its FIRST lines:
   ```rust
   attach_child_ensurer(&brain.browser, app.handle().clone());
   let laya_cfg = tauri::async_runtime::block_on(async {
       brain.config.lock().await.general.general.laya.clone()
   });
   let (laya, pending_laya_start) = attach_laya(&brain, &laya_cfg);
   ```
6. In that same arm replace: `laya: brain.laya.clone(),` → `laya: laya.clone(),`
   (1); `&brain.pending_laya_start` → `&pending_laya_start` (1);
   `let manager = brain.laya.clone();` → `let manager = laya.clone();` (1);
   the `brain.laya.clone(),` argument of `ipc::finetune::spawn_startup_finetune(`
   → `laya.clone(),` (1).
7. Fix `unused import` errors in `main.rs` by deleting those imports only.

### 1.2.e `src-tauri/src/console.rs`
1. Replace `let brain = match crate::build_brain(None) {` with
   `let brain = match crate::build_brain(None, tokio::runtime::Handle::current()) {`.
2. Directly after that `match` statement's closing `};` insert:
   ```rust
   let laya_cfg = brain.config.lock().await.general.general.laya.clone();
   let (laya, pending_laya_start) = crate::attach_laya(&brain, &laya_cfg);
   ```
3. Replace `if let Some((_checkpoint_id, port)) = &brain.pending_laya_start {`
   → `if let Some((_checkpoint_id, port)) = &pending_laya_start {` (1) and
   `let manager = brain.laya.clone();` → `let manager = laya.clone();` (1).

### Verify (run once each, in order)
```
cargo build --workspace
cargo test --workspace
cargo test -p mnemo --features browser,embeddings
cargo test -p mnemo
cargo clippy --workspace --all-targets -- -D warnings
```
Mechanical acceptance checks:
- `grep -c "tauri::\|ipc::\|startup::\|app_handle\|mnemo::" src/host/brain.rs` → `0`.
- `grep -c "fn build_brain_inner\|struct Brain {" src-tauri/src/main.rs` → `0`.
- `grep -c "rt.spawn(async move {" src/host/brain.rs` → `3`.
- `grep -c "notifier.notify(HostEvent::Reconcile(" src/host/brain.rs` → `4`.
- `grep -c "notify(HostEvent::IndexProgress(" src/host/brain.rs` → `5`.
- `grep -c "attach_laya(" src-tauri/src/main.rs src-tauri/src/console.rs` → main.rs `2` (definition + call), console.rs `1`.
- The new test `host::notifier::tests::host_event_serde_roundtrip` passes.
- `git diff --stat` touches only the five files listed.

The coordinator additionally smoke-runs the console (`-console`) and a GUI
debug build at review; the worker does not need to.

---

## Step 1.3 — move the spawn path and lifecycle bookkeeping into `mnemo::host::runtime`

Goal: `spawn_agent_shared`, its allow-list helpers and tests, the subagent
cleanup, the console's spawner and lifecycle bookkeeping, and the
`AgentLoopMap` alias move into the lib. The GUI's `IpcSpawner` stays in the app
and calls the moved function. No behaviour change.

Files: NEW `src/host/runtime.rs`; EDIT `src/host/mod.rs`,
`src-tauri/src/ipc/spawn.rs`, `src-tauri/src/ipc/events.rs`,
`src-tauri/src/ipc/state.rs`, `src-tauri/src/console.rs`. No other file.

### 1.3.a `src/host/mod.rs`
Add `pub mod runtime;` after `pub mod notifier;` (alphabetical).

### 1.3.b `src/host/runtime.rs` (new)
Header (verbatim):
```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shared agent spawn path and lifecycle bookkeeping used by every
//! frontend: id allocation, loop construction (main tree, per-agent plans dir,
//! worktree root), subagent allow-lists, task spawn, registration, and the
//! parent-notification bookkeeping. Moved from the app crate.

use std::sync::Arc;

use crate::agent::factory::AgentLoopFactory;
use crate::agent::AgentLoop;
use crate::runtime::agent::AgentTask;
use crate::runtime::channels::{AgentCommand, AgentHandle, AgentId};
use crate::runtime::{AgentManager, AgentSpawner, DescendantTracker, ParentAwareSpawner};
use crate::tool::{SafetyLevel, ToolCategory, ToolFilter};

/// The per-agent loop map: `Arc<tokio::Mutex<HashMap<AgentId, Arc<AgentLoop>>>>`.
/// Shared by the spawn path, the event forwarders and the IPC state.
pub type AgentLoopMap =
    Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>;
```
Then move, each with its doc comment, in this order:
1. From `src-tauri/src/ipc/spawn.rs`: `pub(crate) struct WorktreeBinding { … }`;
   `async fn parent_root_binding( … ) { … }`; `pub(crate) async fn spawn_agent_shared( … ) -> Result<(AgentId, String), String> { … }`;
   `const REVIEWER_BASE_TOOLS: &[&str] = &[ … ];`; `const ALWAYS_SAFE_ROLE_TOOLS: &[&str] = …;`;
   `async fn compute_subagent_allowlist( … ) { … }`; and the whole
   `#[cfg(test)] mod tests { … }` at the end of the file.
2. From `src-tauri/src/ipc/events.rs`: `pub(crate) async fn cleanup_inactive_subagents( … ) { … }`.
3. From `src-tauri/src/console.rs`: `pub(crate) struct ConsoleSpawner { … }`, its
   `impl ConsoleSpawner { … }`, the three `impl … for ConsoleSpawner { … }`
   blocks (`AgentSpawner`, `ParentAwareSpawner`, `DescendantTracker`), then
   `struct LifecycleBookkeeping { … }` and `impl LifecycleBookkeeping { … }`.

Replacement table (inside the moved text only):

| # | old | new | count |
|---|---|---|---|
| S1 | `pub(crate) struct WorktreeBinding {` | `pub struct WorktreeBinding {` | 1 |
| S2 | `pub(crate) async fn spawn_agent_shared(` | `pub async fn spawn_agent_shared(` | 1 |
| S3 | `pub(crate) async fn cleanup_inactive_subagents(` | `pub async fn cleanup_inactive_subagents(` | 1 |
| S4 | `crate::ipc::events::cleanup_inactive_subagents(` | `cleanup_inactive_subagents(` | 1 |
| S5 | `crate::ipc::state::AgentLoopMap` | `AgentLoopMap` | all (≥3) |
| S6 | `tauri::async_runtime::spawn(task.run(cmd_rx, fanin_tx.clone()));` | `tokio::spawn(task.run(cmd_rx, fanin_tx.clone()));` | 1 |
| S7 | `ConsoleSpawner` | `HostSpawner` | all |
| S8 | `pub(crate) struct HostSpawner {` (after S7) | `pub struct HostSpawner {` | 1 |
| S9 | `    pub(crate) fn new(` inside `impl HostSpawner` | `    pub fn new(` | 1 |
| S10 | `crate::ipc::spawn::spawn_agent_shared(` | `spawn_agent_shared(` | 1 |
| S11 | `struct LifecycleBookkeeping {` | `pub struct LifecycleBookkeeping {` | 1 |
| S12 | `    fn new(manager: Arc<tokio::sync::Mutex<AgentManager>>, agent_loops: AgentLoopMap) -> Self {` | `    pub fn new(manager: Arc<tokio::sync::Mutex<AgentManager>>, agent_loops: AgentLoopMap) -> Self {` | 1 |
| S13 | `    pub(crate) async fn on_started(` / `on_turn_ended(` / `on_exited(` | `    pub async fn …` | 3 |
| S14 | `ModelRef` (the console spawner's parameter type) | `crate::config::ModelRef` | 1 |
| S15 | every remaining `mnemo::` | `crate::` | all |

`spawn_agent_shared` is always awaited from inside a Tokio context (the GUI
calls it under `tauri::async_runtime::block_on` or from async commands; the
console from its runtime), so `tokio::spawn` (S6) is equivalent to the Tauri
spawn it replaces. The test comment in the moved `mod tests` that mentions
`tauri::async_runtime` may stay as is.

### 1.3.c app-crate edits
- `src-tauri/src/ipc/spawn.rs`: delete the moved items; add after the
  `use crate::ipc::state::IpcState;` line:
  ```rust
  pub(crate) use mnemo::host::runtime::{spawn_agent_shared, WorktreeBinding};
  ```
  Keep `spawn_agent` (the command), `spawn_run_all_agent`, and `IpcSpawner`
  with its three `impl` blocks. Fix unused imports per the compiler.
- `src-tauri/src/ipc/events.rs`: delete the moved function; add after the
  `use crate::ipc::state::IpcState;` line:
  ```rust
  pub(crate) use mnemo::host::runtime::cleanup_inactive_subagents;
  ```
- `src-tauri/src/ipc/state.rs`: delete the `pub(crate) type AgentLoopMap = …;`
  alias (and its doc comment) and put in its place:
  ```rust
  pub(crate) use mnemo::host::runtime::AgentLoopMap;
  ```
- `src-tauri/src/console.rs`: delete the moved items; replace the line
  `use crate::ipc::state::AgentLoopMap;` with
  `use mnemo::host::runtime::{AgentLoopMap, HostSpawner, LifecycleBookkeeping};`
  and replace `ConsoleSpawner::new(` with `HostSpawner::new(` (1). Fix unused
  imports per the compiler.

### Verify (run once each, in order)
```
cargo build --workspace
cargo test --workspace
cargo test -p mnemo --features browser,embeddings
cargo test -p mnemo
cargo clippy --workspace --all-targets -- -D warnings
```
Mechanical acceptance checks:
- `grep -c "tauri\|crate::ipc" src/host/runtime.rs` → `0`.
- `grep -c "fn spawn_agent_shared\|fn compute_subagent_allowlist\|REVIEWER_BASE_TOOLS" src-tauri/src/ipc/spawn.rs` → `0`.
- `grep -c "ConsoleSpawner\|struct LifecycleBookkeeping" src-tauri/src/console.rs` → `0`.
- `cargo test -p mnemo --features browser,embeddings host::runtime` runs the
  moved tests (`reviewer_base_tools_never_name_plan_mutations`,
  `parent_root_binding_returns_spec_and_plans_dir`,
  `reviewer_allowlist_intersected_with_parent_planning`,
  `reviewer_allowlist_includes_mutations_parent_has_in_executing`,
  `unrestricted_spawn_gets_parent_subset`, `no_parent_returns_none`,
  `missing_parent_loop_denies_all`) and they pass.
- `git diff --stat` touches only the six files listed.

---

## Deferred from job 1 (recorded, not for the worker)

- D1: `host::brain` is gated on `browser` + `embeddings`; un-gating with
  per-item `#[cfg]` happens in job 3a when the headless engine needs it.
- D2: `HostRuntime` (a struct owning manager + loop map + fan-in + bookkeeping,
  with `ConsoleRuntime` as a thin wrapper) is built in job 3a where the
  `Engine` needs it; job 1 only moves the pieces.
- D3: the Laya classifier manager stays app-owned (`attach_laya`); its
  Tauri-free core moves in job 4 with the setup API.

## Coordinator review checklist for job 1
- `git diff --color-moved=dimmed-zebra` shows the moved regions as moves, with
  only the tabled edits as changes.
- All greps above hold; all verify commands green; the `#![deny(warnings)]`
  cleanups removed imports only.
- Smoke: `-console` starts in a temp project and answers one prompt on the
  mock/cheap endpoint; the GUI debug build starts and the startup index /
  reconcile dialogs behave as before.
- Calibration row per step (tier deepseek; note whether any step needed a
  decision).

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Laya decision-model settings commands for Mnemo2's Settings dialog —
//! the backend of its **Laya** page (plan e02de747, design
//! `.coding/design/mnemo2-laya.md` §4).
//!
//! Mnemo2 shares Mnemo1's config dir, so every value here edits the same
//! `config.toml` the lead's engine reads, through the same kernel seams
//! Mnemo1 rides:
//!
//! - [`laya_status`] — the page's snapshot: the master `enabled` toggle,
//!   `installed`, a coarse `status` word, the seven opt-in flags and the
//!   `[general.routing]` block.
//! - [`laya_setup`] — enable + install through the kernel's own
//!   `Setup::classifier_setup(true)` leg: persist + live-apply + the
//!   managed download pipeline (uv + venv + `laya[serve]` + the English
//!   checkpoint), with [`SetupEvent`] progress streaming on
//!   [`LAYA_EVENT_CHANNEL`]. With an open lead the completing pipeline
//!   autostarts the sidecar and swaps the lead's live classifier slot —
//!   the attach; without one it installs only, and the next lead open
//!   attaches.
//! - [`laya_disable`] — the kernel's own disable leg (persist `enabled =
//!   false`, live-apply, clear the lead's slot, park `Disabled`), then
//!   stop every manager that may hold a `laya-serve` child.
//! - [`save_laya_settings`] — the seven opt-in flags + `[general.routing]`
//!   through the sanctioned `Setup::patch_settings` pipeline (the same
//!   one the Models/Savings pages ride). The master `enabled` flag never
//!   rides this patch: the kernel rejects the DTO field whole, and the
//!   page routes it through `laya_setup`/`laya_disable`, which own the
//!   install and the sidecar lifecycle.
//!
//! ## The one-managed-runtime discipline
//!
//! A running `laya-serve` child lives INSIDE the `LayaManager` that
//! spawned it — dropping the last `Arc` to that manager kills the child
//! (its `Drop` safety net). The lead's attach-time manager is held by
//! `LeadRuntime::laya_manager()` for the session. A settings-side setup
//! runs on the per-call `Setup`'s own manager (every `Setup` builds a
//! fresh one), so this module holds exactly that `Arc` —
//! [`HELD_SETUP_MANAGER`], the same role Mnemo1's `IpcState` manager
//! plays — and every path that must touch a running sidecar reuses the
//! held instances. Nothing here ever builds a second manager.
//!
//! The hold never outlives its consumer either (review finding
//! heldManagerOrphan): when the pipeline task ends, the settlement
//! ([`settle_held_manager_core`]) hands a serving manager to the open
//! lead's runtime — [`LeadRuntime::set_laya_manager`], whose `shutdown`
//! stops it like the attach-time one — or stops it directly when no
//! lead is open, and the app-exit hook ([`stop_held_manager`]) clears
//! whatever a closed race left behind. A static is never dropped; an
//! unheld orphan never is either.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use mnemo::agent::route_knn::RouteKnn;
use mnemo::agent::route_question::OfferedLabels;
use mnemo::config::general::LayaConfig;
use mnemo::config::settings_dto::{RoutingConfigDto, SettingsSaveDto};
use mnemo::config::Config;
use mnemo::headless::{EventReceiver, RecvError, Setup, SetupEvent};
use mnemo::laya::{LayaManager, english_checkpoint};
use mnemo::memory::classifier::{Classifier, ClassifierStatus};

use crate::mnemo2::dispatch_routing;
use crate::mnemo2::ipc::{settings_refresh, settings_setup, Mnemo2State};
use crate::mnemo2::route_eval;
use crate::mnemo2::runtime::LeadRuntime;
use crate::mnemo2::team::TeamRuntime;

/// The Tauri event channel `laya_setup` streams its install progress on:
/// a [`SetupEvent`] payload — `Progress` events (fractions) while the
/// managed download runs, then `Done` or `Failed`. Named like Mnemo2's
/// other channels (`mnemo2://lead-event`, `mnemo2://embedder-event`).
pub const LAYA_EVENT_CHANNEL: &str = "mnemo2://laya-event";

/// The manager the last `laya_setup` fired its pipeline through — the
/// per-call `Setup`'s own instance (every `Setup` call builds a fresh
/// manager). The completing pipeline autostarts the sidecar THROUGH this
/// manager, and a child lives inside the manager that spawned it:
/// without this hold the pipeline task's own `Arc` would be the last
/// one, and its end would kill the sidecar it just started. A static is
/// never dropped, so the hold also never OUTLIVES its consumer (review
/// finding heldManagerOrphan): the pipeline-end settlement
/// ([`settle_held_manager_core`]) hands a serving manager to the open
/// lead's runtime — whose `shutdown` stops it like the attach-time
/// manager — or stops it when no lead is open; the app-exit hook
/// ([`stop_held_manager`]) clears what a closed race left. `laya_disable`
/// and a fresh `laya_setup` also take or replace it, stopping any child
/// it holds — never two settings-side children.
static HELD_SETUP_MANAGER: Mutex<Option<Arc<LayaManager>>> = Mutex::new(None);

/// True while a settings-side setup pipeline is in flight. The kernel's
/// own `begin_setup` guard is per-manager — useless across the per-call
/// `Setup`s, which each build a fresh manager — so this flag is what
/// makes a second Install click fail fast. Claimed by `laya_setup`,
/// released when the pipeline TASK's event stream closes (the terminal
/// `Done`/`Failed` AND the autostart that follows it have both ended)
/// or on an early kernel-leg error; `laya_disable` releases it too —
/// the escape hatch for a pipeline that never reached its terminal
/// event.
static SETUP_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

// ── Wire shapes ─────────────────────────────────────────────────────────────

/// The seven `[general.laya]` opt-in flags as the Laya page shows them.
/// The master `enabled` flag rides the status object itself — its only
/// sanctioned change paths are `laya_setup`/`laya_disable`.
#[derive(Debug, Clone, Serialize)]
pub struct LayaFlagsWire {
    /// Auto-typing of memory records at `memory_write` time.
    pub auto_type_memories: bool,
    /// Failure-triage classification of failed runs.
    pub failure_triage: bool,
    /// The kNN overlay for failure triage (local, rides the memory
    /// embedder).
    pub failure_triage_knn: bool,
    /// Startup failure-triage fine-tune (managed runtime only).
    pub auto_finetune: bool,
    /// Tool-choice steering on the compound reflex.
    pub steer_tool_choice: bool,
    /// Pre-prompt model routing (shadow-first master for the routing
    /// lanes).
    pub routing: bool,
    /// The compound reflex (the lane ladder's top opt-in).
    pub reflex: bool,
}

/// The `[general.routing]` block of the `laya_status` snapshot: model
/// refs in the compact `"endpoint/model"` form (or null), the escalate
/// threshold and its enforce gate.
#[derive(Debug, Clone, Serialize)]
pub struct RoutingWire {
    /// The cheap lane's model.
    pub cheap: Option<String>,
    /// The capable lane's model.
    pub capable: Option<String>,
    /// The medium lane's model.
    pub lane_medium: Option<String>,
    /// The high lane's model.
    pub lane_high: Option<String>,
    /// The escalation model.
    pub escalate: Option<String>,
    /// The confidence gate: a decision routes only at
    /// `confidence >= threshold`.
    pub threshold: f64,
    /// Whether a confident decision actually switches the turn's model
    /// (`false` = shadow).
    pub enforce: bool,
}

/// The `laya_status` snapshot — everything the Laya page renders.
#[derive(Debug, Clone, Serialize)]
pub struct LayaStatusWire {
    /// The master `[general.laya]` toggle on disk.
    pub enabled: bool,
    /// Whether the managed English checkpoint is installed in this config
    /// dir (the Install button appears when `enabled` and not this).
    pub installed: bool,
    /// The coarse runtime state: `"disabled"`, `"not_installed"`,
    /// `"installing"`, `"starting"`, `"ready"` or `"failed"`.
    pub status: &'static str,
    /// The seven opt-in flags.
    pub flags: LayaFlagsWire,
    /// The routing lanes.
    pub routing: RoutingWire,
}

/// The `flags` payload of `save_laya_settings` — the seven opt-in flags,
/// each absent = keep. The master `enabled` key is a struct field only so
/// a caller that sends it is REJECTED with a clear message instead of
/// having it silently dropped: it never rides the settings patch (the
/// kernel rejects the DTO field whole; `laya_setup`/`laya_disable` own
/// the install and the sidecar lifecycle).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LayaFlagsPatch {
    /// Rejected when present — route the master toggle through
    /// `laya_setup`/`laya_disable`.
    pub enabled: Option<bool>,
    /// Auto-typing of memory records. Absent = keep.
    pub auto_type_memories: Option<bool>,
    /// Failure triage. Absent = keep.
    pub failure_triage: Option<bool>,
    /// The kNN overlay for failure triage. Absent = keep.
    pub failure_triage_knn: Option<bool>,
    /// Startup failure-triage fine-tune. Absent = keep.
    pub auto_finetune: Option<bool>,
    /// Tool-choice steering. Absent = keep.
    pub steer_tool_choice: Option<bool>,
    /// Pre-prompt model routing (the master flag for the lanes). Absent =
    /// keep.
    pub routing: Option<bool>,
    /// The compound reflex. Absent = keep.
    pub reflex: Option<bool>,
}

// ── Cores (the Tauri-free hearts the tests exercise) ────────────────────────

/// Read the page snapshot from a `Setup` on the lead's (or the machine
/// global) config dir. `in_flight` is the settings-side setup guard: a
/// lead-less `Setup` reads a fresh `Disabled` status Arc that cannot see
/// a running pipeline's moves, so the guard is what reports
/// "installing" then. With an open lead the engine-backed `Setup` reads
/// the brain's shared status Arc — the same Arc the pipeline and the
/// runtime's attach move.
pub(crate) fn laya_status_read(setup: &Setup, in_flight: bool) -> Result<LayaStatusWire, String> {
    let config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    let laya = &config.general.general.laya;
    let routing = &config.general.general.routing;
    let manager = setup.laya_manager();
    let installed = manager.is_checkpoint_installed(english_checkpoint().id);
    let live = manager
        .status_arc()
        .read()
        .expect("classifier status lock poisoned")
        .clone();
    Ok(LayaStatusWire {
        enabled: laya.enabled,
        installed,
        status: laya_status_word(laya, installed, &live, in_flight),
        flags: LayaFlagsWire {
            auto_type_memories: laya.auto_type_memories,
            failure_triage: laya.failure_triage,
            failure_triage_knn: laya.failure_triage_knn,
            auto_finetune: laya.auto_finetune,
            steer_tool_choice: laya.steer_tool_choice,
            routing: laya.routing,
            reflex: laya.reflex,
        },
        routing: RoutingWire {
            cheap: config.tiers.first_model("trivial").map(str::to_string),
            capable: config.tiers.first_model("high").map(str::to_string),
            lane_medium: config.tiers.first_model("medium").map(str::to_string),
            lane_high: config.tiers.first_model("high").map(str::to_string),
            escalate: config.tiers.first_model("escalate").map(str::to_string),
            threshold: routing.threshold,
            enforce: routing.enforce,
        },
    })
}

/// Map the live classifier state to the page's coarse status word.
/// `in_flight` covers the window where a settings-side pipeline is
/// genuinely running but the status Arc this command reads is not the one
/// it drives (no lead open — a fresh `Disabled` Arc), or has not been
/// moved yet; a mid-flight `laya_disable` leaves the pipeline running, so
/// "installing" stays honest until its terminal event clears the guard.
fn laya_status_word(
    laya: &LayaConfig,
    installed: bool,
    live: &ClassifierStatus,
    in_flight: bool,
) -> &'static str {
    if in_flight && matches!(live, ClassifierStatus::Disabled) {
        return "installing";
    }
    match live {
        ClassifierStatus::Disabled => {
            if laya.enabled && !installed {
                "not_installed"
            } else {
                "disabled"
            }
        }
        ClassifierStatus::Installing | ClassifierStatus::Downloading { .. } => "installing",
        ClassifierStatus::Starting => "starting",
        // The sidecar keeps serving the current checkpoint through a
        // fine-tune — coarse state: ready.
        ClassifierStatus::Ready | ClassifierStatus::FineTuning { .. } => "ready",
        ClassifierStatus::Failed => "failed",
    }
}

/// The `laya_setup` core: claim the settings-side in-flight guard, hold
/// the per-call `Setup`'s own manager (see [`HELD_SETUP_MANAGER`]), then
/// fire the kernel's own enable leg — `Setup::classifier_setup(true)`,
/// which persists the flag, live-applies it and spawns the managed
/// download pipeline. With an open lead the completing pipeline
/// autostarts the sidecar and swaps the lead's classifier slot (the
/// attach); without one it installs only — the next lead open attaches.
pub(crate) async fn laya_setup_core(
    setup: &Setup,
    hold: &Mutex<Option<Arc<LayaManager>>>,
    in_flight: &AtomicBool,
) -> Result<EventReceiver<SetupEvent>, String> {
    if in_flight.swap(true, Ordering::SeqCst) {
        return Err("a Laya setup is already running".into());
    }
    // Hold BEFORE firing: the pipeline task owns its own Arc clone for as
    // long as it runs, and this hold is what keeps the sidecar it spawns
    // alive past the task's end. Replacing a previous hold drops that Arc
    // (its pipeline has long ended) and with it the old sidecar — never
    // two settings-side children.
    *hold.lock().expect("laya hold lock poisoned") = Some(Arc::clone(setup.laya_manager()));
    match setup.classifier_setup(true).await {
        Ok(events) => Ok(events),
        Err(err) => {
            // The kernel leg failed before its pipeline ran (a persist
            // failure, or its own per-manager guard) — release ours so
            // the page can retry.
            in_flight.store(false, Ordering::SeqCst);
            Err(err.to_string())
        }
    }
}

/// The `laya_disable` core: the kernel's own disable leg — persist
/// `enabled = false`, live-apply it (clearing an open lead's classifier
/// slot) and park `Disabled` — then stop every manager that may hold a
/// sidecar child: the lead's attach-time manager and the settings-side
/// hold. `LayaManager::stop` is a `false` no-op on a childless manager,
/// so an extra stop is always safe. Releasing `in_flight` doubles as the
/// escape hatch for a pipeline that never reached its terminal event.
pub(crate) async fn laya_disable_core(
    setup: &Setup,
    managers: &[Arc<LayaManager>],
    in_flight: &AtomicBool,
) -> Result<(), String> {
    let events = setup
        .classifier_setup(false)
        .await
        .map_err(|err| err.to_string())?;
    for manager in managers {
        manager.stop();
    }
    in_flight.store(false, Ordering::SeqCst);
    // The single terminal `Done` rides the receiver; nobody listens on
    // disable — dropping it closes the channel.
    drop(events);
    Ok(())
}

/// The `save_laya_settings` core: the seven opt-in flags + the
/// `[general.routing]` block through the sanctioned patch pipeline. A
/// present `enabled` key is rejected with the message that routes it to
/// `laya_setup`/`laya_disable` — the kernel would reject the DTO field
/// whole, but this message names the right commands.
pub(crate) async fn save_laya_settings_core(
    setup: &Setup,
    flags: Option<LayaFlagsPatch>,
    routing: Option<RoutingConfigDto>,
) -> Result<(), String> {
    let flags = flags.unwrap_or_default();
    if flags.enabled.is_some() {
        return Err(
            "the Laya `enabled` flag never rides the settings patch — use \
             laya_setup or laya_disable (they own the install and the sidecar \
             lifecycle)"
                .into(),
        );
    }
    let patch = SettingsSaveDto {
        laya_auto_type_memories: flags.auto_type_memories,
        laya_steer_tool_choice: flags.steer_tool_choice,
        laya_failure_triage: flags.failure_triage,
        laya_failure_triage_knn: flags.failure_triage_knn,
        laya_auto_finetune: flags.auto_finetune,
        laya_routing: flags.routing,
        laya_reflex: flags.reflex,
        routing,
        ..SettingsSaveDto::default()
    };
    setup.patch_settings(patch).await.map_err(|err| err.to_string())
}

/// Forward the setup pipeline's events to [`LAYA_EVENT_CHANNEL`] until
/// the stream closes, then release the in-flight guard and settle the
/// settings-side hold ([`settle_held_manager_core`]). The close is the
/// pipeline TASK's end: the terminal `Done`/`Failed` rode the stream,
/// and the autostart the task starts AFTER `Done` (sidecar spawn +
/// readiness probe + the live-slot swap) has landed or failed by then,
/// so the manager's child state is final. Forwarding until that close —
/// not stopping at the terminal event — is what lets the settlement see
/// the outcome, and it keeps a second Install click failing fast while
/// the sidecar itself is still coming up.
fn spawn_laya_forwarder(
    app: AppHandle,
    mut events: EventReceiver<SetupEvent>,
    manager: Arc<LayaManager>,
    in_flight: &'static AtomicBool,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit(LAYA_EVENT_CHANNEL, &event);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        in_flight.store(false, Ordering::SeqCst);
        settle_held_manager(&app, manager).await;
    });
}

/// The async shell of [`settle_held_manager_core`]: resolve the open
/// lead through the sanctioned `settings_setup` accessor
/// (`Mnemo2State`'s runtime field is private), then settle. A
/// `settings_setup` failure falls back to the no-lead arm: right after
/// the pipeline's own successful config save an unreadable config is a
/// torn state, and stopping a sidecar nobody can name a consumer for is
/// the safe side of the orphan trade-off (the app-exit hook still runs
/// later).
async fn settle_held_manager(app: &AppHandle, manager: Arc<LayaManager>) {
    let runtime = match settings_setup(&app.state::<Mnemo2State>()).await {
        Ok((_, runtime)) => runtime,
        Err(_) => None,
    };
    settle_held_manager_core(&HELD_SETUP_MANAGER, runtime.as_deref(), manager);
}

/// Settle the settings-side hold at the pipeline task's end (review
/// finding heldManagerOrphan): a live sidecar must never outlive its
/// consumer or the app, and the static hold is never dropped.
///
/// - live sidecar (the manager's status reads `Starting`/`Ready`/
///   `FineTuning` — the autostart's outcome) + an open lead → hand the
///   manager to that session ([`LeadRuntime::set_laya_manager`], which
///   stops the manager it replaces — its child was superseded by the
///   live-slot swap that landed with this pipeline's sidecar), so
///   `LeadRuntime::shutdown` stops it like the attach-time manager;
/// - live sidecar + no lead → stop it now: a sidecar serving a closed
///   project's slot is an orphan (the next lead open attaches through
///   its own manager);
/// - no live sidecar (a failed or disabled pipeline — the child was
///   scoped-torn-down or never spawned) → just clear the hold: the
///   session's own manager, if any, keeps serving the untouched slot.
///
/// The hold is only touched while it still points at THIS manager — a
/// mid-flight `laya_disable` takes it, a newer `laya_setup` replaces it.
fn settle_held_manager_core(
    hold: &Mutex<Option<Arc<LayaManager>>>,
    runtime: Option<&LeadRuntime>,
    manager: Arc<LayaManager>,
) {
    let live = {
        let status = manager
            .status_arc()
            .read()
            .expect("classifier status lock poisoned");
        matches!(
            *status,
            ClassifierStatus::Starting
                | ClassifierStatus::Ready
                | ClassifierStatus::FineTuning { .. }
        )
    };
    let mut held = hold.lock().expect("laya hold lock poisoned");
    if !held.as_ref().is_some_and(|current| Arc::ptr_eq(current, &manager)) {
        return;
    }
    match (live, runtime) {
        (true, Some(runtime)) => {
            *held = None;
            runtime.set_laya_manager(manager);
        }
        (true, None) => {
            // The same Arc as `manager` (the ptr-check above matched it):
            // dropping the hold's clone and stopping ours is one stop.
            held.take();
            manager.stop();
        }
        (false, _) => {
            held.take();
        }
    }
}

/// Stop the settings-side hold's sidecar and clear it — the app-exit
/// hook's call (mod.rs's `RunEvent::ExitRequested` handler, right after
/// `state.shutdown()`): a static is never dropped, so without this a
/// settings-installed `laya-serve` that no lead adopted would outlive
/// the app (orphan process, port held). Stopping a childless manager is
/// a `false` no-op, so the call is always safe.
pub(crate) fn stop_held_manager() {
    if let Some(manager) = HELD_SETUP_MANAGER
        .lock()
        .expect("laya hold lock poisoned")
        .take()
    {
        manager.stop();
    }
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The Laya page's status snapshot: the master `enabled` toggle,
/// `installed`, the coarse `status` word, the seven opt-in flags and the
/// `[general.routing]` block. With an open lead this reads the lead's
/// live classifier state (the engine-backed `Setup` shares the brain's
/// status Arc with the runtime's attach and the setup pipeline);
/// without one it reads the config on disk — nothing is running.
#[tauri::command]
pub async fn laya_status(state: State<'_, Mnemo2State>) -> Result<LayaStatusWire, String> {
    let (setup, _) = settings_setup(&state).await?;
    let in_flight = SETUP_IN_FLIGHT.load(Ordering::SeqCst);
    laya_status_read(&setup, in_flight)
}

/// Enable + install the managed Laya runtime: `classifier_setup(true)` —
/// persist the flag, live-apply it, and run the managed download pipeline
/// (uv + venv + `laya[serve]` + the English checkpoint), with
/// [`SetupEvent`] progress on [`LAYA_EVENT_CHANNEL`]. With an open lead
/// the completing pipeline autostarts the sidecar and swaps the lead's
/// classifier slot; without one it installs only (the next lead open
/// attaches). A second call while a setup runs fails fast.
#[tauri::command]
pub async fn laya_setup(state: State<'_, Mnemo2State>, app: AppHandle) -> Result<(), String> {
    let (setup, _) = settings_setup(&state).await?;
    let events = laya_setup_core(&setup, &HELD_SETUP_MANAGER, &SETUP_IN_FLIGHT).await?;
    spawn_laya_forwarder(
        app,
        events,
        Arc::clone(setup.laya_manager()),
        &SETUP_IN_FLIGHT,
    );
    Ok(())
}

/// Disable the classifier: flip the config off through the kernel's own
/// disable leg (which also clears an open lead's classifier slot), stop
/// every manager that may hold a running `laya-serve` child — the lead's
/// attach-time manager and the settings-side hold — and release the
/// in-flight guard. A disable with nothing running is a no-op.
#[tauri::command]
pub async fn laya_disable(state: State<'_, Mnemo2State>) -> Result<(), String> {
    let (setup, runtime) = settings_setup(&state).await?;
    // Every child owner to stop: the lead's attach-time manager, then
    // this module's settings-side hold (taking it also drops the Arc).
    let held = HELD_SETUP_MANAGER
        .lock()
        .expect("laya hold lock poisoned")
        .take();
    let mut managers: Vec<Arc<LayaManager>> = Vec::new();
    if let Some(manager) = runtime.as_ref().and_then(|rt| rt.laya_manager()) {
        managers.push(manager);
    }
    if let Some(manager) = held {
        managers.push(manager);
    }
    laya_disable_core(&setup, &managers, &SETUP_IN_FLIGHT).await
}

/// Save the seven opt-in flags and/or the `[general.routing]` block
/// through the sanctioned settings patch (the same pipeline the Models
/// and Savings pages ride, including the live rewire on an open lead).
/// The master `enabled` flag never rides this patch — send it through
/// `laya_setup`/`laya_disable`.
#[tauri::command]
pub async fn save_laya_settings(
    state: State<'_, Mnemo2State>,
    flags: Option<LayaFlagsPatch>,
    routing: Option<RoutingConfigDto>,
) -> Result<(), String> {
    let (setup, runtime) = settings_setup(&state).await?;
    save_laya_settings_core(&setup, flags, routing).await?;
    settings_refresh(runtime).await
}

// Mnemo2 dispatch routing (plan 2ba2168a, design §4.9/§4.10)

/// The Laya page's Mnemo2 routing snapshot: the global `mnemo2.toml`
/// `[routing]` table, the log summary and the last eval record.
#[derive(Debug, Clone, Serialize)]
pub struct RoutingSettingsWire {
    /// The `[routing]` table (`RoutingSettings::default()` when absent).
    pub settings: dispatch_routing::RoutingSettings,
    /// Counts over `dispatch_routing.jsonl`.
    pub summary: RoutingLogSummary,
    /// The last eval's suggestion (`route_eval.json`), when one ran.
    pub eval: Option<route_eval::EvalRecord>,
}

/// The `[routing]` table, the log summary and the eval record, read from
/// disk.
#[tauri::command]
pub async fn routing_settings_status(
    state: State<'_, Mnemo2State>,
) -> Result<RoutingSettingsWire, String> {
    let (setup, _) = settings_setup(&state).await?;
    Ok(routing_settings_wire(setup.config_dir()))
}

/// Persist the `[routing]` table to `mnemo2.toml` (every other table
/// preserved) and push it into an open team, so the next dispatch sees the
/// new mode without a restart. Returns the fresh snapshot.
#[tauri::command]
pub async fn save_routing_settings(
    state: State<'_, Mnemo2State>,
    settings: dispatch_routing::RoutingSettings,
) -> Result<RoutingSettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let team = match runtime {
        Some(runtime) => runtime.team_handle().await.ok(),
        None => None,
    };
    apply_routing_settings(setup.config_dir(), &settings, team.as_deref())?;
    Ok(routing_settings_wire(setup.config_dir()))
}

/// The command's core: write the `[routing]` table, then reload a live team's
/// copy so the next dispatch acts on it (review R1). The Laya page's Save and
/// the eval's Apply both land here; with no team open there is nothing to
/// reload and the next `TeamRuntime::open` reads the file itself.
pub fn apply_routing_settings(
    config_dir: &Path,
    settings: &dispatch_routing::RoutingSettings,
    team: Option<&TeamRuntime>,
) -> Result<(), String> {
    dispatch_routing::save_routing_settings(config_dir, settings)?;
    if let Some(team) = team {
        team.reload_routing_settings();
    }
    Ok(())
}

/// The dispatch routing log summarised for the page.
#[tauri::command]
pub fn routing_log_summary() -> RoutingLogSummary {
    routing_log_summary_read()
}

/// Assemble the page's routing snapshot from `config_dir`.
pub fn routing_settings_wire(config_dir: &Path) -> RoutingSettingsWire {
    RoutingSettingsWire {
        settings: dispatch_routing::load_routing_settings(config_dir),
        summary: routing_log_summary_read(),
        eval: route_eval::load_eval_record(),
    }
}

/// The dispatch routing log summarised for the Laya page: how many decisions
/// each mode made, how often Laya agreed with the lead, how accurate each was
/// against the derived outcome labels, and how the overrides and nudges went
/// (design §4.8/§4.10).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RoutingLogSummary {
    /// Decisions (`route` rows) per mode, keyed `off`/`shadow`/`suggest`/`auto`.
    pub decisions_by_mode: BTreeMap<String, usize>,
    /// Share of routed workers whose final tier matched the lead's tier —
    /// Laya versus the lead, the agreement a mode change rides on.
    pub agreement_rate: Option<f64>,
    /// Share of Laya-routed workers whose final tier matched the derived
    /// outcome label.
    pub laya_accuracy: Option<f64>,
    /// Share of lead-tier workers whose final tier matched the derived
    /// outcome label.
    pub lead_accuracy: Option<f64>,
    /// Workers with a derived outcome label.
    pub labelled: usize,
    /// Reroutes per source (`lead` / `user`).
    pub overrides_by_source: BTreeMap<String, usize>,
    /// Nudge rows emitted.
    pub nudges: usize,
    /// Nudges a later reroute row recorded as acted on (`nudged: true`).
    pub nudges_acted_on: usize,
    /// The day of the last eval (`route_eval.json`), when one ran.
    pub eval_date: Option<String>,
}

/// The mode's key in [`RoutingLogSummary::decisions_by_mode`] — the log's own
/// spelling.
fn mode_key(mode: dispatch_routing::RoutingMode) -> &'static str {
    match mode {
        dispatch_routing::RoutingMode::Off => "off",
        dispatch_routing::RoutingMode::Shadow => "shadow",
        dispatch_routing::RoutingMode::Suggest => "suggest",
        dispatch_routing::RoutingMode::Auto => "auto",
    }
}

/// Read every row of the dispatch routing log. A missing log is no rows; an
/// unparseable line is skipped (the log is append-only and may be torn).
fn read_dispatch_rows() -> Vec<dispatch_routing::DispatchRoutingRow> {
    let Ok(text) = std::fs::read_to_string(dispatch_routing::dispatch_routing_log_path())
    else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Read and summarise the log, stamping the last eval's date.
pub fn routing_log_summary_read() -> RoutingLogSummary {
    let mut summary = summarize_rows(&read_dispatch_rows());
    summary.eval_date = route_eval::load_eval_record().map(|record| record.date);
    summary
}

/// The counting behind [`routing_log_summary_read`] — pure, so the tests feed
/// it fixture rows.
pub fn summarize_rows(rows: &[dispatch_routing::DispatchRoutingRow]) -> RoutingLogSummary {
    use dispatch_routing::DispatchRoutingRow as Row;

    let mut summary = RoutingLogSummary::default();
    let mut laya_tier: BTreeMap<&str, &str> = BTreeMap::new();
    let mut lead_tier: BTreeMap<&str, &str> = BTreeMap::new();
    let mut routed: BTreeSet<&str> = BTreeSet::new();
    let mut nudged: BTreeSet<&str> = BTreeSet::new();
    let mut acted: BTreeSet<&str> = BTreeSet::new();

    for row in rows {
        match row {
            Row::Route { mode, tier, cleared, lead_tier: lead, worker_id, .. } => {
                *summary
                    .decisions_by_mode
                    .entry(mode_key(*mode).to_string())
                    .or_default() += 1;
                if *cleared {
                    if let Some(tier) = tier {
                        laya_tier.insert(worker_id.as_str(), tier.as_str());
                        routed.insert(worker_id.as_str());
                    }
                }
                if let Some(lead) = lead {
                    lead_tier.insert(worker_id.as_str(), lead.as_str());
                }
            }
            Row::LeadReroute { worker_id, nudged: was_nudged, .. } => {
                *summary
                    .overrides_by_source
                    .entry("lead".to_string())
                    .or_default() += 1;
                if *was_nudged {
                    acted.insert(worker_id.as_str());
                }
            }
            Row::UserReroute { worker_id, nudged: was_nudged, .. } => {
                *summary
                    .overrides_by_source
                    .entry("user".to_string())
                    .or_default() += 1;
                if *was_nudged {
                    acted.insert(worker_id.as_str());
                }
            }
            Row::Nudge { worker_id, .. } => {
                summary.nudges += 1;
                nudged.insert(worker_id.as_str());
            }
            Row::LeadOverride { .. } | Row::NudgeIgnored { .. } | Row::Outcome { .. } => {}
        }
    }
    summary.nudges_acted_on = nudged.intersection(&acted).count();

    let mut agreed = 0;
    let mut compared = 0;
    for worker in &routed {
        if let (Some(laya), Some(lead)) = (laya_tier.get(worker), lead_tier.get(worker)) {
            compared += 1;
            if laya == lead {
                agreed += 1;
            }
        }
    }
    summary.agreement_rate = (compared > 0).then(|| agreed as f64 / compared as f64);

    let mut laya_right = 0;
    let mut laya_labelled = 0;
    let mut lead_right = 0;
    let mut lead_labelled = 0;
    for row in rows {
        let Row::Outcome { worker_id, final_tier, label: Some(label), .. } = row else {
            continue;
        };
        summary.labelled += 1;
        let worker = worker_id.as_str();
        if routed.contains(worker) {
            laya_labelled += 1;
            if final_tier == label {
                laya_right += 1;
            }
        } else if lead_tier.contains_key(worker) {
            lead_labelled += 1;
            if final_tier == label {
                lead_right += 1;
            }
        }
    }
    summary.laya_accuracy =
        (laya_labelled > 0).then(|| laya_right as f64 / laya_labelled as f64);
    summary.lead_accuracy =
        (lead_labelled > 0).then(|| lead_right as f64 / lead_labelled as f64);
    summary
}

/// The Laya page's eval result: every label-count run (four labels — the
/// question dispatch asks — then three and two) and the record written to
/// `route_eval.json`.
#[derive(Debug, Clone, Serialize)]
pub struct RoutingEvalWire {
    /// The four-label run — the question dispatch actually asks (design
    /// §5.1).
    pub four: route_eval::EvalReport,
    /// The three-label run.
    pub three: route_eval::EvalReport,
    /// The two-label run.
    pub two: route_eval::EvalReport,
    /// The record written to `route_eval.json`.
    pub record: route_eval::EvalRecord,
}

/// Run the routing eval against the open lead's attached Laya: the hand set
/// under four labels — the question dispatch actually asks — and again under
/// three and two, one prompt at a time; writes `route_eval.json` and returns
/// all three reports. It never applies a threshold — the page's Apply button
/// saves the suggestion through [`save_routing_settings`].
#[tauri::command]
pub async fn run_routing_eval(
    state: State<'_, Mnemo2State>,
) -> Result<RoutingEvalWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let Some(runtime) = runtime else {
        return Err("open a lead first: the eval asks the lead's attached Laya".to_string());
    };
    let classifier = attached_classifier(&setup)?;
    let route_knn = runtime
        .team_handle()
        .await
        .ok()
        .and_then(|team| team.route_knn());
    let routing = dispatch_routing::load_routing_settings(setup.config_dir());
    let (four, three, two) = routing_eval_reports(
        classifier.as_ref(),
        route_knn.as_deref(),
        routing.knn_min_rows,
        routing.knn_min_share,
    )
    .await?;
    let record = route_eval::eval_record(&four, &three, &two);
    route_eval::save_eval_record(&record)?;
    Ok(RoutingEvalWire { four, three, two, record })
}

/// The lead engine's live classifier, or a clear error naming what is
/// missing.
fn attached_classifier(setup: &Setup) -> Result<Arc<dyn Classifier>, String> {
    let missing = || "Laya is not attached: enable it on this page first".to_string();
    let slot = setup.classifier_slot().ok_or_else(missing)?;
    let classifier = slot
        .read()
        .expect("classifier slot lock poisoned")
        .clone();
    classifier.ok_or_else(missing)
}

/// The three corpus passes, sequentially, with no file writes — the command
/// adds the record; the tests drive this with a stub classifier and never
/// touch the network.
pub async fn routing_eval_reports(
    classifier: &dyn Classifier,
    knn: Option<&RouteKnn>,
    knn_min_rows: usize,
    knn_min_share: f64,
) -> Result<
    (
        route_eval::EvalReport,
        route_eval::EvalReport,
        route_eval::EvalReport,
    ),
    String,
> {
    let cases = route_eval::parse_hand_set(route_eval::HAND_SET_JSONL)?;
    // The four-label run is the question dispatch actually asks (design §5.1:
    // every pickable tier is offered); the three- and two-label runs cover the
    // reduced offered sets — the interim legacy-tier shape, and the extremes
    // with both middles folded into `hard`.
    let four_offered = OfferedLabels {
        trivial: true,
        routine: true,
        involved: true,
        hard: true,
    };
    let three_offered = OfferedLabels { trivial: true, routine: true, involved: false, hard: true };
    let two_offered = OfferedLabels { trivial: true, routine: false, involved: false, hard: true };
    let four_scores = route_eval::classify_corpus(classifier, four_offered, &cases).await;
    let three_scores = route_eval::classify_corpus(classifier, three_offered, &cases).await;
    let two_scores = route_eval::classify_corpus(classifier, two_offered, &cases).await;
    let four_knn = match knn {
        Some(knn) => Some(
            route_eval::evaluate_knn(
                &cases,
                &four_scores,
                four_offered,
                knn,
                knn_min_rows,
                knn_min_share,
            )
            .await,
        ),
        None => None,
    };
    let three_knn = match knn {
        Some(knn) => Some(
            route_eval::evaluate_knn(
                &cases,
                &three_scores,
                three_offered,
                knn,
                knn_min_rows,
                knn_min_share,
            )
            .await,
        ),
        None => None,
    };
    let two_knn = match knn {
        Some(knn) => Some(
            route_eval::evaluate_knn(
                &cases,
                &two_scores,
                two_offered,
                knn,
                knn_min_rows,
                knn_min_share,
            )
            .await,
        ),
        None => None,
    };
    Ok((
        route_eval::report_with_knn(&four_scores, four_offered, four_knn),
        route_eval::report_with_knn(&three_scores, three_offered, three_knn),
        route_eval::report_with_knn(&two_scores, two_offered, two_knn),
    ))
}

// ── Tests ───────────────────────────────────────────────────────────────────
//
// The network legs (uv, venv, the checkpoint download) are NEVER exercised
// here — the discipline the kernel seam's and the runtime's tests follow
// (`doctor_installed_laya` plants a fake, and the guard leg of the real
// `classifier_setup` errors before any pipeline spawns). The save tests
// assert that a patch keeps every section no Laya save touches — the
// shared pipeline rewrites config.toml fully from the schema, so
// surviving keys prove only the named section was patched (comments and
// unknown keys are dropped by that same documented design).

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::config::general::SafetyMode;
    use crate::mnemo2::runtime::LeadRuntimeOptions;

    /// A temp config dir with a rich fixture: a `config.toml` whose
    /// Laya-owned sections (`[general.laya]`, `[general.routing]`) sit
    /// beside ones no Laya save touches, and an `endpoints.toml` with an
    /// endpoint, a pricing row and tiers. Returns the TempDir (kept
    /// alive) and a `Setup` on it.
    fn fixture() -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().expect("config dir");
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[general]
safety = "auto-approve-project"

[general.laya]
enabled = true
auto_type_memories = true

[general.routing]
cheap = { endpoint = "ep1", model = "tiny" }
capable = { endpoint = "ep1", model = "big" }
lane_high = { endpoint = "ep1", model = "top" }
threshold = 0.75
enforce = true

[models]
planning = { endpoint = "ep1", model = "plan-model" }

[context]
summarize_at_fill_rate = 0.5
"#,
        )
        .expect("write config.toml");
        std::fs::write(
            dir.path().join("endpoints.toml"),
            r#"[[endpoint]]
name = "ep1"
kind = "openai"
base_url = "http://127.0.0.1:9/v1"
models = ["plan-model", "exec-model", "complete-model", "sub-model", "vision-x", "other-model", "tiny", "big", "top"]

[[pricing]]
model = "plan-model"
input_per_1m = 1.5
output_per_1m = 2.5
cached_per_1m = 0.1

[tiers]
difficult = ["ep1/plan-model"]
average = ["ep1/exec-model"]
simple = ["ep1/exec-model"]
"#,
        )
        .expect("write endpoints.toml");
        let setup = Setup::open(Some(dir.path().to_path_buf())).expect("setup");
        (dir, setup)
    }

    /// Doctor a PRETEND-installed managed runtime under `<config_dir>/laya`
    /// — the same fake the kernel seam's and the runtime's tests plant:
    /// exactly the two files `is_checkpoint_installed` probes (the marker
    /// and the venv's `laya-serve`). The fake binary is deliberately NOT a
    /// valid executable image: no real sidecar, no network.
    fn doctor_installed_laya(dir: &std::path::Path) {
        let base = dir.join("laya");
        std::fs::create_dir_all(base.join("markers")).expect("markers dir");
        std::fs::write(
            base.join("markers").join(english_checkpoint().id),
            b"installed",
        )
        .expect("marker file");
        let serve = mnemo::laya::venv_bin(&base.join("venv"), std::env::consts::OS, "laya-serve");
        std::fs::create_dir_all(serve.parent().expect("venv bin dir")).expect("venv bin dir");
        std::fs::write(&serve, b"not a real executable").expect("fake laya-serve");
    }

    #[test]
    fn status_reads_the_fixture_and_names_not_installed() {
        let (dir, setup) = fixture();
        let wire = laya_status_read(&setup, false).expect("read");
        assert!(wire.enabled);
        assert!(!wire.installed, "no checkpoint in the fixture");
        assert_eq!(wire.status, "not_installed");
        // The seven flags round out of the config.
        assert!(wire.flags.auto_type_memories);
        assert!(!wire.flags.failure_triage);
        assert!(!wire.flags.failure_triage_knn);
        assert!(!wire.flags.auto_finetune);
        assert!(!wire.flags.steer_tool_choice);
        assert!(!wire.flags.routing);
        assert!(!wire.flags.reflex);
        // The routing block rides in its compact form.
        assert_eq!(wire.routing.cheap.as_deref(), Some("ep1/exec-model"));
        assert_eq!(wire.routing.capable.as_deref(), Some("ep1/plan-model"));
        assert_eq!(wire.routing.lane_medium, None);
        assert_eq!(wire.routing.lane_high.as_deref(), Some("ep1/plan-model"));
        assert_eq!(wire.routing.escalate, None);
        assert!((wire.routing.threshold - 0.75).abs() < 1e-9);
        assert!(wire.routing.enforce);

        // Installed + the fresh Disabled Arc a lead-less Setup reads: the
        // master flag is on with nothing running — "disabled", never a
        // phantom "ready" (the next lead open attaches).
        doctor_installed_laya(dir.path());
        let wire = laya_status_read(&setup, false).expect("read");
        assert!(wire.installed);
        assert_eq!(wire.status, "disabled");
    }

    #[test]
    fn status_maps_every_live_state() {
        let enabled = LayaConfig {
            enabled: true,
            ..Default::default()
        };
        let disabled = LayaConfig::default();
        // Every pipeline/engine state collapses to its page word.
        assert_eq!(
            laya_status_word(&enabled, false, &ClassifierStatus::Installing, false),
            "installing"
        );
        assert_eq!(
            laya_status_word(
                &enabled,
                false,
                &ClassifierStatus::Downloading { label: "uv".into(), progress: 0.5 },
                false
            ),
            "installing"
        );
        assert_eq!(
            laya_status_word(&enabled, false, &ClassifierStatus::Starting, false),
            "starting"
        );
        assert_eq!(laya_status_word(&enabled, false, &ClassifierStatus::Ready, false), "ready");
        assert_eq!(
            laya_status_word(&enabled, false, &ClassifierStatus::FineTuning { label: "x".into() }, false),
            "ready"
        );
        assert_eq!(laya_status_word(&enabled, false, &ClassifierStatus::Failed, false), "failed");
        // Disabled splits by the master flag + install state.
        assert_eq!(
            laya_status_word(&enabled, false, &ClassifierStatus::Disabled, false),
            "not_installed"
        );
        assert_eq!(
            laya_status_word(&enabled, true, &ClassifierStatus::Disabled, false),
            "disabled"
        );
        assert_eq!(
            laya_status_word(&disabled, false, &ClassifierStatus::Disabled, false),
            "disabled"
        );
        // The settings-side guard covers a pipeline the fresh Arc cannot
        // see (no lead open) — and never overrides a live state.
        assert_eq!(
            laya_status_word(&enabled, true, &ClassifierStatus::Disabled, true),
            "installing"
        );
        assert_eq!(
            laya_status_word(&enabled, false, &ClassifierStatus::Ready, true),
            "ready"
        );
    }

    #[test]
    fn status_reports_a_settings_side_setup_in_flight() {
        let (_dir, setup) = fixture();
        // No lead is open, so the pipeline's moves are invisible to the
        // fresh status Arc — the guard is what reports the install.
        let wire = laya_status_read(&setup, true).expect("read");
        assert_eq!(wire.status, "installing");
    }

    #[tokio::test]
    async fn save_rejects_enabled_and_round_trips_flags_and_routing() {
        let (dir, setup) = fixture();
        // The master flag never rides the save — and the rejection changes
        // nothing on disk.
        let err = save_laya_settings_core(
            &setup,
            Some(LayaFlagsPatch {
                enabled: Some(true),
                ..Default::default()
            }),
            None,
        )
        .await
        .expect_err("enabled is rejected");
        assert!(err.contains("laya_setup"), "the error names the commands: {err}");
        let config = Config::load(dir.path()).expect("reload");
        assert!(config.general.general.laya.enabled, "the rejection wrote nothing");

        // All seven flags + the routing block through the sanctioned patch.
        let flags = LayaFlagsPatch {
            enabled: None,
            auto_type_memories: Some(false),
            failure_triage: Some(true),
            failure_triage_knn: Some(true),
            auto_finetune: Some(true),
            steer_tool_choice: Some(true),
            routing: Some(true),
            reflex: Some(true),
        };
        let routing = RoutingConfigDto {
            threshold: Some(0.9),
            first_prompt_min_confidence: Some(Some(0.55)),
            enforce: Some(false),
        };
        save_laya_settings_core(&setup, Some(flags), Some(routing))
            .await
            .expect("save");

        let config = Config::load(dir.path()).expect("reload");
        let laya = &config.general.general.laya;
        assert!(laya.enabled, "the master flag keeps its stored value — the save never touches it");
        assert!(!laya.auto_type_memories, "a false flips it off");
        assert!(laya.failure_triage);
        assert!(laya.failure_triage_knn);
        assert!(laya.auto_finetune);
        assert!(laya.steer_tool_choice);
        assert!(laya.routing);
        assert!(laya.reflex);
        let routing = &config.general.general.routing;
        assert_eq!(routing.first_prompt_min_confidence, Some(0.55));
        assert_eq!(
            config.tiers.get("trivial").unwrap().models,
            vec!["ep1/exec-model", "ep1/tiny"]
        );
        assert_eq!(config.tiers.get("routine").unwrap().models, vec!["ep1/exec-model"]);
        assert_eq!(
            config.tiers.get("high").unwrap().models,
            vec!["ep1/plan-model", "ep1/big", "ep1/top"]
        );
        assert!((routing.threshold - 0.9).abs() < 1e-9);
        assert!(!routing.enforce);

        // Keep semantics: a flags-only save leaves the routing block alone.
        let flags = LayaFlagsPatch {
            reflex: Some(false),
            ..Default::default()
        };
        save_laya_settings_core(&setup, Some(flags), None)
            .await
            .expect("keep");
        let config = Config::load(dir.path()).expect("reload");
        assert!(!config.general.general.laya.reflex);
        assert_eq!(
            config.tiers.get("trivial").unwrap().models,
            vec!["ep1/exec-model", "ep1/tiny"]
        );

        // Unrelated sections survive the schema rewrite...
        assert_eq!(config.general.general.safety, SafetyMode::AutoApproveProject);
        assert_eq!(
            config
                .general
                .models
                .planning
                .as_ref()
                .expect("planning kept")
                .model,
            "plan-model"
        );
        assert_eq!(config.general.context.summarize_at_fill_rate, 0.5);
        // ...and so does the [tiers] table save_all carries through.
        let tiers = mnemo::config::endpoints::load_tier_ladder(
            &dir.path().join("endpoints.toml"),
            None,
        )
        .unwrap();
        assert_eq!(tiers.get("high").unwrap().models[0], "ep1/plan-model");
    }

    #[tokio::test]
    async fn disable_flips_the_config_off_and_stops_nothing_extra() {
        let (dir, setup) = fixture();
        let in_flight = AtomicBool::new(true);
        // No managers held — a disable with nothing running is a no-op.
        laya_disable_core(&setup, &[], &in_flight)
            .await
            .expect("disable");
        let config = Config::load(dir.path()).expect("reload");
        assert!(!config.general.general.laya.enabled, "the config flips off");
        assert!(!in_flight.load(Ordering::SeqCst), "the guard releases");
        // A childless manager in the stop list stays a safe no-op.
        let manager = Arc::clone(setup.laya_manager());
        laya_disable_core(&setup, std::slice::from_ref(&manager), &in_flight)
            .await
            .expect("disable with a manager");
        assert!(!manager.stop(), "nothing was running");
    }

    #[tokio::test]
    async fn setup_fails_fast_while_one_is_in_flight() {
        let (_dir, setup) = fixture();
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(None);
        let in_flight = AtomicBool::new(true);
        let err = laya_setup_core(&setup, &hold, &in_flight)
            .await
            .expect_err("double-click guard");
        assert_eq!(err, "a Laya setup is already running");
        assert!(hold.lock().expect("hold lock").is_none(), "the hold is untouched");
        assert!(in_flight.load(Ordering::SeqCst), "the guard stays claimed");
    }

    #[tokio::test]
    async fn setup_routes_through_the_real_kernel_leg_without_a_sidecar() {
        let (dir, setup) = fixture();
        // Claim the kernel's own per-manager setup guard first: the real
        // `classifier_setup(true)` leg persists + live-applies, THEN hits
        // the guard and errors before spawning any pipeline — the real
        // leg, no network, no sidecar.
        assert!(setup.laya_manager().begin_setup(), "claim the kernel guard");
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(None);
        let in_flight = AtomicBool::new(false);
        let err = laya_setup_core(&setup, &hold, &in_flight)
            .await
            .expect_err("the kernel guard fires");
        assert!(err.contains("already running"), "{err}");
        // The real enable leg persisted the flag before the guard fired.
        let config = Config::load(dir.path()).expect("reload");
        assert!(config.general.general.laya.enabled);
        // Our guard released, and the hold is the Setup's OWN manager —
        // the instance the (suppressed) pipeline would have run on: never
        // a second manager.
        assert!(!in_flight.load(Ordering::SeqCst));
        let held = hold.lock().expect("hold lock").as_ref().expect("hold set").clone();
        assert!(Arc::ptr_eq(&held, setup.laya_manager()));
    }

    /// The lead-open fixture the runtime's tests use (its private
    /// `write_config`): a default model resolvable from a keyless local
    /// endpoint — `Engine::start`'s only gate for a caller without an
    /// injected provider — with `[general.laya]` deliberately absent so
    /// nothing attaches at engine start. The only manager in play is the
    /// settings-side one.
    fn write_lead_config(dir: &std::path::Path) {
        std::fs::write(
            dir.join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
        )
        .expect("config.toml");
        std::fs::write(
            dir.join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\"]\n",
        )
        .expect("endpoints.toml");
    }

    #[tokio::test]
    async fn a_settled_manager_handed_to_the_runtime_is_stopped_by_shutdown() {
        // Review finding heldManagerOrphan: the settings-side hold is a
        // static (never dropped), so the sidecar a `laya_setup` pipeline
        // spawned would outlive the session AND the app. The pipeline-end
        // settlement hands a SERVING manager to the open lead's runtime —
        // here simulated the way the real autostart drives it (the
        // manager's status Arc reads Ready at the pipeline task's end).
        // No real sidecar ever runs: the observable is the one the
        // runtime's own attach test uses — the session holds the very
        // Arc (ptr_eq) after the hand-off, and `shutdown` leaves it
        // holding nothing (its explicit `stop` is the primary path; the
        // manager's `Drop` is only the safety net).
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_lead_config(config.path());
        let runtime = LeadRuntime::open(LeadRuntimeOptions {
            workspace_root: root.path().to_path_buf(),
            config_dir: Some(config.path().to_path_buf()),
            state_dir: state.path().to_path_buf(),
            model: None,
        })
        .await
        .expect("the lead opens without Laya — no manager of its own");
        assert!(
            runtime.laya_manager().is_none(),
            "nothing attached at engine start"
        );

        // The settings-side hold: the per-call Setup's own manager, its
        // status reading Ready the way the autostart's spawn + readiness
        // probe + live-slot swap leaves it.
        let setup = Setup::open(Some(config.path().to_path_buf())).expect("setup");
        let manager = Arc::clone(setup.laya_manager());
        *manager
            .status_arc()
            .write()
            .expect("classifier status lock poisoned") = ClassifierStatus::Ready;
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(Some(Arc::clone(&manager)));

        settle_held_manager_core(&hold, Some(&runtime), Arc::clone(&manager));
        let adopted = runtime
            .laya_manager()
            .expect("the session adopted the settings-side manager");
        assert!(Arc::ptr_eq(&adopted, &manager));
        assert!(
            hold.lock().expect("hold lock").is_none(),
            "the hold is cleared by the hand-off"
        );

        runtime.shutdown().await;
        assert!(
            runtime.laya_manager().is_none(),
            "shutdown stopped the settings-installed manager like the attach-time one"
        );
    }

    #[test]
    fn settling_a_live_orphan_without_a_lead_stops_and_clears_the_hold() {
        // No lead open at the pipeline's end: the sidecar's slot went
        // with the closed project — stopping it beats leaking it to app
        // exit. (Childless here — the fixture never runs one; the
        // observable is the cleared hold, the real child dies in the
        // same stop call.)
        let (_dir, setup) = fixture();
        let manager = Arc::clone(setup.laya_manager());
        *manager
            .status_arc()
            .write()
            .expect("classifier status lock poisoned") = ClassifierStatus::Ready;
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(Some(Arc::clone(&manager)));
        settle_held_manager_core(&hold, None, Arc::clone(&manager));
        assert!(
            hold.lock().expect("hold lock").is_none(),
            "an orphan sidecar never outlives its consumer"
        );
    }

    #[test]
    fn settling_a_failed_pipeline_only_clears_the_hold() {
        // A failed or disabled pipeline leaves the manager childless
        // (the child was scoped-torn-down or never spawned): the
        // session's own manager, if any, keeps serving the untouched
        // slot — nothing is stopped, the hold just clears.
        let (_dir, setup) = fixture();
        let manager = Arc::clone(setup.laya_manager());
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(Some(Arc::clone(&manager)));
        settle_held_manager_core(&hold, None, Arc::clone(&manager));
        assert!(hold.lock().expect("hold lock").is_none());
    }

    #[test]
    fn settling_never_clobbers_a_hold_that_moved_on() {
        // A mid-flight `laya_disable` takes the hold, a newer
        // `laya_setup` replaces it: the old pipeline's settlement must
        // leave it alone.
        let (_dir_a, setup_a) = fixture();
        let old_manager = Arc::clone(setup_a.laya_manager());
        let (_dir_b, setup_b) = fixture();
        let replacement = Arc::clone(setup_b.laya_manager());
        let hold: Mutex<Option<Arc<LayaManager>>> = Mutex::new(Some(Arc::clone(&replacement)));
        settle_held_manager_core(&hold, None, Arc::clone(&old_manager));
        let still = hold.lock().expect("hold lock").clone();
        assert!(
            Arc::ptr_eq(still.as_ref().expect("hold kept"), &replacement),
            "the newer hold survives the old pipeline's settlement"
        );
    }

    fn route_row(
        worker: &str,
        mode: dispatch_routing::RoutingMode,
        tier: Option<&str>,
        cleared: bool,
        lead: Option<&str>,
    ) -> dispatch_routing::DispatchRoutingRow {
        dispatch_routing::DispatchRoutingRow::Route {
            ts: 1,
            worker_id: worker.to_string(),
            plan_id: "p".to_string(),
            step: 1,
            mode,
            label: String::new(),
            tier: tier.map(str::to_string),
            confidence: 0.6,
            probabilities: BTreeMap::new(),
            threshold: 0.475,
            cleared,
            lead_tier: lead.map(str::to_string),
            ran_tier: tier.map(str::to_string),
            ran_model: None,
            text_sha256: String::new(),
            text: "text".to_string(),
            skip: None,
            source: Some("laya".to_string()),
            knn_label: None,
            knn_share: None,
            knn_k: None,
        }
    }

    fn outcome_row(
        worker: &str,
        final_tier: &str,
        label: Option<&str>,
    ) -> dispatch_routing::DispatchRoutingRow {
        dispatch_routing::DispatchRoutingRow::Outcome {
            ts: 2,
            worker_id: worker.to_string(),
            plan_id: "p".to_string(),
            step: 1,
            status: "merged".to_string(),
            final_tier: final_tier.to_string(),
            models_used: vec![],
            review_rounds: 1,
            fix_rounds: 0,
            findings: 0,
            findings_high: 0,
            audits: 0,
            cost_micros: 0,
            label: label.map(str::to_string),
        }
    }

    fn reroute_row(
        worker: &str,
        lead: bool,
        nudged: bool,
    ) -> dispatch_routing::DispatchRoutingRow {
        if lead {
            dispatch_routing::DispatchRoutingRow::LeadReroute {
                ts: 3,
                worker_id: worker.to_string(),
                plan_id: "p".to_string(),
                step: 1,
                from_tier: "routine".to_string(),
                to_tier: "high".to_string(),
                from_model: None,
                to_model: None,
                status: "running".to_string(),
                review_rounds: 0,
                fix_rounds: 0,
                nudged,
            }
        } else {
            dispatch_routing::DispatchRoutingRow::UserReroute {
                ts: 3,
                worker_id: worker.to_string(),
                plan_id: "p".to_string(),
                step: 1,
                from_tier: "routine".to_string(),
                to_tier: "trivial".to_string(),
                from_model: None,
                to_model: None,
                status: "running".to_string(),
                review_rounds: 0,
                fix_rounds: 0,
                nudged,
            }
        }
    }

    fn nudge_row(worker: &str) -> dispatch_routing::DispatchRoutingRow {
        dispatch_routing::DispatchRoutingRow::Nudge {
            ts: 4,
            worker_id: worker.to_string(),
            plan_id: "p".to_string(),
            step: 1,
            reason: "strikes".to_string(),
            from_tier: "routine".to_string(),
            to_tier: "high".to_string(),
            detail: "stuck".to_string(),
        }
    }

    #[test]
    fn summary_counts_agreement_and_accuracy() {
        use dispatch_routing::RoutingMode;

        let rows = vec![
            // Two shadow decisions with a lead tier, one of each kind.
            route_row("w1", RoutingMode::Shadow, Some("routine"), true, Some("routine")),
            route_row("w2", RoutingMode::Shadow, Some("trivial"), true, Some("routine")),
            // An off-mode decision: Laya cleared nothing, the lead ran.
            route_row("w3", RoutingMode::Off, None, false, Some("high")),
            // A suggest decision with no lead tier: routed, not compared.
            route_row("w4", RoutingMode::Suggest, Some("high"), true, None),
            reroute_row("w1", true, true),
            reroute_row("w2", false, false),
            nudge_row("w1"),
            nudge_row("w2"),
            outcome_row("w1", "routine", Some("routine")),
            outcome_row("w2", "routine", Some("high")),
            outcome_row("w3", "high", Some("high")),
            outcome_row("w4", "trivial", Some("routine")),
        ];

        let summary = summarize_rows(&rows);
        assert_eq!(
            summary.decisions_by_mode.get("shadow"),
            Some(&2),
            "{:?}",
            summary.decisions_by_mode
        );
        assert_eq!(summary.decisions_by_mode.get("off"), Some(&1));
        assert_eq!(summary.decisions_by_mode.get("suggest"), Some(&1));
        // Laya versus the lead: w1 agrees, w2 does not, w4 has no lead tier.
        assert_eq!(summary.agreement_rate, Some(0.5));
        // Accuracy against the derived labels, per routing source.
        assert!((summary.laya_accuracy.expect("laya accuracy") - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(summary.lead_accuracy, Some(1.0));
        assert_eq!(summary.labelled, 4);
        assert_eq!(summary.overrides_by_source.get("lead"), Some(&1));
        assert_eq!(summary.overrides_by_source.get("user"), Some(&1));
        assert_eq!(summary.nudges, 2);
        assert_eq!(summary.nudges_acted_on, 1);
        assert_eq!(summary.eval_date, None);
    }

    /// A classifier that answers each hand-set text with the label the
    /// fixture records for it (`routine` for the vague ones), at confidence
    /// 0.9 — enough to drive both eval passes with no network.
    struct StubClassifier {
        expected: BTreeMap<String, Option<String>>,
    }

    #[async_trait::async_trait]
    impl Classifier for StubClassifier {
        async fn classify(
            &self,
            state: &str,
            _question: &mnemo::memory::classifier::Question,
        ) -> Option<mnemo::memory::classifier::Answer> {
            let label = self
                .expected
                .get(state)
                .cloned()
                .flatten()
                .unwrap_or_else(|| "routine".to_string());
            Some(mnemo::memory::classifier::Answer::Choice {
                label,
                confidence: 0.9,
                probabilities: BTreeMap::new(),
            })
        }
    }

    #[tokio::test]
    async fn routing_eval_reports_with_a_stub_classifier() {
        let cases = route_eval::parse_hand_set(route_eval::HAND_SET_JSONL).expect("hand set");
        let expected: BTreeMap<String, Option<String>> = cases
            .iter()
            .map(|case| (case.text.clone(), case.expected.clone()))
            .collect();
        let stub = StubClassifier { expected };

        let (four, three, two) = routing_eval_reports(&stub, None, 30, 0.80)
            .await
            .expect("reports");
        assert_eq!(four.labels, 4);
        assert_eq!(three.labels, 3);
        assert_eq!(two.labels, 2);
        assert_eq!(four.table.len(), 23);
        assert_eq!(three.table.len(), 23);
        assert_eq!(two.table.len(), 23);
        // Every case clears the gate at 0.9: the four-label run — the question
        // dispatch asks — gets all 56 labelled cases right, and the three
        // vague ones are false routes and so are the misses.
        assert_eq!(four.table[0].routed, 59);
        assert!((four.table[0].accuracy - 56.0 / 59.0).abs() < 1e-12);
        assert_eq!(four.misses.len(), 3);
        assert!((four.suggested.expect("a suggested gate") - 0.25).abs() < 1e-9);
        // The stub answers the recorded label, and an un-offered label folds
        // the same way on the answer and the expectation side — so the
        // reduced offered sets are also right on all 56 labelled cases. The
        // runs differ in their labels count and their own default gate.
        assert!((three.table[0].accuracy - 56.0 / 59.0).abs() < 1e-12);
        assert_eq!(three.misses.len(), 3);
        assert!((two.table[0].accuracy - 56.0 / 59.0).abs() < 1e-12);
        assert!(
            (four.default_gate - mnemo::agent::route_question::DEFAULT_MIN_CONFIDENCE_4).abs() < 1e-9
        );
        assert!(
            (three.default_gate - mnemo::agent::route_question::DEFAULT_MIN_CONFIDENCE_3).abs()
                < 1e-9
        );
        assert!(
            (two.default_gate - mnemo::agent::route_question::DEFAULT_MIN_CONFIDENCE_2).abs() < 1e-9
        );
        assert!(three.suggested.is_some());
        assert!(two.suggested.is_some());
    }
}

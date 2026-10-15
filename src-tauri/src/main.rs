// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo Tauri app — the native window shell.
//!
//! Wires the brain (config, project, provider, tools, workflow, runtime, agent
//! loop) to the Tauri IPC bridge and opens the webview frontend.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(warnings)]

use std::sync::{Arc, RwLock};

use mnemo::config::{global_config_dir, Config, SafetyMode};
use mnemo::app::project::Project;
use mnemo::provider::trace::LlmRequestLog;
use mnemo::AgentManager;
use mnemo::tool::agent::sandbox::Sandbox;
use tauri::{Emitter, Manager, PhysicalPosition, WebviewBuilder, WebviewUrl, WindowBuilder};

mod console;
mod instance_marker;
mod ipc;
mod startup;
mod watchdog;
mod webview_udf;

use ipc::IpcState;
use ipc::PendingApprovals;
use ipc::PendingQuestions;
pub(crate) use mnemo::brain::{build_brain, Brain, BrainOutcome};
use mnemo::notifier::{HostEvent, HostNotifier};

/// Read just the `enable_browser_inspection` flag from `config.toml`, without
/// loading the full [`Config`] (which reads four files). Used at the very top
/// of [`main`], before WebView2 is created, to decide whether to expose the
/// CDP debug port in a release build. Returns `false` on any read/parse error
/// (fail closed — never expose the unauthenticated port unless the opt-in is
/// clearly present). Debug builds ignore this and always expose CDP.
#[cfg(windows)]
fn browser_inspection_enabled() -> bool {
    let path = global_config_dir().join("config.toml");
    mnemo::config::GeneralConfig::load_or_default(&path)
        .map(|c| c.general.enable_browser_inspection)
        .unwrap_or(false)
}

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
            HostEvent::EmbedderStatus(status) => {
                // 3b.3 (§5.1): the embedder://status channel —
                // payload-identical to the app's own emissions from the
                // IPC load paths (the settings UI's listener). Today
                // only the headless Engine's startup path notifies this
                // variant; the app's load paths emit directly.
                let _ = self.0.emit("embedder://status", &status);
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
    let laya = Arc::new(ipc::laya::LayaManager::new(classifier_status.clone()));

    // Laya is managed-only (the external-endpoint mode is gone): the app owns
    // the runtime. When enabled + the English checkpoint is installed, the
    // classifier client points at the loopback sidecar the startup hook
    // spawns right after this (status Starting until the probe answers).
    // Enabled but not installed ⇒ a hint is logged and no client exists (the
    // Settings → Classifier setup completes it; `laya_setup` then starts the
    // server and swaps the client in).
    let mut pending_laya_start = None;
    let classifier = if laya_cfg.enabled {
        let checkpoint = ipc::laya::english_checkpoint();
        if laya.is_checkpoint_installed(checkpoint.id) {
            match ipc::laya::LayaManager::alloc_free_port() {
                Some(port) => {
                    pending_laya_start = Some((checkpoint.id.to_string(), port));
                    *classifier_status.write().expect("classifier status lock poisoned") =
                        mnemo::memory::classifier::ClassifierStatus::Starting;
                    ipc::laya::build_managed_classifier(port, classifier_status.clone())
                }
                None => {
                    eprintln!(
                        "warning: no free loopback port for the managed laya-serve; \
                         the classifier stays disabled"
                    );
                    None
                }
            }
        } else {
            eprintln!(
                "info: [general.laya] is enabled but the '{}' checkpoint is not \
                 downloaded — run the setup in Settings → Classifier",
                checkpoint.id
            );
            None
        }
    } else {
        None
    };
    *brain
        .classifier_slot
        .write()
        .expect("classifier slot lock poisoned") = classifier;
    (laya, pending_laya_start)
}

/// Install the app-layer child-webview ensurer on a shared [`BrowserManager`]
/// (plan 5ae26d22): the agent's `browser_navigate` invokes this hook when no
/// child CDP target exists, creating the child webview (hidden) + emitting
/// `browser://reveal` so the frontend surfaces the Browser tab. The closure
/// clones the `AppHandle` per call and resolves `IpcState` lazily at ensure
/// time — the state is managed during setup, before any agent can run.
fn attach_child_ensurer(browser: &mnemo::browser::BrowserManager, app: tauri::AppHandle) {
    browser.set_child_ensurer(std::sync::Arc::new(move |url: String| {
        let app = app.clone();
        Box::pin(async move {
            let Some(state) = app.try_state::<IpcState>() else {
                return Err("app state not yet available".into());
            };
            ipc::browser_webview::ensure_for_agent_impl(&app, state.inner(), &url)
                .await
                .map_err(|e| e.message)
        })
    }));
}

/// Adopt the login shell's `$PATH` (Unix only).
///
/// macOS GUI apps launched from Finder/Dock do NOT inherit the user's shell
/// environment (no `.zshrc`/`.zprofile`), so `PATH` stays the bare system
/// default (`/usr/bin:/bin:/usr/sbin:/sbin`) — the agent's `sh -c` tool would
/// then fail to find `cargo`/`npm`/`node`/Homebrew `git` inside a shipped
/// `.app`. Run the user's login shell once at startup (before any child
/// process spawns) and adopt its resolved `PATH`. Best-effort: any failure
/// (no `$SHELL`, spawn error, non-adoptable output) keeps the inherited PATH.
/// Accepted trade-off: a dotfile that echoes to stdout pollutes the captured
/// value (the rc output shares stdout with the `printf`) — the
/// control-character check in [`is_adoptable_path`] rejects the obvious
/// cases; a same-line echo cannot be distinguished. In-tree equivalent of
/// the fix-path-env approach — implemented here dependency-free so the
/// macOS CI build has no extra crate-name risk.
#[cfg(unix)]
fn inherit_shell_path() {
    if let Some(shell) = std::env::var_os("SHELL") {
        if let Ok(output) = std::process::Command::new(&shell)
            .args(["-l", "-c", "printf %s \"$PATH\""])
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if is_adoptable_path(&path) {
                    std::env::set_var("PATH", path);
                } else {
                    eprintln!(
                        "mnemo: keeping the inherited PATH — the login shell's \
                         PATH was empty or contained control characters"
                    );
                }
            }
        }
    }
}

/// Whether a captured login-shell PATH value is safe to adopt.
///
/// Delegates to [`mnemo::shell_path::is_adoptable_path`], which lives in
/// platform-neutral library code so every platform's tests guard it (the
/// predicate was once `#[cfg(unix)]`-only here, which hid a whitespace-only
/// regression from the Windows leg — the macOS CI failure of 2026-09-21).
#[cfg(unix)]
fn is_adoptable_path(value: &str) -> bool {
    mnemo::shell_path::is_adoptable_path(value)
}

/// Non-Unix: nothing to do — Windows GUI apps inherit the user PATH.
#[cfg(not(unix))]
fn inherit_shell_path() {}

fn main() {
    // Console mode — run the brain as a terminal REPL, never touching
    // WebView2/Tauri. Checked FIRST, before anything else in main(): a
    // release build is a GUI-subsystem binary, so a terminal launch starts
    // with NULL std handles — console::attach_console() must AllocConsole
    // a window of its own and rewire stdin/stdout/stderr BEFORE the first
    // print anywhere in the process (Rust caches its stdio handles on
    // first use; the migration call below eprintln's). It deliberately
    // does NOT AttachConsole the parent: PowerShell/cmd keep reading a
    // shared console and steal keystrokes. The console path then skips
    // all GUI initialization (the WEBVIEW2 env var below, the builder,
    // the window). Accepts both `-console` and `--console`; project
    // resolution inside build_brain also honors `--project <path>` + the
    // cwd walk, same as the GUI. `--console` matches the `--project` flag
    // spelling (two dashes).
    if startup::console_mode_requested(std::env::args()) {
        console::attach_console();
        // Migrate the legacy pre-rename config dir (~/.myharness →
        // ~/.mnemo) before build_brain reads config — the same requirement
        // the GUI path below satisfies, ordered after the attach above
        // because it can eprintln. No-op on fresh installs and after a
        // successful migration.
        mnemo::config::migrate_legacy_config_dir();
        mnemo::app::install_panic_hook();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build the console tokio runtime");
        let code = rt.block_on(console::run_console());
        std::process::exit(code);
    }

    // Migrate the legacy pre-rename config dir (~/.myharness → ~/.mnemo)
    // BEFORE anything reads config (browser_inspection_enabled,
    // build_brain). No-op on fresh installs and after a successful
    // migration.
    mnemo::config::migrate_legacy_config_dir();

    // Unix (macOS): adopt the login shell's $PATH before any child process
    // spawns (the shell tool, git, the headless browser). No-op on Windows.
    inherit_shell_path();

    // Expose the WebView2 Chrome DevTools Protocol on the per-instance CDP
    // port (9222 when free — see `pick_cdp_port`) so the agent
    // can attach to the *live* WebView2 (the Browser tab's child webview —
    // the one the human browses/plays in)
    // and inspect its state. WebView2 reads this env var at creation time and
    // merges it with `additionalBrowserArgs` in tauri.conf.json.
    //
    // `--disable-extensions` is set unconditionally (hardening — was always-on
    // in the old tauri.conf.json window's `additionalBrowserArgs`). The CDP
    // port is appended only in debug builds, or release builds where the user
    // opted in via `enable_browser_inspection` — the endpoint is
    // unauthenticated, so any local process could run arbitrary JS / read the
    // DOM of the app's webview. The opt-in makes that risk explicit. The env
    // var MUST be set before WebView2 is created (it reads it at creation
    // time), so this runs before the Tauri builder.
    //
    // Windows-only block: `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is a
    // WebView2 concept (ignored by wry on other platforms) and the flags
    // below address Windows-specific behavior. Gating the block also keeps
    // `browser_inspection_enabled` alive on Windows targets only, matching
    // its own `#[cfg(windows)]`.
    //
    // MNEMO_DISABLE_WIN_OCCLUSION=1 is an opt-in escape hatch for RDP
    // users (fix #4 of .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md):
    // it appends --disable-features=CalculateNativeWinOcclusion so the
    // renderer stays unthrottled while the window is occluded (no
    // buffered-event catch-up burst) at the cost of background CPU/GPU while
    // hidden.
    #[cfg(windows)]
    {
        // Pick this instance's CDP port BEFORE the env var is built, and
        // record it so BrowserManager (constructed later, after the
        // builder) attaches to the same port the env var exposes — a
        // second instance whose WebView2 cannot bind 9222 gets its own
        // port instead of silently attaching to the first instance's
        // WebView2 (backlog 55ba23b1).
        let cdp_port = if cfg!(debug_assertions) || browser_inspection_enabled() {
            let port = mnemo::webview_args::pick_cdp_port();
            mnemo::webview_args::set_cdp_port(port);
            Some(port)
        } else {
            None
        };
        let args = mnemo::webview_args::build_webview2_args(
            cdp_port,
            mnemo::webview_args::env_flag_enabled(std::env::var_os("MNEMO_DISABLE_WIN_OCCLUSION")),
        );
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
    }

    mnemo::app::install_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Sweep orphaned browser profile dirs left behind by crashed
            // prior runs (dead managers' stale markers, legacy >1h dirs) —
            // the in-session watchdog re-sweeps periodically; see
            // `BrowserManager::sweep_orphan_profiles`. Runs on every startup
            // path, even if this session never launches a browser.
            tauri::async_runtime::spawn_blocking(
                mnemo::browser::BrowserManager::sweep_orphan_profiles,
            );

            // Path (a): bare WindowBuilder + two ordered add_child webviews.
            // The window itself carries no webview; the agent-chat (React) and
            // browser-tab children are added below. Creating the agent-chat
            // webview first means its WebView2 env binds the CDP port;
            // the browser-tab child (created lazily by browser_webview.rs)
            // shares that env. A renderer crash in either child leaves the
            // other alive (proven by spike e9cd6fb1).
            let agent_chat_handle: Arc<std::sync::Mutex<Option<tauri::webview::Webview>>> =
                Arc::new(std::sync::Mutex::new(None));
            // A switch restart reloads in place of a window the user just
            // closed: `switch_project` writes the pending-project marker and
            // hard-exits (tauri::process::restart). Windows hands the
            // foreground to another app when the old window is destroyed, and
            // the relaunched process has no foreground right of its own (its
            // parent is dead) — without this the restarted window opens
            // BEHIND whatever took focus. set_focus (tao force_window_active:
            // SetForegroundWindow + the ALT-key fallback) brings it back to
            // the top, matching the window it replaces. Peeking does not
            // consume the marker — build_brain below still owns it.
            let switched_restart = mnemo::config::peek_pending_project().is_some();
            let window = WindowBuilder::new(app, "main")
                .title("Mnemo")
                .inner_size(1200.0, 720.0)
                .min_inner_size(800.0, 560.0)
                .center()
                .build()?;
            if switched_restart {
                if let Err(e) = window.set_focus() {
                    eprintln!("mnemo: failed to focus the restarted window: {e}");
                }
            }
            // Per-instance WebView2 user data folder: the FIRST instance
            // keeps WebView2's default profile; every ADDITIONAL instance
            // gets its own per-pid folder (see webview_udf.rs — without it a
            // second instance's session never initializes and its window is
            // a blank white frame, 2027-01-13). MUST run before the first
            // webview: WebView2 bakes the folder into its environment at
            // creation time.
            webview_udf::install_default_data_dir(app.handle(), std::process::id());

            // The agent-chat webview fills the whole window (the browser-tab
            // child is positioned over its placeholder rect by browser_webview.rs).
            let agent_chat_builder = webview_udf::apply(
                WebviewBuilder::new("agent-chat", WebviewUrl::App("index.html".into())),
                "agent-chat",
            );
            let agent_chat = window.add_child(
                agent_chat_builder,
                PhysicalPosition::new(0, 0),
                window.inner_size()?,
            )?;
            let _ = agent_chat.show();
            *agent_chat_handle
                .lock()
                .expect("agent_chat handle poisoned") = Some(agent_chat.clone());

            // Keep the agent-chat webview filling the window on resize (an
            // add_child webview does not auto-fill like a WebviewWindow does)
            // — but NEVER call `set_size` on the event loop: a WebView2
            // controller call can block on a stalled compositor when an RDP
            // session is switched (upstream WebView2Feedback #3581), which
            // froze the whole app (R1, .coding/reviews/2026-08-18-rdp-freeze-
            // diagnosis.md). Resizes go to a latest-wins worker thread
            // instead: intermediate sizes coalesce, and a wedged call parks
            // only that thread. The worker's clone keeps the webview handle
            // alive until process exit, so the RunEvent::Exit drop of the
            // state handle is no longer the last reference — the OS reaps the
            // HWND at process termination moments later.
            let resize_sink = agent_chat.clone();
            let resize_worker =
                mnemo::thread_util::spawn_latest_wins(move |size: tauri::PhysicalSize<u32>| {
                    let _ = resize_sink.set_size(size);
                });
            // `on_window_event` lives on `Window` (not `WindowBuilder`), so it
            // is attached after the window is built.
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Resized(size) = event {
                    resize_worker.submit(*size);
                }
            });

            // Try to build the brain. If it fails, we still open the window —
            // the error is surfaced in the UI as a startup-error screen so the
            // user can fix the code and restart. This prevents the
            // chicken-and-egg problem of the app crashing when you're using it
            // to code on itself.
            let brain_result = build_brain(
                Some(Arc::new(TauriNotifier(app.handle().clone()))),
                tauri::async_runtime::handle().inner().clone(),
            );

            let pending_approvals = Arc::new(PendingApprovals::new());
            let pending_questions = Arc::new(PendingQuestions::new());

            // The backlog is UI state that must work even when the brain
            // failed to build, so resolve the project root independently of
            // the brain result (mirroring the Err path's cwd fallback) and
            // open the store before the match.
            let backlog_root = match &brain_result {
                Ok(BrainOutcome::Ready(brain)) => brain.workspace.root.clone(),
                // NeedsProject + Err: no project root — fall back to cwd. The
                // backlog store tolerates a missing path (opens empty), and on
                // the NeedsProject path no agent can run so nothing is added.
                _ => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            };

            // Same-project instance conflict + marker (2027-01-13):
            // `.coding/instance.json` in the project records which instance
            // launched last on it. The conflict is read BEFORE this
            // instance's marker overwrites the incumbent's, so a second
            // instance on a LIVE project can warn the user before opening.
            // Both are best-effort — a read-only project must still open
            // (the marker write is `let _ =`).
            let instance_conflict: Option<ipc::startup::InstanceConflict> =
                if matches!(&brain_result, Ok(BrainOutcome::Ready(_))) {
                    use crate::instance_marker::{conflict_for, InstanceMarker};

                    let conflict = conflict_for(
                        &backlog_root,
                        std::process::id(),
                        startup::instance_pid_alive,
                    );
                    let _ = InstanceMarker::new_self().write(&backlog_root);
                    conflict.map(|m| ipc::startup::InstanceConflict {
                        pid: m.pid,
                        started_at: m.started_at,
                    })
                } else {
                    None
                };

            // Set the OS window title to the product name + the loaded
            // project's folder name, so the user can tell which project is
            // open at a glance (e.g. "Mnemo — myproject"). The static
            // title in tauri.conf.json is the initial/fallback shown before
            // setup runs. On the NeedsProject path there is no project yet,
            // so the title is just the product name. `backlog_root` holds the
            // resolved project root in the Ready branch (and cwd otherwise),
            // and `join` below only borrows it, so reading its file name here
            // is safe.
            if let Some(window) = app.get_window("main") {
                let needs_project =
                    matches!(&brain_result, Ok(BrainOutcome::NeedsProject(_)));
                let _ = window.set_title(&startup::window_title(needs_project, &backlog_root));
            }

            let backlog = Arc::new(tokio::sync::Mutex::new(mnemo::app::backlog::BacklogStore::open(
                backlog_root
                    .join(Project::CODING_DIR_NAME)
                    .join("backlog.jsonl"),
            )));

            match &brain_result {
                Ok(BrainOutcome::Ready(brain)) => {
                    attach_child_ensurer(&brain.browser, app.handle().clone());
                    let laya_cfg = tauri::async_runtime::block_on(async {
                        brain.config.lock().await.general.general.laya.clone()
                    });
                    let (laya, pending_laya_start) = attach_laya(brain, &laya_cfg);
                    // Create the agent manager + per-agent loop map, wrapped in
                    // Arcs up front so the spawner can share them.
                    let mut mgr = AgentManager::new(256);

                    // Take the fan-in receiver from the bare manager before
                    // wrapping it in an Arc<Mutex> (the forwarder owns it).
                    let fanin_rx = mgr.take_fanin_rx().expect("fan-in receiver already taken");

                    let manager = Arc::new(tokio::sync::Mutex::new(mgr));
                    let agent_loops =
                        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

                    // Wire the spawn_agent tool's spawner into the factory BEFORE
                    // building the main agent, so every agent (including main) can
                    // start background agents. The spawner shares the manager +
                    // loop map + factory with the IPC commands, and holds the app
                    // handle so it can emit a PromptDispatched event for the
                    // spawned agent's task (showing it as the first prompt).
                    //
                    // The same `IpcSpawner` is wired as the DescendantTracker
                    // (it implements both traits) so the dispatch layer can gate
                    // workflow-state transitions on "no spawned subagents
                    // running". Build it once and hand clones to both setters.
                    let spawner = Arc::new(ipc::spawn::IpcSpawner::new(
                        brain.factory.clone(),
                        manager.clone(),
                        agent_loops.clone(),
                        app.handle().clone(),
                    ));
                    brain.factory.set_spawner(spawner.clone());
                    brain.factory.set_descendant_tracker(spawner);

                    // Wire the app-side policy factory (2b.2; master-spec
                    // :266-269): the app impl builds each agent's WorkflowPolicy
                    // and registers the app-owned tools (ask_user + the backlog
                    // trio). The backlog store + notifier live on the app factory
                    // (the app-owned wiring) — the kernel factory only sees the
                    // trait. Wired before the first agent build (the policy and
                    // its tool registrations are read per build). The skill
                    // library loads HERE (2b.3: the kernel factory's skills
                    // surface retired — the bin loads the same
                    // `.coding/skills/*.toml` dir the brain used to wire and
                    // hands it to the app factory, so the skill tools and
                    // `enter_skill`'s handle resolve app→app).
                    let skills_dir = brain.workspace.state_dir.join("skills");
                    let skills = Arc::new(mnemo::app::skill::SkillLibrary::load(skills_dir));
                    let policy_factory = Arc::new(
                        mnemo::app::policy_factory::WorkflowPolicyFactory::new(
                            brain.factory.memory_handle(),
                            brain.factory.knowledge_handle(),
                            brain.factory.codegraph_handle(),
                            brain.factory.reviews_dir(),
                            Some(skills),
                        ),
                    );
                    let factory_dyn: Arc<dyn mnemo::AgentPolicyFactory> =
                        policy_factory.clone();
                    brain.factory.set_policy_factory(factory_dyn);

                    // Share the ONE backlog store Arc (created above, before
                    // the match) between the IPC `backlog_*` commands
                    // (state.backlog.store) and the agent's `backlog_add`
                    // tool — both go through the same mutex, so agent adds
                    // and UI mutations stay consistent and can never clobber
                    // each other. Wired before the main agent is built (the
                    // tool is read per registry build). No backlog wiring on
                    // the NeedsProject/Err paths — no factory, no agent.
                    policy_factory.set_backlog(Arc::clone(&backlog));

                    // The UI seam for agent-side adds: the library crate has
                    // no Tauri dependency, so the factory's notifier callback
                    // is the bridge. After every successful `backlog_add`
                    // the closure re-emits the current backlog state on
                    // `backlog://changed` (the same event the IPC commands
                    // fire), so the Backlog tab updates live instead of
                    // waiting for a refresh/restart. Runs at tool-execution
                    // time, when IpcState is already managed (app.manage
                    // below) — the spawn is fire-and-forget on the Tauri
                    // async runtime.
                    {
                        let app_handle = app.handle().clone();
                        let notifier: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                            let app = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                let state = app.state::<IpcState>();
                                crate::ipc::backlog_cmds::emit_backlog_changed(&app, &state).await;
                            });
                        });
                        policy_factory.set_backlog_notifier(notifier);
                    }

                    // Build the first ("main") agent via the factory so it gets
                    // its own workflow + tool registry (now including spawn_agent).
                    // This uses the SAME shared spawn path as the UI `spawn_agent`
                    // command and the IpcSpawner tool (Maint M3) — id allocation,
                    // channel setup, loop-map insert, and registration are
                    // identical. The setup closure is sync, so briefly block_on
                    // (the locks are uncontended at startup).
                    tauri::async_runtime::block_on(async {
                        ipc::spawn::spawn_agent_shared(
                            ipc::spawn::SpawnHost {
                                factory: brain.factory.clone(),
                                manager: manager.clone(),
                                agent_loops: agent_loops.clone(),
                            },
                            "main".to_string(),
                            None,
                            None,
                            ipc::spawn::SessionOptions {
                                // no forced model — the main agent uses the default
                                model: None,
                                // no role — the main agent is unrestricted
                                role: None,
                                // own_plans_dir — main keeps .coding/plans/
                                own_plans_dir: false,
                                // no worktree binding — the main agent works the main tree
                                worktree: None,
                                // 3a.5: keep the factory build's default policy (pre-3a.5 caller)
                                permission: None,
                                // 3a.6 (G5): the main agent never resumes
                                initial_messages: None,
                                // 3a.7 (G7): the main agent sets no session limits
                                limits: None,
                                // 3c (G2): the main agent registers no host tools
                                host_tools: vec![],
                            },
                        )
                        .await
                        .expect("main agent spawn must succeed");
                    });

                    // Spawn the event forwarder — takes ownership of the
                    // fan-in receiver, a clone of the manager Arc, and a
                    // clone of the per-agent loops map (so it can remove dead
                    // agents' loops on Exited).
                    ipc::spawn_event_forwarder(
                        app.handle().clone(),
                        fanin_rx,
                        manager.clone(),
                        pending_approvals.clone(),
                        pending_questions.clone(),
                        agent_loops.clone(),
                    );

                    // Push live console events from the shared browser to the
                    // Browser tab as `browser://console`.
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        brain.browser.clone(),
                    );

                    // Start the hang watchdog (main thread, after the brain
                    // is wired): from here on, any main-thread stall — agent
                    // runs, IPC, frontend event floods — writes an evidence
                    // report into .coding/logs/hang-*.txt.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        backlog_root.join(Project::CODING_DIR_NAME).join("logs"),
                    );

                    app.manage(IpcState {
                        app_policy_factory: Some(Arc::clone(&policy_factory)),
                        runtime: ipc::state::AgentRuntimeContext {
                            manager,
                            agent_loops,
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: Some(brain.factory.clone()),
                            memory_store: brain.own_memory_store.clone(),
                            safety_mode: brain.safety_mode.clone(),
                            safety_rules: Some(brain.safety_rules.clone()),
                            model_resolver: Some(brain.model_resolver.clone()),
                            browser: brain.browser.clone(),
                            startup_error: None,
                            needs_project: false,
                            embedder_status: brain.embedder_status.clone(),
                            classifier_status: brain.classifier_status.clone(),
                            classifier: brain.classifier_slot.clone(),
                            laya: laya.clone(),
                            instance_conflict,
                        },
                        project: ipc::state::ProjectContext {
                            root: Arc::new(tokio::sync::Mutex::new(
                                mnemo::app::project::Project::from_root(&brain.workspace.root),
                            )),
                            config: brain.config.clone(),
                            sandbox: brain.sandbox.clone(),
                        },
                        approvals: pending_approvals.clone(),
                        questions: pending_questions.clone(),
                        trace: brain.trace.clone(),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog.clone(),
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });

                    // The embedder status was already determined by
                    // `build_embedder` during brain construction (Ready for a
                    // loaded bundled model or hash mode; Failed when a
                    // configured model fails to load). Emit it once so the
                    // frontend's startup poll + banner reflect the real state.
                    // (The legacy Ollama/cloud probe was removed with the
                    // remote-embedder path — the bundled path has no probe.)
                    {
                        let app_handle = app.handle().clone();
                        let status_lock = app.state::<IpcState>().runtime.embedder_status.clone();
                        let status = status_lock
                            .read()
                            .expect("embedder status lock poisoned")
                            .clone();
                        let _ = app_handle.emit("embedder://status", &status);
                    }

                    // The classifier status was already determined during
                    // brain construction (Starting when the managed sidecar
                    // was spawned, Disabled otherwise). Emit it once so the
                    // Settings → Classifier section reflects the real state
                    // on mount.
                    {
                        let app_handle = app.handle().clone();
                        let status_lock = app.state::<IpcState>().runtime.classifier_status.clone();
                        let status = status_lock
                            .read()
                            .expect("classifier status lock poisoned")
                            .clone();
                        let _ = app_handle.emit("classifier://status", &status);
                    }

                    // First-run model download: the configured bundled model
                    // wasn't installed, so the store started on the hash
                    // embedder. Download in the background (progress via
                    // embedder://status); on success the task installs the
                    // real embedder into the live store + re-embeds rows, so
                    // semantic recall goes live without a restart.
                    if let Some(model) = &brain.pending_model_download {
                        if let Some(store) = &brain.own_memory_store {
                            ipc::embeddings::spawn_startup_download(
                                app.handle().clone(),
                                store.clone(),
                                brain.embedder_status.clone(),
                                model.clone(),
                            );
                        }
                    }

                    // Installed-model local load: the model is on disk, so no
                    // download — but its ONNX load was deferred off the main
                    // thread. Load in the background and swap into the live
                    // store + re-embed rows (the same install path as the
                    // download, minus the network).
                    if let Some(model) = &brain.pending_model_load {
                        if let Some(store) = &brain.own_memory_store {
                            ipc::embeddings::spawn_startup_load(
                                app.handle().clone(),
                                store.clone(),
                                brain.embedder_status.clone(),
                                model.clone(),
                            );
                        }
                    }

                    // Managed Laya: the classifier client already points at
                    // the pre-allocated loopback port (attach_laya); spawn
                    // the sidecar now and drive the status to Ready/Failed
                    // (progress on classifier://status).
                    if let Some((_checkpoint_id, port)) = &pending_laya_start {
                        let app_handle = Some(app.handle().clone());
                        let manager = laya.clone();
                        let checkpoint = ipc::laya::english_checkpoint();
                        let port = *port;
                        tauri::async_runtime::spawn(async move {
                            ipc::laya::start_managed_server(
                                &app_handle,
                                manager,
                                checkpoint,
                                port,
                            )
                            .await;
                        });
                    }

                    // Startup failure-triage fine-tune (backlog 1a4049c1,
                    // `[general.laya] auto_finetune`): managed runtime only.
                    // The gate (enabled + managed + flag + installed) lives
                    // in ipc::finetune; the spawned task counts the labeled
                    // rows newer than the fine-tune marker and, when ≥50
                    // accrued, fine-tunes via the managed venv and hot-swaps
                    // the served checkpoint. Never blocks startup.
                    ipc::finetune::spawn_startup_finetune(
                        Some(app.handle().clone()),
                        laya.clone(),
                        brain.classifier_slot.clone(),
                        brain.classifier_status.clone(),
                        brain.config.clone(),
                    );
                }
                Ok(BrainOutcome::NeedsProject(config)) => {
                    // No project resolved at startup — show the project picker.
                    // The window is open and the config is loaded (so the
                    // picker can list registered projects), but no agent can
                    // run until a project is chosen. Build a minimal IpcState
                    // mirroring the startup-error fallback, but with
                    // needs_project=true and startup_error=None so the
                    // frontend shows the picker (not the error screen).
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let fallback_project = Project::from_root(&cwd);
                    let fallback_sandbox = Sandbox::new(&cwd)
                        .unwrap_or_else(|_| Sandbox::new(std::path::Path::new(".")).unwrap());
                    let fallback_safety = Arc::new(RwLock::new(SafetyMode::default()));
                    let mgr = AgentManager::new(256);
                    // A shared Disabled classifier status + manager pair —
                    // managed Laya stays inert without a brain, but the
                    // Settings catalog + setup still work.
                    let (fallback_classifier_status, laya_manager) =
                        ipc::laya::fresh_disabled();
                    let fallback_browser = mnemo::browser::BrowserManager::new();
                    attach_child_ensurer(&fallback_browser, app.handle().clone());
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        fallback_browser.clone(),
                    );

                    // Hang watchdog with a temp-dir fallback: no project is
                    // open, so .coding/logs doesn't exist yet — but a hang in
                    // the picker still deserves an evidence report.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        std::env::temp_dir().join("mnemo-hang-reports"),
                    );

                    app.manage(IpcState {
                        app_policy_factory: None,
                        runtime: ipc::state::AgentRuntimeContext {
                            manager: Arc::new(tokio::sync::Mutex::new(mgr)),
                            agent_loops: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: None,
                            memory_store: None,
                            safety_mode: fallback_safety,
                            safety_rules: None,
                            model_resolver: None,
                            browser: fallback_browser,
                            startup_error: None,
                            needs_project: true,
                            instance_conflict: None,
                            embedder_status: Arc::new(RwLock::new(
                                mnemo::memory::embedder::EmbedderStatus::Ready,
                            )),
                            classifier_status: fallback_classifier_status,
                            classifier: Arc::new(RwLock::new(None)),
                            laya: laya_manager,
                        },
                        project: ipc::state::ProjectContext {
                            root: Arc::new(tokio::sync::Mutex::new(fallback_project)),
                            // `brain_result` is matched by reference (the Ready
                            // arm borrows `brain`), so `config` is a
                            // `&Box<Config>` here and must be cloned through
                            // the box — it cannot be moved out.
                            config: Arc::new(tokio::sync::Mutex::new((**config).clone())),
                            sandbox: Arc::new(fallback_sandbox),
                        },
                        approvals: pending_approvals.clone(),
                        questions: pending_questions.clone(),
                        trace: Arc::new(LlmRequestLog::new()),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog.clone(),
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    eprintln!("failed to build brain: {msg}");

                    // Construct a minimal fallback state so commands don't
                    // panic. The frontend will show the startup-error screen.
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let fallback_project = Project::from_root(&cwd);
                    let fallback_sandbox = Sandbox::new(&cwd)
                        .unwrap_or_else(|_| Sandbox::new(std::path::Path::new(".")).unwrap());
                    let fallback_config = Arc::new(tokio::sync::Mutex::new(Config::default()));
                    // A shared Disabled classifier status + manager pair —
                    // managed Laya stays inert without a brain, but the
                    // Settings catalog + setup still work.
                    let (fallback_classifier_status, laya_manager) =
                        ipc::laya::fresh_disabled();
                    let fallback_safety = Arc::new(RwLock::new(SafetyMode::default()));
                    let mgr = AgentManager::new(256);
                    // The brain failed to build, so there is no shared
                    // browser from the factory — create a fresh manager so
                    // the Browser tab still works (it spawns its own
                    // Chromium on first use).
                    let fallback_browser = mnemo::browser::BrowserManager::new();
                    attach_child_ensurer(&fallback_browser, app.handle().clone());
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        fallback_browser.clone(),
                    );

                    // Hang watchdog with a temp-dir fallback: the brain
                    // failed to build, so no project logs dir — but a hang on
                    // the startup-error screen still deserves evidence.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        std::env::temp_dir().join("mnemo-hang-reports"),
                    );

                    app.manage(IpcState {
                        app_policy_factory: None,
                        runtime: ipc::state::AgentRuntimeContext {
                            manager: Arc::new(tokio::sync::Mutex::new(mgr)),
                            agent_loops: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: None,
                            memory_store: None,
                            safety_mode: fallback_safety,
                            safety_rules: None,
                            model_resolver: None,
                            browser: fallback_browser,
                            startup_error: Some(msg),
                            needs_project: false,
                            instance_conflict: None,
                            embedder_status: Arc::new(RwLock::new(
                                mnemo::memory::embedder::EmbedderStatus::Ready,
                            )),
                            classifier_status: fallback_classifier_status,
                            classifier: Arc::new(RwLock::new(None)),
                            laya: laya_manager,
                        },
                        project: ipc::state::ProjectContext {
                            root: Arc::new(tokio::sync::Mutex::new(fallback_project)),
                            config: fallback_config,
                            sandbox: Arc::new(fallback_sandbox),
                        },
                        approvals: pending_approvals,
                        questions: pending_questions,
                        // No brain → no providers to capture, but the Trace tab
                        // still needs a live (empty) log to list/clear.
                        trace: Arc::new(LlmRequestLog::new()),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog,
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::agent::send_prompt,
            ipc::agent::send_suggestion,
            ipc::agent::cancel_suggestion,
            ipc::agent::interrupt,
            ipc::agent::cancel,
            ipc::agent::compact,
            ipc::agent::clear_conversation,
            ipc::agent::approve,
            ipc::agent::answer_question,
            ipc::agent::set_safety_mode,
            ipc::agent::get_safety_mode,
            ipc::agent::get_safety_rules,
            ipc::agent::save_safety_rules,
            ipc::agent::add_safety_rule,
            ipc::agent::add_safety_rule_broad,
            ipc::agent::add_safety_rule_class,
            ipc::agent::get_startup_error,
            ipc::agent::list_agents,
            ipc::agent::ui_diag,
            ipc::agent::context_caps,
            ipc::startup::startup_snapshot,
            ipc::spawn::spawn_agent,
            ipc::agent::set_model,
            ipc::agent::get_workflow_state,
            ipc::agent::get_plan,
            ipc::agent::enter_skill,
            ipc::settings::get_settings,
            ipc::settings::save_settings,
            ipc::settings::get_embedder_status,
            ipc::settings::get_classifier_status,
            ipc::embeddings::list_bundled_embedding_models,
            ipc::embeddings::download_bundled_model,
            ipc::laya::laya_catalog,
            ipc::laya::laya_setup,
            ipc::keys::get_api_keys,
            ipc::mcp::mcp_list_servers,
            ipc::mcp::mcp_save_servers,
            ipc::mcp::mcp_oauth_start,
            ipc::mcp::mcp_status,
            ipc::mcp::mcp_test,
            ipc::models::list_models,
            ipc::models::list_vision_models,
            ipc::settings::save_endpoints,
            ipc::projects::list_projects,
            ipc::projects::create_project,
            ipc::projects::switch_project,
            ipc::projects::remove_project,
            ipc::projects::pick_directory,
            ipc::projects::get_needs_project,
            ipc::trace::list_llm_requests,
            ipc::trace::get_llm_request,
            ipc::trace::clear_llm_requests,
            ipc::trace::get_trace_logging,
            ipc::trace::set_trace_logging,
            ipc::trace::get_steering_stats,
            ipc::memory_debug::memory_debug_overview,
            ipc::memory_debug::memory_debug_list,
            ipc::memory_debug::memory_debug_recall,
            ipc::memory_debug::memory_access_log,
            ipc::memory_debug::memory_debug_resolve_link,
            ipc::memory_debug::memory_debug_backlinks,
            ipc::memory_maintenance::memory_cleanup,
            ipc::memory_maintenance::memory_rebuild_search,
            ipc::memory_maintenance::memory_rebuild_index,
            ipc::memory_maintenance::memory_index_status,
            ipc::codegraph_cmds::codegraph_status,
            ipc::codegraph_cmds::codegraph_refresh,
            ipc::codegraph_cmds::codegraph_graph,
            ipc::codegraph_cmds::get_index_progress,
            ipc::codegraph_cmds::codegraph_rebuild_index,
            ipc::agent::get_session_stats,
            ipc::agent::get_project_stats,
            ipc::agent::get_savings_stats,
            ipc::agent::get_session_list,
            ipc::files::read_file,
            ipc::files::read_image_data_url,
            ipc::files::write_file,
            ipc::files::list_files,
            ipc::files::git_diff_head,
            ipc::files::git_init,
            ipc::files::git_history,
            ipc::files::browse_markdown_file,
            ipc::files::get_git_branch,
            ipc::files::save_conversation,
            ipc::files::load_conversation,
            ipc::browser::browser_pages,
            ipc::browser::browser_screenshot_latest,
            ipc::browser::browser_console,
            ipc::browser::browser_open,
            ipc::browser::browser_normalize_url,
            ipc::browser_webview::browser_webview_supported,
            ipc::browser_webview::browser_webview_ensure,
            ipc::browser_webview::browser_webview_ensure_for_agent,
            ipc::browser_webview::browser_webview_set_rect,
            ipc::browser_webview::browser_webview_navigate,
            ipc::browser_webview::browser_webview_stop,
            ipc::browser_webview::browser_webview_reload,
            ipc::browser_webview::browser_webview_set_tab_visible,
            ipc::browser_webview::browser_webview_overlay_enter,
            ipc::browser_webview::browser_webview_overlay_exit,
            ipc::browser_webview::browser_webview_destroy,
            ipc::backlog_cmds::backlog_add,
            ipc::backlog_cmds::backlog_list,
            ipc::backlog_cmds::backlog_remove,
            ipc::backlog_cmds::backlog_reorder,
            ipc::backlog_cmds::backlog_clear_finished,
            ipc::backlog_cmds::backlog_retry,
            ipc::backlog_cmds::backlog_edit,
            ipc::backlog_cmds::backlog_set_deferred,
            ipc::backlog_cmds::backlog_set_auto_feed,
            ipc::backlog_cmds::backlog_set_parallel_run_all,
            ipc::backlog_cmds::backlog_dispatch_next,
            ipc::backlog_cmds::backlog_dispatch_item,
            ipc::backlog_cmds::backlog_run_all,
            ipc::backlog_cmds::backlog_stop_all,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mnemo app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop the hang watchdog FIRST: the exit teardown below
                // deliberately blocks the main thread for up to 10 s, which
                // the watchdog would otherwise (correctly) report as a stall.
                if let Some(watchdog) = &app_handle.state::<IpcState>().watchdog {
                    watchdog.stop();
                }

                // Stop the managed laya-serve sidecar — it must never
                // outlive the app (the LayaManager Drop is the backstop).
                app_handle.state::<IpcState>().runtime.laya.stop();

                // Drain any queued trace/provider-error log writes before the
                // process exits — mirroring is asynchronous (background writer
                // thread), so without this the last moments of a session's
                // log lines could be lost. Kept OUTSIDE the timed teardown
                // below: it does not touch WebView2 and must always run.
                app_handle.state::<IpcState>().trace.flush_file_writes();

                // The remaining teardown touches the headless browser /
                // WebView2, whose calls can block on a wedged compositor
                // (RDP session switch — R5 of .coding/reviews/2026-08-18-
                // rdp-freeze-diagnosis.md). Run them on a separate thread
                // with a hard timeout. The bound must exceed
                // BrowserManager::close()'s NORMAL worst case — up to ~5 s of
                // Arc::try_unwrap retries + the kill wait, plus any time
                // parked behind a state lock held by an in-flight browser
                // launch (review F1) — so only a genuine wedge hits it; a
                // spurious timeout would orphan the headless Chromium child
                // (chromiumoxide never sets kill_on_drop, and Windows does
                // not reap children on parent exit).
                let browser = app_handle.state::<IpcState>().runtime.browser.clone();
                let browser_webview = app_handle.state::<IpcState>().browser_webview.clone();
                let agent_chat = app_handle.state::<IpcState>().agent_chat_webview.clone();
                let teardown = move || {
                    // Kill the shared headless debug browser before the process
                    // exits: chromiumoxide never sets kill_on_drop, so a bare
                    // drop at teardown would leave the Chromium child alive
                    // holding locks on its throwaway profile dir (Review L2).
                    let _ = tauri::async_runtime::block_on(browser.close());
                    // Destroy the child WebView2 (drop the handle) so no native
                    // HWND leaks on exit. Mirrors the headless-browser close above.
                    tauri::async_runtime::block_on(async move {
                        let mut bw = browser_webview.state.lock().await;
                        // Dropping the Webview handle tears down the platform
                        // webview (wry closes the child on drop).
                        bw.take_webview();
                    });
                    // Drop the agent-chat (React) child webview handle too, so
                    // no native HWND leaks on exit. try_lock, not lock: a
                    // wedged holder of this std::sync::Mutex must never block
                    // exit (defense in depth — today the resize worker holds
                    // its own Webview clone and this mutex is only ever held
                    // briefly, but that invariant is cheap to keep).
                    if let Ok(mut h) = agent_chat.try_lock() {
                        *h = None;
                    }
                };
                if !mnemo::thread_util::run_with_timeout(
                    std::time::Duration::from_secs(10),
                    teardown,
                ) {
                    eprintln!(
                        "mnemo: exit teardown exceeded 10s (wedged compositor?) — \
                         proceeding to exit; WebView2 HWNDs are reaped by process \
                         termination, but the headless Chromium child may survive"
                    );
                }
            }
        });
}

#[cfg(test)]
mod switch_restart_focus_contract {
    //! Source-contract regression tests for the switch-restart window focus.
    //! The defect needs two processes + a window manager to reproduce
    //! end-to-end, so the guard pins the wiring instead (the same convention
    //! as the frontend's windowRestore source contracts): a launch that
    //! peeks the pending-project marker must focus the window it creates.

    /// The window-creation path must peek the marker and set_focus when it
    /// is present — without the guard the relaunched window opens behind
    /// whatever took the foreground when the old window was destroyed.
    #[test]
    fn switch_restart_focuses_the_restarted_window() {
        let src = include_str!("main.rs");
        assert!(
            src.contains("peek_pending_project().is_some()"),
            "the restart detection must peek the pending-project marker"
        );
        assert!(
            src.contains("window.set_focus()"),
            "the restarted window must be focused (brought on top)"
        );
    }
}

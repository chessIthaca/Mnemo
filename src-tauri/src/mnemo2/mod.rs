// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's minimal Tauri host.
//!
//! Mnemo2 is an application built on the headless engine, not a second copy
//! of Mnemo1's IPC/tool surface. This module owns only the desktop shell and
//! the lead-session bridge.

/// The headless-engine-backed lead conversation.
pub mod runtime;

/// Durable Phase 3 review and merge records.
pub mod phase3;

/// The project's startup code-index and memory passes, as one status.
pub mod indexing;

/// The real worker team runtime.
pub mod team;

/// Best-effort naming of the processes holding a worker checkout's files.
pub mod lock_holders;

/// Durable per-project usage and cost accounting.
pub mod usage;

/// The Cost tab's spend and savings insight commands.
pub mod insights;

/// The Plans tab's backlog section: the project's `.coding/backlog.jsonl`
/// ideas, read and edited in place.
pub mod backlog;

/// The lead's team ledger for the TeamLead workflow.
pub(crate) mod ledger;

/// The typed Tauri command surface for the lead conversation.
pub mod ipc;

/// Real project registry access and folder picking.
pub mod project;

/// Read-only worker diffs and screenshot artifacts for the workbench.
pub mod workbench;

/// The Graph tab's code-graph commands (status, search, neighbourhood,
/// impact, entry points, context).
pub mod graph;

/// The native child-WebView2 Browser pane and its non-Windows fallback.
pub mod browser;

/// The per-instance run file the `mnemo2-mcp` helper finds a live Mnemo2 by.
pub mod run_file;

/// The Laya decision-model settings commands (status, setup, disable,
/// save) — the backend of the Settings dialog's Laya page (plan
/// e02de747).
pub mod laya_settings;

/// The local endpoint for an external lead (Claude Code over MCP).
pub mod mcp_server;

/// Compact model names for external worker displays.
pub mod model_label;

/// Model-role settings for the Models and Savings pages: per-task
/// models, vision, the code-indexing (embedding) model with install
/// progress, optimizer levers, per-model pricing and budget caps.
pub mod model_settings;

/// A liveness command used by the frontend to distinguish the Mnemo2 host
/// from any other Tauri WebView (for example Mnemo1's Browser tab), which
/// also exposes `__TAURI_INTERNALS__`.
#[tauri::command]
fn mnemo2_host_ping() -> &'static str {
    "pong"
}

/// What the command line asked for: `mnemo2 [--hidden] [--project <path>]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartArgs {
    /// Start with the main window hidden, living in the tray.
    pub hidden: bool,
    /// A project folder the frontend should open on startup.
    pub project: Option<String>,
}

/// Read [`StartArgs`] from the process arguments (without the program name).
/// Unknown arguments are ignored; the last `--project` wins.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> StartArgs {
    let mut parsed = StartArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--hidden" {
            parsed.hidden = true;
        } else if arg == "--project" {
            parsed.project = args.next().filter(|path| !path.is_empty());
        } else if let Some(path) = arg.strip_prefix("--project=") {
            parsed.project = Some(path.to_string()).filter(|path| !path.is_empty());
        }
    }
    parsed
}

/// The tray's "Workers" line.
fn workers_label(running: usize) -> String {
    format!("Workers: {running} running")
}

/// How many worker turns are running on the open project.
async fn running_workers(app: &tauri::AppHandle) -> usize {
    use tauri::Manager as _;

    let Some(state) = app.try_state::<ipc::Mnemo2State>() else {
        return 0;
    };
    match state.open_team().await {
        Some(team) => team.live_worker_turns().await,
        None => 0,
    }
}

/// Quit from the tray, asking first while workers are running.
fn request_quit(app: &tauri::AppHandle) {
    use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let running = running_workers(&app).await;
        if running == 0 {
            app.exit(0);
            return;
        }
        let quitting = app.clone();
        app.dialog()
            .message(format!(
                "{running} worker(s) are still running. Quitting interrupts them; their branches and transcripts are kept."
            ))
            .title("Quit Mnemo2?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Quit".to_string(),
                "Keep running".to_string(),
            ))
            .show(move |confirmed| {
                if confirmed {
                    quitting.exit(0);
                }
            });
    });
}

/// The tray icon: open Mnemo2, a live worker count, and quit.
fn install_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Open Mnemo2", true, None::<&str>)?;
    let workers = MenuItem::with_id(app, "workers", workers_label(0), false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &workers, &quit])?;
    let mut tray = TrayIconBuilder::with_id("mnemo2")
        .tooltip("Mnemo2")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => mcp_server::show_main_window(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                mcp_server::show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    // A cheap tick keeps the count current without tracking every event.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut shown = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let running = running_workers(&app).await;
            if running != shown {
                shown = running;
                let _ = workers.set_text(workers_label(running));
            }
        }
    });
    Ok(())
}

/// Check every 30 s that the run file still exists and write it again if it
/// does not, so the helper keeps finding this instance. Ends with the file's
/// removal at exit.
fn keep_run_file(run_file: std::sync::Arc<run_file::RunFileHandle>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(run_file::RESTORE_EVERY).await;
            if run_file.is_removed() {
                return;
            }
            match run_file.restore_if_missing() {
                Ok(true) => eprintln!("mnemo2: the run file had disappeared; wrote it again"),
                Ok(false) => {}
                Err(error) => eprintln!("mnemo2: could not restore the run file: {error}"),
            }
        }
    });
}

/// Start the local endpoint for an external lead and publish its run file.
/// The app still works without it, so failures are logged, not fatal.
fn start_endpoint(app: &tauri::AppHandle) {
    use tauri::Manager as _;

    let config_dir = mnemo::config::resolve_config_dir(None);
    run_file::prune_stale(&config_dir, crate::instance_marker::pid_alive);
    let token = run_file::new_token();
    let host = std::sync::Arc::new(mcp_server::AppLeadHost::new(app.clone()));
    let server = tauri::async_runtime::block_on(mcp_server::McpServer::start(
        host,
        token.clone(),
        mcp_server::ServerOptions::default(),
    ));
    let server = match server {
        Ok(server) => server,
        Err(error) => {
            eprintln!("mnemo2: could not start the local MCP endpoint: {error}");
            return;
        }
    };
    match run_file::RunFileHandle::create(&config_dir, server.port(), token) {
        Ok(run_file) => {
            let run_file = std::sync::Arc::new(run_file);
            keep_run_file(run_file.clone());
            let state = app.state::<ipc::Mnemo2State>();
            state.install_endpoint(server, run_file);
        }
        Err(error) => eprintln!("mnemo2: could not write the run file: {error}"),
    }
}

/// Whether the window starts hidden and hides instead of closing. `--hidden`
/// only works with a tray icon to bring the window back from; without one the
/// window stays visible and closes normally, or it could never be reached.
fn starts_hidden(hidden: bool, tray_ok: bool) -> bool {
    hidden && tray_ok
}

fn install_shell(app: &tauri::AppHandle, hidden: bool) -> tauri::Result<()> {
    use tauri::{PhysicalPosition, PhysicalSize, WebviewBuilder, WebviewUrl, WindowBuilder};

    let window = WindowBuilder::new(app, "main")
        .title("Mnemo2")
        .inner_size(1440.0, 900.0)
        .min_inner_size(360.0, 540.0)
        .center()
        .visible(!hidden)
        .build()?;
    crate::webview_udf::install_default_data_dir(app, std::process::id());
    let shell = window.add_child(
        crate::webview_udf::apply(
            WebviewBuilder::new("mnemo2-shell", WebviewUrl::App("index.html".into())),
            "mnemo2-shell",
        ),
        PhysicalPosition::new(0, 0),
        window.inner_size()?,
    )?;
    shell.show()?;
    let resize_sink = shell.clone();
    let resize_worker = mnemo::thread_util::spawn_latest_wins(move |size: PhysicalSize<u32>| {
        let _ = resize_sink.set_size(size);
    });
    let hider = window.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Resized(size) => resize_worker.submit(*size),
        // A hidden-start Mnemo2 lives in the tray: closing only hides it.
        tauri::WindowEvent::CloseRequested { api, .. } if hidden => {
            api.prevent_close();
            let _ = hider.hide();
        }
        _ => {}
    });
    Ok(())
}

const EXIT_RUNNING: u8 = 0;
const EXIT_SHUTTING_DOWN: u8 = 1;
const EXIT_READY: u8 = 2;
const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Copy)]
#[repr(usize)]
enum ShutdownStep {
    Starting,
    SavingPreferences,
    StoppingRuntimes,
    ClosingBrowser,
}

struct ShutdownProgress(std::sync::atomic::AtomicUsize);

impl ShutdownProgress {
    fn new() -> Self {
        Self(std::sync::atomic::AtomicUsize::new(
            ShutdownStep::Starting as usize,
        ))
    }

    fn set(&self, step: ShutdownStep) {
        self.0
            .store(step as usize, std::sync::atomic::Ordering::Release);
    }

    fn pending_step(&self) -> &'static str {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            step if step == ShutdownStep::SavingPreferences as usize => "saving preferences",
            step if step == ShutdownStep::StoppingRuntimes as usize => "stopping runtimes",
            step if step == ShutdownStep::ClosingBrowser as usize => "closing the browser",
            _ => "starting shutdown",
        }
    }
}

/// Run shutdown on the async runtime, leaving the event-loop thread free to
/// service WebView2 work. The exit callback always runs, even when a step
/// stalls or panics.
fn spawn_orderly_shutdown<F, E>(
    shutdown: F,
    exit: E,
    progress: std::sync::Arc<ShutdownProgress>,
    deadline: std::time::Duration,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
    E: FnOnce() + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        let shutdown = tauri::async_runtime::spawn(shutdown);
        match tokio::time::timeout(deadline, shutdown).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                eprintln!(
                    "mnemo2: shutdown failed while {}: {error}; exiting anyway",
                    progress.pending_step()
                );
            }
            Err(_) => {
                eprintln!(
                    "mnemo2: shutdown deadline exceeded while {}; exiting anyway",
                    progress.pending_step()
                );
            }
        }
        exit();
    });
}

/// Build and run the Mnemo2 desktop host.
///
/// The bare window and its shell child are created explicitly so per-instance
/// WebView2 data directories are selected before either child exists.
pub fn run() {
    std::env::remove_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS");
    mnemo::app::install_panic_hook();
    let args = parse_args(std::env::args().skip(1));
    let state = ipc::Mnemo2State::default();
    state.set_startup_project(args.project.as_deref().and_then(|path| {
        match project::canonical_root(std::path::Path::new(path)) {
            Ok(root) => Some(root.to_string_lossy().into_owned()),
            Err(error) => {
                eprintln!("mnemo2: ignoring --project: {error}");
                None
            }
        }
    }));
    let hidden = args.hidden;
    let exit_phase = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(EXIT_RUNNING));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .manage(browser::BrowserController::default())
        .setup(move |app| {
            // The tray comes first: whether it exists decides whether the
            // window may start hidden and hide on close.
            let tray_ok = match install_tray(app.handle()) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("mnemo2: could not create the tray icon: {error}");
                    false
                }
            };
            install_shell(app.handle(), starts_hidden(hidden, tray_ok))?;
            start_endpoint(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            mnemo2_host_ping,
            graph::graph_status,
            graph::graph_refresh,
            graph::graph_search,
            graph::graph_neighbourhood,
            graph::graph_impact,
            graph::graph_top,
            graph::graph_context,
            ipc::mnemo2_startup_project,
            ipc::external_lead_status,
            ipc::external_open_failed,
            ipc::lead_open,
            laya_settings::laya_status,
            laya_settings::laya_setup,
            laya_settings::laya_disable,
            laya_settings::save_laya_settings,
            ipc::lead_snapshot,
            ipc::indexing_snapshot,
            ipc::lead_prompt,
            ipc::lead_interrupt,
            ipc::lead_compact,
            ipc::lead_set_model,
            ipc::lead_approve,
            ipc::lead_answer,
            ipc::lead_close,
            ipc::team_snapshot,
            ipc::board_snapshot,
            ipc::board_plan_document,
            backlog::backlog_items,
            backlog::backlog_add,
            backlog::backlog_edit,
            backlog::backlog_remove,
            backlog::backlog_reorder,
            backlog::backlog_set_deferred,
            backlog::backlog_clear_finished,
            ipc::settings_endpoints,
            ipc::settings_upsert_endpoint,
            ipc::settings_remove_endpoint,
            ipc::settings_set_api_key,
            ipc::settings_test_endpoint,
            ipc::settings_list_provider_models,
            ipc::settings_endpoint_usage,
            ipc::settings_tiers,
            ipc::settings_save_tiers,
            model_settings::model_settings_get,
            model_settings::model_settings_save,
            model_settings::embedding_install,
            ipc::team_dispatch,
            ipc::team_audit_dispatch,
            ipc::team_message,
            ipc::team_pause,
            ipc::team_interrupt,
            ipc::team_stop,
            ipc::team_resume,
            ipc::team_set_model,
            ipc::team_compact,
            ipc::team_review,
            ipc::team_record_check,
            ipc::team_resolve_finding,
            ipc::team_review_fix,
            ipc::team_route_fix,
            ipc::team_merge,
            ipc::team_revert_merge,
            ipc::team_stop_all,
            ipc::team_set_slots,
            ipc::team_delete_preview,
            ipc::team_delete,
            ipc::team_messages,
            ipc::project_list,
            ipc::project_add,
            ipc::project_create,
            ipc::project_pick_folder,
            ipc::project_attach,
            ipc::project_cleanup,
            ipc::workbench_diff,
            ipc::workbench_screenshot,
            ipc::workbench_preferences,
            ipc::workbench_preferences_set,
            browser::browser_supported,
            browser::browser_snapshot,
            browser::browser_ensure,
            browser::browser_navigate,
            browser::browser_history,
            browser::browser_reload,
            browser::browser_set_rect,
            browser::browser_set_visible,
            browser::browser_overlay,
            browser::browser_close,
            browser::browser_open_external,
            insights::insights_cost,
            insights::insights_savings,
        ])
        .build(tauri::generate_context!("mnemo2-app/tauri.conf.json"))
        .expect("failed to build the Mnemo2 desktop host")
        .run(move |app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                use std::sync::atomic::Ordering;

                if exit_phase.load(Ordering::Acquire) == EXIT_READY {
                    return;
                }
                api.prevent_exit();
                if exit_phase
                    .compare_exchange(
                        EXIT_RUNNING,
                        EXIT_SHUTTING_DOWN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    return;
                }

                let shutdown_app = app.clone();
                let progress = std::sync::Arc::new(ShutdownProgress::new());
                let shutdown_progress = progress.clone();
                let shutdown = async move {
                    use tauri::Manager as _;

                    let state = shutdown_app.state::<ipc::Mnemo2State>();
                    let browser = shutdown_app.state::<browser::BrowserController>();
                    // Stop advertising this instance before tearing the project down.
                    state.shutdown_endpoint();
                    shutdown_progress.set(ShutdownStep::SavingPreferences);
                    if let Ok(owner) = ipc::state_owner(&shutdown_app) {
                        let _ = state.save_cached_preferences(&owner).await;
                    }
                    shutdown_progress.set(ShutdownStep::StoppingRuntimes);
                    state.shutdown().await;
                    // A settings-installed sidecar that no lead adopted at
                    // its pipeline's end lives only in laya_settings'
                    // static hold — a static is never dropped, so stop it
                    // explicitly here (review finding heldManagerOrphan).
                    laya_settings::stop_held_manager();
                    shutdown_progress.set(ShutdownStep::ClosingBrowser);
                    let _ = browser.close().await;
                };
                let exit_app = app.clone();
                let exit_phase = exit_phase.clone();
                spawn_orderly_shutdown(
                    shutdown,
                    move || {
                        exit_phase.store(EXIT_READY, Ordering::Release);
                        exit_app.exit(code.unwrap_or(0));
                    },
                    progress,
                    SHUTDOWN_DEADLINE,
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> StartArgs {
        parse_args(list.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn start_args_read_hidden_and_project() {
        assert_eq!(args(&[]), StartArgs::default());
        assert_eq!(
            args(&["--hidden"]),
            StartArgs {
                hidden: true,
                project: None
            }
        );
        assert_eq!(
            args(&["--hidden", "--project", "C:\\work\\app"]),
            StartArgs {
                hidden: true,
                project: Some("C:\\work\\app".into())
            }
        );
        assert_eq!(args(&["--project=/p/q"]).project.as_deref(), Some("/p/q"));
        // The last --project wins; unknown flags and a bare --project are fine.
        assert_eq!(
            args(&["--project", "a", "--other", "--project", "b"]).project.as_deref(),
            Some("b")
        );
        assert_eq!(args(&["--project"]).project, None);
        assert_eq!(args(&["--project="]).project, None);
    }

    #[test]
    fn a_hidden_start_needs_a_tray_to_come_back_from() {
        assert!(starts_hidden(true, true), "hidden with a tray: hide and live in the tray");
        // Without a tray the window is shown and closing really closes it.
        assert!(!starts_hidden(true, false), "no tray: stay visible, do not hide on close");
        assert!(!starts_hidden(false, true));
        assert!(!starts_hidden(false, false));
    }

    #[test]
    fn the_tray_worker_line_counts_running_workers() {
        assert_eq!(workers_label(0), "Workers: 0 running");
        assert_eq!(workers_label(3), "Workers: 3 running");
    }

    #[test]
    fn orderly_shutdown_leaves_the_event_loop_thread_free() {
        let (main_tx, main_rx) = std::sync::mpsc::channel::<tokio::sync::oneshot::Sender<()>>();
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();
        let progress = std::sync::Arc::new(ShutdownProgress::new());
        let shutdown_progress = progress.clone();
        spawn_orderly_shutdown(
            async move {
                shutdown_progress.set(ShutdownStep::ClosingBrowser);
                let (done_tx, done_rx) = tokio::sync::oneshot::channel();
                main_tx.send(done_tx).expect("request main-thread work");
                done_rx.await.expect("main-thread work completes");
            },
            move || exit_tx.send(()).expect("report exit"),
            progress,
            std::time::Duration::from_secs(2),
        );

        // This models WebView2 close: it can finish only while the thread that
        // requested shutdown is still servicing event-loop work.
        let done = main_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("shutdown runs without blocking the event-loop thread");
        done.send(()).expect("finish main-thread work");
        exit_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("shutdown proceeds to exit");
    }

    #[test]
    fn stalled_shutdown_exits_after_the_deadline() {
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();
        let progress = std::sync::Arc::new(ShutdownProgress::new());
        progress.set(ShutdownStep::StoppingRuntimes);
        spawn_orderly_shutdown(
            std::future::pending(),
            move || exit_tx.send(()).expect("report exit"),
            progress.clone(),
            std::time::Duration::from_millis(20),
        );

        exit_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("a pending shutdown step cannot prevent exit");
        assert_eq!(progress.pending_step(), "stopping runtimes");
    }

    #[test]
    fn panicking_shutdown_step_still_exits() {
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();
        let progress = std::sync::Arc::new(ShutdownProgress::new());
        let shutdown_progress = progress.clone();
        spawn_orderly_shutdown(
            async move {
                shutdown_progress.set(ShutdownStep::ClosingBrowser);
                panic!("simulated browser close panic");
            },
            move || exit_tx.send(()).expect("report exit"),
            progress.clone(),
            std::time::Duration::from_secs(2),
        );

        exit_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("a panicking shutdown step cannot prevent exit");
        assert_eq!(progress.pending_step(), "closing the browser");
    }

    /// The native shell must compile with Mnemo2's own Tauri identity. Sharing
    /// Mnemo1's identity also shares its WebView2 user-data folder, which
    /// deadlocks the second process before its webview initializes.
    #[test]
    fn context_uses_mnemo2_identifier() {
        let context: tauri::Context<tauri::Wry> =
            tauri::generate_context!("mnemo2-app/tauri.conf.json", test = true);
        assert_eq!(context.config().identifier, "com.mnemo.mnemo2");
    }

    /// Library tests need the same COMCTL32 v6 manifest as desktop binaries.
    #[test]
    fn library_tests_link_tauri_resource_manifest() {
        let build_script = include_str!("../../build.rs");
        assert!(build_script.contains("cargo:rustc-link-arg-tests="));
        assert!(build_script.contains("resource.lib"));
    }
}

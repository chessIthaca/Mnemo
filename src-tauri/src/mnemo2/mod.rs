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

/// Durable per-project usage and cost accounting.
pub mod usage;

/// The lead's team ledger for the TeamLead workflow.
pub(crate) mod ledger;

/// The typed Tauri command surface for the lead conversation.
pub mod ipc;

/// Real project registry access and folder picking.
pub mod project;

/// Read-only worker diffs and screenshot artifacts for the workbench.
pub mod workbench;

/// The native child-WebView2 Browser pane and its non-Windows fallback.
pub mod browser;

/// A liveness command used by the frontend to distinguish the Mnemo2 host
/// from any other Tauri WebView (for example Mnemo1's Browser tab), which
/// also exposes `__TAURI_INTERNALS__`.
#[tauri::command]
fn mnemo2_host_ping() -> &'static str {
    "pong"
}

fn install_shell(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::{PhysicalPosition, PhysicalSize, WebviewBuilder, WebviewUrl, WindowBuilder};

    let window = WindowBuilder::new(app, "main")
        .title("Mnemo2")
        .inner_size(1440.0, 900.0)
        .min_inner_size(360.0, 540.0)
        .center()
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
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Resized(size) = event {
            resize_worker.submit(*size);
        }
    });
    Ok(())
}

/// Build and run the Mnemo2 desktop host.
///
/// The bare window and its shell child are created explicitly so per-instance
/// WebView2 data directories are selected before either child exists.
pub fn run() {
    std::env::remove_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS");
    mnemo::app::install_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(ipc::Mnemo2State::default())
        .manage(browser::BrowserController::default())
        .setup(|app| {
            install_shell(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            mnemo2_host_ping,
            ipc::lead_open,
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
            ipc::settings_endpoints,
            ipc::settings_upsert_endpoint,
            ipc::settings_remove_endpoint,
            ipc::settings_set_api_key,
            ipc::settings_test_endpoint,
            ipc::settings_list_provider_models,
            ipc::settings_endpoint_usage,
            ipc::settings_tiers,
            ipc::settings_save_tiers,
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
        ])
        .build(tauri::generate_context!("mnemo2-app/tauri.conf.json"))
        .expect("failed to build the Mnemo2 desktop host")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager as _;
                let state = app.state::<ipc::Mnemo2State>();
                let browser = app.state::<browser::BrowserController>();
                let _ = tauri::async_runtime::block_on(async {
                    if let Ok(owner) = ipc::state_owner(app) {
                        let _ = state.save_cached_preferences(&owner).await;
                    }
                    state.shutdown().await;
                    let _ = browser.close().await;
                });
            }
        });
}

#[cfg(test)]
mod tests {
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

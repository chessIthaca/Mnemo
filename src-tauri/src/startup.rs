// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pure startup decision logic, extracted from `main.rs` (quality review
//! LOW 4) so it is unit-testable without Tauri state or I/O: console-mode
//! detection, the `--project` flag parse, the startup provider's
//! endpoint/model fallback ordering, the re-embed-at-startup gate, and the
//! window-title decision. `main()` / `build_brain_inner` keep only the
//! wiring.

use mnemo::config::{Config, Endpoint};

/// Whether the process was launched in console mode (`-console` or
/// `--console`) — run the brain as a terminal REPL, never touching
/// WebView2/Tauri. Checked FIRST, before anything else in `main()`: a
/// release build is a GUI-subsystem binary, so a terminal launch starts
/// with NULL std handles — `console::attach_console()` must rewire stdio
/// before the first print anywhere in the process. `--console` matches the
/// `--project` flag spelling (two dashes).
pub(crate) fn console_mode_requested(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|a| a == "-console" || a == "--console")
}

/// The `--project <path>` CLI override, when present. Only the
/// space-separated form is honored (matching `--console`'s spelling); a
/// trailing `--project` with no value yields `None`.
pub(crate) fn project_flag_from_args(
    args: impl IntoIterator<Item = String>,
) -> Option<std::path::PathBuf> {
    args.into_iter()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--project")
        .and_then(|w| w.get(1))
        .map(std::path::PathBuf::from)
}

/// The startup provider's endpoint + model, resolved from config with the
/// documented fallback ordering:
///
/// - endpoint: the `default_provider`-named endpoint, else the config's
///   default endpoint (the first when `default_provider` is unset or
///   names a deleted endpoint).
/// - model: `default_model`, else the endpoint's first model id, else
///   `"gpt-4o"` (the terminal fallback — the caller builds a dummy
///   provider when the endpoint is `None`).
pub(crate) fn resolve_startup_provider(config: &Config) -> (Option<&Endpoint>, String) {
    let endpoint_name = config.general.general.default_provider.as_deref();
    let endpoint = endpoint_name
        .and_then(|name| config.endpoint(name))
        .or_else(|| config.default_endpoint());

    let model = config
        .general
        .general
        .default_model
        .clone()
        .or_else(|| endpoint.and_then(|e| e.model_ids().first().cloned()))
        .unwrap_or_else(|| "gpt-4o".to_string());
    (endpoint, model)
}

/// Whether the cross-machine re-embed check should run at startup. Every
/// gate exists for a documented reason:
///
/// - A bundled model must be configured (`None` = hash-only setup, nothing
///   semantic to re-embed with).
/// - The explicit `"hash"` opt-out must skip (case-insensitive, matching
///   the sentinel contract in `build_embedder` / `embedder_startup_plan` —
///   a case-sensitive compare would let the hash embedder destroy stored
///   semantic vectors).
/// - A pending first-run download or local load must skip — the background
///   task owns the re-embed (running it here with the interim hash
///   embedder would destroy vectors).
/// - The status must be `Ready` — when the model failed to load (hash
///   fallback), there is nothing useful to re-embed with.
pub(crate) fn should_reembed_at_startup(
    configured: Option<&str>,
    pending_download: bool,
    pending_load: bool,
    status: &mnemo::memory::embedder::EmbedderStatus,
) -> bool {
    configured.is_some()
        && !configured.is_some_and(|v| {
            v.eq_ignore_ascii_case(mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH)
        })
        && !pending_download
        && !pending_load
        && *status == mnemo::memory::embedder::EmbedderStatus::Ready
}

/// The OS window title: the product name plus the loaded project's folder
/// name (e.g. `Mnemo — myproject`), so the user can tell which project is
/// open at a glance. On the NeedsProject path there is no project yet, so
/// the title is just the product name. A root without a file name (e.g.
/// `/`) degrades to `Mnemo — .`.
pub(crate) fn window_title(needs_project: bool, root: &std::path::Path) -> String {
    if needs_project {
        return "Mnemo".to_string();
    }
    let folder = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".");
    format!("Mnemo — {folder}")
}

/// Whether a process with `pid` is alive right now. The same-project
/// conflict warning asks this about the incumbent's marker pid — a stale
/// marker from a dead instance must not warn. Windows: an `OpenProcess`
/// existence probe (PROCESS_QUERY_LIMITED_INFORMATION needs no special
/// rights); elsewhere a `ps -p` check, so the app needs no libc dependency
/// on either platform.
pub(crate) fn instance_pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed flags, no handle inheritance.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle was just opened and is no longer used.
        unsafe { CloseHandle(handle) };
        true
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .ok()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::memory::embedder::EmbedderStatus;

    /// An endpoint named `name` serving `models` (ids only — no per-model
    /// overrides needed here). Built on [`Endpoint::test_default`] so new
    /// `Endpoint` fields never break these tests.
    fn endpoint_named(name: &str, models: &[&str]) -> Endpoint {
        Endpoint {
            name: name.into(),
            models: models
                .iter()
                .map(|id| mnemo::config::ModelSpec {
                    id: (*id).to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Endpoint::test_default()
        }
    }

    // --- console_mode_requested ---

    #[test]
    fn console_mode_accepts_both_spellings() {
        assert!(console_mode_requested(["-console".to_string()]));
        assert!(console_mode_requested(["--console".to_string()]));
        assert!(console_mode_requested([
            "mnemo".to_string(),
            "--console".to_string(),
        ]));
    }

    #[test]
    fn console_mode_absent_is_gui() {
        assert!(!console_mode_requested(["mnemo".to_string()]));
        assert!(!console_mode_requested([
            "mnemo".to_string(),
            "--project".to_string(),
            "/tmp/p".to_string(),
        ]));
        assert!(!console_mode_requested(Vec::<String>::new()));
    }

    // --- project_flag_from_args ---

    #[test]
    fn project_flag_parses_space_separated_value() {
        assert_eq!(
            project_flag_from_args([
                "mnemo".to_string(),
                "--project".to_string(),
                "/tmp/p".to_string(),
            ]),
            Some(std::path::PathBuf::from("/tmp/p"))
        );
    }

    #[test]
    fn project_flag_absent_is_none() {
        assert_eq!(
            project_flag_from_args(["mnemo".to_string(), "--console".to_string()]),
            None
        );
    }

    #[test]
    fn project_flag_trailing_without_value_is_none() {
        assert_eq!(
            project_flag_from_args(["mnemo".to_string(), "--project".to_string()]),
            None
        );
    }

    #[test]
    fn project_flag_first_occurrence_wins() {
        assert_eq!(
            project_flag_from_args([
                "mnemo".to_string(),
                "--project".to_string(),
                "/first".to_string(),
                "--project".to_string(),
                "/second".to_string(),
            ]),
            Some(std::path::PathBuf::from("/first"))
        );
    }

    #[test]
    fn project_flag_equals_form_is_not_honored() {
        // Only the space-separated form is honored, matching --console.
        assert_eq!(
            project_flag_from_args(["mnemo".to_string(), "--project=/tmp/p".to_string()]),
            None
        );
    }

    // --- resolve_startup_provider ---

    #[test]
    fn provider_named_endpoint_and_default_model_win() {
        let mut config = Config::default();
        config.endpoints.push(endpoint_named("ep", &["m1", "m2"]));
        config.endpoints.push(endpoint_named("other", &["x"]));
        config.general.general.default_provider = Some("ep".into());
        config.general.general.default_model = Some("chosen".into());
        let (endpoint, model) = resolve_startup_provider(&config);
        assert_eq!(endpoint.map(|e| e.name.as_str()), Some("ep"));
        assert_eq!(model, "chosen");
    }

    #[test]
    fn provider_dangling_default_falls_back_to_first_endpoint() {
        // default_provider names a deleted endpoint — fall back to the
        // first configured endpoint rather than the dummy provider.
        let mut config = Config::default();
        config.endpoints.push(endpoint_named("first", &["m1"]));
        config.endpoints.push(endpoint_named("second", &["x"]));
        config.general.general.default_provider = Some("gone".into());
        let (endpoint, model) = resolve_startup_provider(&config);
        assert_eq!(endpoint.map(|e| e.name.as_str()), Some("first"));
        assert_eq!(model, "m1");
    }

    #[test]
    fn provider_no_default_provider_uses_first_endpoint() {
        let mut config = Config::default();
        config.endpoints.push(endpoint_named("first", &["m1"]));
        config.endpoints.push(endpoint_named("second", &["x"]));
        let (endpoint, model) = resolve_startup_provider(&config);
        assert_eq!(endpoint.map(|e| e.name.as_str()), Some("first"));
        assert_eq!(model, "m1");
    }

    #[test]
    fn provider_no_endpoints_is_none_with_gpt4o_terminal() {
        let config = Config::default();
        let (endpoint, model) = resolve_startup_provider(&config);
        assert!(endpoint.is_none());
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn provider_model_falls_back_to_endpoint_first_model() {
        let mut config = Config::default();
        config.endpoints.push(endpoint_named("ep", &["m1", "m2"]));
        config.general.general.default_provider = Some("ep".into());
        let (_, model) = resolve_startup_provider(&config);
        assert_eq!(model, "m1");
    }

    // --- should_reembed_at_startup ---

    #[test]
    fn reembed_runs_when_configured_ready_and_no_pendings() {
        assert!(should_reembed_at_startup(
            Some("bge-small-en-v1.5"),
            false,
            false,
            &EmbedderStatus::Ready,
        ));
    }

    #[test]
    fn reembed_skips_hash_opt_out_case_insensitively() {
        // The sentinel check must match build_embedder's contract
        // case-insensitively — a case-sensitive compare would let the hash
        // embedder destroy stored semantic vectors.
        assert!(!should_reembed_at_startup(
            Some(mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH),
            false,
            false,
            &EmbedderStatus::Ready,
        ));
        assert!(!should_reembed_at_startup(
            Some("hash"),
            false,
            false,
            &EmbedderStatus::Ready,
        ));
        assert!(!should_reembed_at_startup(
            Some("Hash"),
            false,
            false,
            &EmbedderStatus::Ready,
        ));
    }

    #[test]
    fn reembed_skips_when_download_or_load_pending() {
        // The background task owns the re-embed — running it here with the
        // interim hash embedder would destroy vectors.
        assert!(!should_reembed_at_startup(
            Some("bge-small-en-v1.5"),
            true,
            false,
            &EmbedderStatus::Ready,
        ));
        assert!(!should_reembed_at_startup(
            Some("bge-small-en-v1.5"),
            false,
            true,
            &EmbedderStatus::Ready,
        ));
    }

    #[test]
    fn reembed_skips_when_status_not_ready() {
        for status in [
            EmbedderStatus::Checking,
            EmbedderStatus::Failed,
            EmbedderStatus::Fallback,
            EmbedderStatus::Pulling,
            EmbedderStatus::Downloading {
                model: "m".into(),
                progress: 0.5,
            },
        ] {
            assert!(
                !should_reembed_at_startup(
                    Some("bge-small-en-v1.5"),
                    false,
                    false,
                    &status,
                ),
                "status {status:?} must gate the re-embed off"
            );
        }
    }

    #[test]
    fn reembed_skips_when_unconfigured() {
        assert!(!should_reembed_at_startup(
            None,
            false,
            false,
            &EmbedderStatus::Ready,
        ));
    }

    // --- window_title ---

    #[test]
    fn title_needs_project_is_bare_product_name() {
        assert_eq!(window_title(true, std::path::Path::new("/any/where")), "Mnemo");
    }

    #[test]
    fn title_ready_appends_project_folder() {
        assert_eq!(
            window_title(false, std::path::Path::new("/home/user/myproject")),
            "Mnemo — myproject",
        );
    }

    #[test]
    fn title_rootless_degrades_to_dot() {
        assert_eq!(window_title(false, std::path::Path::new("/")), "Mnemo — .");
    }
}

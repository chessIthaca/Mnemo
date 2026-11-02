+++
title = "Mnemo2 manages Laya from Settings > Decisions and attaches it at engine start"
created = "2026-11-02"
+++

Mnemo2 attaches the managed Laya classifier when its lead engine starts (Setup::attach_laya in src/headless/setup.rs, held by LeadRuntime in src-tauri/src/mnemo2/runtime.rs, stopped in shutdown). Commands in src-tauri/src/mnemo2/laya_settings.rs: laya_status (enabled/installed/status word disabled|not_installed|installing|starting|ready|failed, the seven [general.laya] flags, the [general.routing] lanes), laya_setup (enable + install/start, progress on mnemo2://laya-event as SetupEvent), laya_disable, save_laya_settings(flags, routing) — never carries `enabled`. A sidecar installed from Settings is handed to the open lead (set_laya_manager) or stopped when no lead is open, and stopped at app exit. UI: mnemo2/src/components/LayaPage.svelte + lib/layaSettings.ts, rail entry "Decisions" in SettingsDialog; preview scene settings-laya. With [general.laya] routing = true and enforce off (shadow), every turn appends a RouteDecisionRow to ~/.mnemo/laya/training/routing.jsonl.

Mnemo2's Settings → Safety shows and saves `[git] core_operations`, the git subcommands that always make the lead ask first. Until now only Mnemo1's settings could edit it. Saving applies to the open lead's next git call without a restart.

WHY: the Mnemo1-only settings are being moved or retired before Mnemo1 is removed. The user chose (2026-11-11) to move the git approval list to Mnemo2's Safety page.

HOW IT WORKS TODAY:
- `[git] core_operations: Vec<String>` (src/config/general.rs ~1291, `GitConfig`, default ["merge", "push"]) is the list of git subcommands that always force the approval prompt, even in an auto-approve mode and even when a safety rule matches (`Tool::never_auto_for`). It is enforced on both the GitTool and the ShellTool's git guard.
- The shared settings patch already handles it: `SettingsSaveDto.core_operations: Option<Vec<String>>` (src/config/settings_dto.rs ~422; applied ~881, which trims, lowercases and drops empties).
- `Setup::patch_settings` pushes it into the live factory (src/headless/setup.rs ~593 `set_core_operations`), so the open lead's next git call follows it.
- Team workers run with PermissionPolicy::OverridePermissions and never prompt. The page already says so.

MNEMO2 FILES:
- Host: src-tauri/src/mnemo2/safety_settings.rs. It has the `SafetySettingsWire` snapshot, `read_snapshot`, and the commands `safety_settings_get`, `save_safety_mode` and `save_safety_rules`, registered in src-tauri/src/mnemo2/mod.rs ~667-669.
- Front end: mnemo2/src/lib/safetySettings.ts (types, `normalizeSafetySettings`, the `SafetyTransport` interface, `createTauriSafetyTransport`, the preview seam) and mnemo2/src/components/SafetyPage.svelte.
- Fakes that implement SafetyTransport: `makeSafetyTransport`/`safetySnapshot` in mnemo2/src/components/SettingsDialog.test.ts ~110-120, and the preview transport in mnemo2/src/test/previewSafety.ts.

CONSTRAINTS:
- Mnemo1 (frontend/, src-tauri/src/ipc/) is unchanged.
- The list is machine-global (config.toml), so saving works with or without an open project.
- Reuse the page's existing section pattern (`section.group` with header, `micro` text, `section-actions` with Discard + Save buttons, `trackUnsaved`).
- No new dependencies.

VERIFY:
- `cargo test -p mnemo-app --features mnemo2 --lib safety_settings`
- `cargo +1.99.1 clippy -p mnemo-app --all-targets --features mnemo2`
- `npm test --workspace mnemo2`
- `npm run build:mnemo2` (includes svelte-check)

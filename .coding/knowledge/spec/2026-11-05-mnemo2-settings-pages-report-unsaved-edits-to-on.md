+++
title = "Mnemo2 settings pages report unsaved edits to one guard in the dialog"
created = "2026-11-05"
+++

Settings pages call trackUnsaved(() => dirty) from mnemo2/src/lib/unsavedEdits.ts; SettingsDialog.svelte provides the registry and routes X, Escape, backdrop and rail clicks through requestLeave, which shows InlineConfirm ("Discard unsaved changes?", Keep editing / Discard changes) while anything is unsaved. Sheet.svelte stays unguarded because other dialogs share it. A new settings page with a draft MUST add one trackUnsaved line; immediate-apply pages (Project, Appearance) add none. An open MCP add/edit form counts as unsaved even when empty. Safety, MCP and Appearance render before the endpoints/tiers load gates because they use neither. Whether Tauri's webview shows the beforeunload prompt on window close is unverified. A shared useDraft composable was considered and left out: pages have multi-section drafts and the registry already gives the single plug point. Root cause of the original silent loss: the Svelte 5 rewrite moved to per-page Save/Discard and dropped the React-era dialog-level guard.

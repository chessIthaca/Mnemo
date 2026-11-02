+++
title = "Mnemo2's Safety setting gates the lead only; team workers run ungated"
created = "2026-11-02"
+++

Mnemo2 Settings > Safety (src-tauri/src/mnemo2/safety_settings.rs, mnemo2/src/components/SafetyPage.svelte) saves [general] safety through Setup::patch_settings, which on an open lead also moves the engine's live Arc<RwLock<SafetyMode>> (src/headless/setup.rs ~569-573), so the lead's next tool call follows the new mode without a restart; per-project auto-approve rules live in <project>/.coding/safety.toml (Setup::set_safety_rules, re-read on mtime). Team workers are spawned with PermissionPolicy::OverridePermissions (src-tauri/src/mnemo2/team.rs ~2750), so neither the mode nor the rules gate them: every worker action that would ask is recorded as a bypassed approval and runs. The page states this.

+++
title = "Mnemo2 plays notification sounds for finished, needs-you and stopped-with-errors"
created = "2026-11-02"
+++

Mnemo2 Settings > Appearance & sounds (AppearancePage.svelte, src-tauri/src/mnemo2/appearance_settings.rs) toggles the [ui] sound_complete, sound_input_needed and sound_stopped_errors flags, each with a Test button. mnemo2/src/lib/sounds.ts synthesizes the three tones with one lazy WebAudio context (no asset files) and store.ts rings them on lead events (finished; approval_request/user_question; non-retrying error) and worker status transitions (completed; needs_you; failed) for the open project only. Debounce is per sound kind (1.5 s) with a priority rule: needs-you > stopped-with-errors > finished, so a needs-you ping always plays even right after a completion chime. Toggles apply live without a restart.

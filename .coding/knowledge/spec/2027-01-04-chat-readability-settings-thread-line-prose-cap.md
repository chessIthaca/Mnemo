+++
title = "chat readability settings (thread line, prose cap, turn tint, hover timestamps)"
created = "2027-01-04"
status = "superseded"
+++

Four [ui] settings (plan afa81f0a, commit e674eb3, branch wt/agenticcoding, 2027-01-04), each a Settings→Chat toggle defaulting ON:

- chat_thread_line — activity-run wrapper in Conversation.tsx owns the level-2 indent: ON = ml-[1.5em] border-l border-slate-700/60 pl-[0.5em], OFF = ml-[2em].
- chat_prose_cap — ~100ch max-width on assistant prose only (prose-p/li/blockquote:max-w-[100ch] + streaming/steer text); code blocks/diffs/tool outputs stay full width.
- chat_turn_tint — alternating turn background bands (rounded-lg bg-bg-tertiary/30 on odd turns); turns split on user/steer entries; the activity render-filter never changes turn membership.
- chat_hover_timestamps — title tooltips on transcript entries (fmtTs: time-only same-day, date+time otherwise); legacy unstamped entries degrade gracefully (no tooltip).

Transcript structure (Conversation.tsx): turns → chunkRuns (maximal consecutive activity runs) → renderEntry (tooltip wrapper). The streaming response renders INSIDE the last turn's group (standalone fallback when turns is empty) so the tint band covers it while streaming — no jump at finalize.

Timestamps: ts?: number on TranscriptEntry (types.ts). applyAgentEvent stamps post-event entries identity-based (only entries lacking ts); every direct-push site stamps itself: InputBar optimistic user entry (ts: Date.now()), pushTranscriptMessage ({ ...msg, ts: msg.ts ?? Date.now() }), reduceQuestionAnswered qa literal (ts: Date.now() — invoked directly via recordQuestionAnswer). /load restore keeps saved ts by design.

Plumbing: UiConfig fields + Default true (src/config/general.rs) → save path is SettingsSaveDto + validate_and_apply_settings_patch (settings_dto.rs) ONLY — the patch.rs twin (SettingsPatch + apply_settings_patch) was never wired in and was deleted (plan 8684dc0e, quality review HIGH 3, 2027-01-07), so the original round-1 H1 both-paths trap no longer exists; regression test chat_readability_flags_persist_through_the_save_patch in settings_dto.rs → GetSettingsUi DTO/fixture (settings.rs, contract_fixtures.rs, dto-get-settings.json) → tauri.ts → useAgentStore setters → App.tsx hydration (!== false, absent = ON) → ChatSection toggles (ChatDraft auto-serializes via JSON.stringify).

Reviews: 3 rounds (2 high + 1 low → 1 low → PASS), final report .coding/reviews/2027-01-04-chat-readability-round3-review.md.

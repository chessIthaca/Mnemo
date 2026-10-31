+++
title = "Mnemo2 reopened lead session ignores new prompts (pre-existing)"
created = "2027-01-11"
+++

Mnemo2 (src-tauri/src/mnemo2): a fully-reopened LeadRuntime's resumed session does not process a new send_prompt — no LLM request is made and no events are emitted (verified 2027-02-11 with a text-only ScriptedLlm experiment: open, prompt, wait_for_persisted, shutdown, reopen, prompt — the second drain_turn times out; the session's queue was untouched). This is PRE-EXISTING on main's base, independent of the b952eab4 usage work (whose hooks are inert on text-only turns). Existing reopen tests (restores_the_saved_transcript_and_model_on_reopen) only assert the restored transcript/model and never prompt after reopen, so CI does not catch it. Worker resume is NOT affected: a resumed worker session accepts messages normally. This is the concrete symptom of the open "finish safe project recovery" Phase 4 roadmap item — any lead-restart usage/cost test must prove post-restart accounting through worker resume, not lead re-prompt, until this is fixed.

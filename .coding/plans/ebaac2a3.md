# Plan: An audit that ends its turn with plain text still delivers its report

## Goal
When an audit's turn finishes without a delivered report, Mnemo2 delivers its final assistant text as the report (so the lead gets "reports: ..." and the audit is released like any reported audit); if that text is empty, the audit gets one automatic message asking for the report.

## Kind
bug_fixing

## Context
ROOT CAUSE: an audit's report counts only when it arrives through the `message_parent` host tool: src-tauri/src/mnemo2/team/pump.rs (~380-400) records the TeamMessage and sets `worker.report_delivered = true`; at the Finished event (~760-775) `audit_release = worker.kind == "audit" && worker.report_delivered && status Completed`, and release_finished_audit (~936) ends the session and frees the slot. src-tauri/src/mnemo2/runtime.rs (~1475-1490) turns an audit's Finished event into the "finished its read-only audit ... Adjudicate" notice regardless of a report. The audit brief (team/audit.rs ~96) tells audits to report through message_parent, but models sometimes end with the findings as plain text.

DECISIONS:
- At an audit's Finished event with `report_delivered == false`, take the final assistant message text of that turn (from the transcript the pump keeps or the text streamed in the turn; the worker picks the existing source). If it is non-empty after trimming, deliver it exactly as a message_parent report would be delivered: the same TeamMessage, the same lead notice ("<name> reports: <text>"), `report_delivered = true` — BEFORE the Finished event is forwarded, so the lead sees the report before the adjudication notice and audit_release frees the slot in this same pass. Reuse the message_parent path rather than duplicating it.
- If the final text is empty: send the audit one automatic message (sender = the host, like other automatic nudges) reading "Your audit report did not reach the lead. Send your complete findings with message_parent now, then end your turn." at most once per turn generation; do not release the session.
- Implementation workers are unaffected (kind != "audit").
- The repeated-report rules (plan 9a6cffc1: a second report is refused) still apply; a fallback delivery counts as the first report.

REGRESSION TEST: src-tauri/src/mnemo2/team/tests.rs `an_audit_ending_with_plain_text_delivers_it_as_its_report`, modelled on `finished_audit_releases_its_slot` (~4153): a scripted audit whose turn ends with the text "Audit findings: none." and no message_parent call; assert a TeamMessage from the audit with that text, `report_delivered == true`, status Completed, the session released (slot free), and that the lead's report notice precedes the adjudication notice. It fails on today's code (no TeamMessage, session idle). Second test `an_audit_ending_silently_is_asked_for_its_report_once`: empty final text gives exactly one automatic message and no release.

VERIFY: regression test red then green; `cargo test --workspace`, `cargo clippy --workspace --all-targets` as plain commands. The automatic message text is a prompt: the lead reviews it word by word.

## Acceptance scenarios
- An audit that ends its turn with its findings as plain text (no message_parent) produces a 'reports:' notice with that text before the adjudication notice, and its session is released so it no longer holds a worker slot.
- An audit that ends its turn with no text receives one automatic message asking for its report through message_parent and stays available to answer.

## Steps
- [x] 1. **Deliver an audit's final text as its report when message_parent was skipped** Files: `src-tauri/src/mnemo2/team/pump.rs`, tests in `src-tauri/src/mnemo2/team/tests.rs`; `src-tauri/src/mnemo2/runtime.rs` only if notice ordering needs it. Touch nothing else. 1. In the pump's Finished handling for an audit with report_delivered == false: deliver the turn's final assistant text through the message_parent path (TeamMessage, lead notice, report_delivered) before the Finished event is forwarded, so audit_release applies in the same pass. 2. Empty final text: one automatic message with the exact decided text per turn generation; no release. 3. Regression tests exactly as the context describes (prove the first fails on the unchanged code). Done when: the regression test was red before the fix (say so in the commit message), both tests pass, `cargo test --workspace` and `cargo clippy --workspace --all-targets` pass warning-free, and only the listed files changed.

## Bug
An audit worker that wrote its findings as its final message instead of calling message_parent (fourthAud1, 2026-11-16) produced only the "finished its read-only audit ... Adjudicate" notice with no report text, stayed idle holding a worker slot for hours, and looked stuck in mission control; the lead had to dig the report out of its transcript.

## Regression test
an_audit_ending_with_plain_text_delivers_it_as_its_report

## Decisions
- 2026-11-16: no spec impact: Fallback delivery of an audit report already defined as its message_parent report; no capability requirement changes.

## Progress
- 2026-11-16: Planned 2026-11-16; dispatch only after plan 3b8f715a's worker oneRun01 merges (it rewrites src-tauri/src/mnemo2/team/pump.rs), and one after the other with plan 41cd8251 (both touch pump.rs).
- 2026-11-16 step 1 done: step 1 merged: auditRep01 (auditRep01) on deepseek-v4.1-flash:1017, merge commit 09ea83d1; review: Fallback report delivery for audits as planned; the silent-audit ask is now limited to one per audit session (lead review_fix).

## Verdict: PASS

No code defects found. The threshold change (3→2) propagates correctly through the escalation logic; the one-shot `escalated` HashSet semantics hold; the rewritten escalation note is clean of every marker-detection substring (defense-in-depth atop the architectural `observe_result`-before-`attach_escalations` ordering); both updated tests genuinely exercise the new threshold and assert "2 times"; docs are synced; no Windows-only code; no warnings. One non-blocking observation on a side-car backlog status flip.

### Changes reviewed
- `src/agent/steering_stats.rs` — `ESCALATION_THRESHOLD` 3→2 (doc comment updated); C3 escalation note format string rewritten (permissive→directive); test `repeat_ignored_actionable_nudges_escalate_once` updated for the new threshold.
- `src/agent/dispatch.rs` — test `escalation_notes_attach_to_success_only` loops `0..3`→`0..2`.
- `PLAN.md` — "firing 3×"→"firing 2×".
- New HOW knowledge file + SPEC amendment + new bug knowledge file (side-car docs).

### 1. Correctness — threshold propagation ✓

`observe_result` (steering_stats.rs:380-409): for each detected ACTIONABLE kind (non-empty `targets()`, line 381 gate), increments per-agent `agent_fired`, then checks:

```rust
if fired >= ESCALATION_THRESHOLD
    && switched == 0
    && inner.escalated.insert((agent_id, kind.index()))
```

Trace with threshold 2:
- 1st fire: fired=1, `1 >= 2` false → no escalation (below threshold). ✓
- 2nd fire: fired=2, `2 >= 2` true, switched=0, `insert` returns true (new entry) → note queued. ✓
- 3rd fire: fired=3, `3 >= 2` true, switched=0, `insert` returns false (already present) → no note (one-shot). ✓

The `>=` fires AT the threshold (2nd fire), not after — correct. The check is at the right point: inside the actionable-kind loop, after incrementing `fired`, gated by `switched == 0` and the one-shot `insert`. `&&` short-circuits, so `insert` (the side-effecting one-shot) only runs when both guards pass.

**One-shot semantics hold:** `HashSet::insert` returns true only the first time → exactly one note per (agent, kind). `take_escalations` (line 447) drains `pending_escalations` but does NOT clear `escalated`, so a drain never re-enables escalation. ✓

**Switch cancellation:** `observe_call` (line 432-439) increments `agent_switched[agent][kind]` when the call names a target tool. The `switched == 0` guard then blocks escalation for the rest of the session. Because `&&` short-circuits, `insert` is never called when switched > 0 — the (agent, kind) is never marked escalated, but the `switched` guard alone permanently blocks it. An intervening switch before the threshold-reaching fire is the documented cancellation path (test confirms). ✓

### 2. Self-fire safety ✓

New note text: *"NOTE: the '{label}' marker has fired {fired} times this session without a switch — follow it (its named target tools) on the next call; each repeat wastes a round-trip the target tools would answer directly"*

Checked against all 9 marker substrings (`MarkerKind::marker` / `SEARCH_NUDGE_MARK`):
- "is an indexed symbol" (SearchNudge) — absent ✓
- "TIP: for file-content search" (ShellTip) — absent ✓
- "No symbols matched" (GraphMiss) — absent ✓
- "RECALLED CONTEXT" (RecallRider) — absent ✓
- "SYMBOL NUDGE:" (ReadNudge) — absent ✓
- "TIP: pattern has no regex metacharacters" (LiteralTip) — absent ✓
- "known memory hit:" (KnownMemoryHit) — absent ✓
- "working-memory events accumulated this session" (ConsolidationDue) — absent ✓
- "TIP: output redirection detected" (ShellRedirect) — absent ✓

Also absent: "TIP:", "consider memory_consolidate" (review-focus substrings). The note uses "NOTE:" not "TIP:". ✓

**Architectural defense (primary):** `attach_escalations` (dispatch.rs:670-682) runs AFTER `observe_result` (dispatch.rs:414→415 and 440→441), appending the note via `r.output.push('\n'); r.output.push_str(&note)` at the END. `observe_result` scans the output BEFORE the note is appended; each subsequent `observe_result` scans only the new result. No self-fire path exists. The clean text is documented defense-in-depth (steering_stats.rs:378-379). ✓

### 3. Test correctness ✓

**`repeat_ignored_actionable_nudges_escalate_once`** (steering_stats.rs:546-591):
- 1 fire → empty ("one fire: below threshold") ✓
- 2nd fire → 1 note ("second fire without a switch escalates"); asserts `contains("'search-nudge' marker")` and `contains("2 times")` ✓
- 3rd fire → empty ("one-shot per agent+kind") ✓
- Switch sub-test: fire 1 → observe_call(graph_context) → fire 2 → empty ("a switch cancels the escalation") ✓ (fired=2>=2 but switched=1≠0)
- Fired-only sub-test: 4× LiteralTip → empty (empty targets, skipped) ✓

Both updated tests fail-without/pass-with the threshold change: with threshold 3, the 2nd fire would not escalate (2 < 3), so `assert_eq!(notes.len(), 1)` and `assert!(ok.output.contains("NOTE:"))` would fail. Valid regression coverage. Assertions correctly check "2 times" not "3 times". ✓

**`escalation_notes_attach_to_success_only`** (dispatch.rs:689-720): both loops `0..2` (was `0..3`); success result gets "NOTE:" appended (not prepended — `starts_with("done")`); second attach is empty (drain is final); error result unchanged ("boom"). ✓

### 4. Documentation sync ✓

- PLAN.md: "firing 3×"→"firing 2×" ✓
- SPEC amendment (2026-08-29-enforcement-ladder...md): now states `ESCALATION_THRESHOLD=2 (amended 2026-12-04 from 3...)` and describes the new directive note text ✓
- New HOW file: references "threshold 2" and the new note text ✓
- README.md: no escalation-threshold reference (only unrelated "3×3 retry stack" hits) ✓
- Module doc (steering_stats.rs:1-38) and `ESCALATION_THRESHOLD` doc comment (260-264): no stale "3"/"three" ✓
- No source file contains the old note text ("deliberately ignoring" / "consider following it") — only the plan/spec docs, which legitimately describe the before/after ✓

### 5. Multi-platform neutrality ✓

Pure Rust logic: a const value, a format string, test loop bounds. No Windows-only APIs, paths, shell syntax, or `cfg(windows)`. ✓

### 6. Constitution ✓

- All public/pub(crate) fns have doc comments (unchanged). ✓
- No `#[allow(...)]` added. ✓
- `#![deny(warnings)]`: format string uses both `{label}` (explicit named arg) and `{fired}` (implicit local capture) — both satisfied; `fired` is used in both the `>=` check and the format string, so no unused-variable warning. Changes are minimal and warning-free by inspection. (cargo test reported 1705 passed / 0 failed / 0 warnings per the plan; reviewer has no shell to re-run independently.)

### Non-blocking observation

`.coding/backlog.jsonl` has a status flip on item 8b8f40d2 ("Improve graph-tool usage discipline...") → "failed" with note "plan loop never ran this turn (workflow rested in Complete) — task not done". This is the same goal plan c5cc9a84 implements, so "failed" is mildly misleading — but it is an automated side-car artifact (not a deliberate code change), outside the source-code review scope, and non-blocking. Consider marking it "done" with a pointer to plan c5cc9a84 if the backlog is revisited.

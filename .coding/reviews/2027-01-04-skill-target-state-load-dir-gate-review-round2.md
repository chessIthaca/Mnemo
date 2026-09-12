## Verdict: PASS

Round-2 verification of the L1 fix for plan 7e9d03ec "Close skill target_state invariant at registry load (UI enter_skill path)" on `wt/agenticcoding`. Method: `git diff HEAD` + `git status` (4 modified + 4 untracked), full read of `src/skill/mod.rs` and `src/tool/workflow/skill.rs`, cross-check of `src/workflow/mod.rs:25` (serde form), the round-1 report (`.coding/reviews/2027-01-04-skill-target-state-load-dir-gate-review.md`), and repo-wide literal searches for the old/new message text. The round-1 L1 finding is correctly and completely fixed; no new issues.

### 1. L1 fix correctness — PASS

- **`src/skill/mod.rs:114-118` (load_dir gate eprintln):** the valid set is now phrased in the lowercase serde form — `(must be \"planning\", \"executing\", or \"complete\"); skipping`. The `\`-newline string continuation strips the newline + leading whitespace, so the message renders as one line with exactly one space before `(must be` — verified against Rust's string-continuation semantics. A user correcting their TOML per this message writes `target_state = "planning"` → parses cleanly. The confusing detour into the parse-error arm (mod.rs:123-125) is eliminated.
- **`src/tool/workflow/skill.rs:158-160` (SkillStartTool error):** now `invalid target_state '{target_state}': must be one of \"planning\", \"executing\", \"complete\"`. The valid set matches the JSON/serde form the tool's args actually accept (the schema enum at skill.rs:95 also lists lowercase). The `invalid target_state` prefix is intact.
- **Premise confirmed:** `WorkflowState` is `#[serde(rename_all = "lowercase")]` (src/workflow/mod.rs:25) — lowercase is what TOML/JSON accept, so the rewording is the correct direction. Both sites were fixed together, exactly as the round-1 recommendation prescribed ("do both together, or neither — consistency matters").
- **Gate itself unchanged:** the `matches!(Planning | Executing | Complete)` gate (mod.rs:108-113) and the skill.rs gate (154-157) are byte-identical to what round 1 passed on every axis; the only code delta since round 1 is the two message strings.

### 2. No test breakage (by inspection) — PASS

- **`start_skill_rejects_non_lifecycle_target_state`** (skill.rs:383-406): asserts `!r.success` and `r.output.contains("invalid target_state")` (skill.rs:398). The reworded message still begins `invalid target_state '{target_state}':` — the `contains` check cannot fail. The loop's other asserts (state stays Planning, no active skill) are unaffected by wording.
- **`load_dir_skips_skill_with_non_lifecycle_target_state`** (mod.rs:253-306): asserts only registry contents (`reg.get("good").is_some()`, `reg.get("bad_subagent"/"bad_skill"/"bad_reviewing").is_none()`) — no message-text assertions; the eprintln goes to uncaptured stderr.
- **Repo-wide sweep:** literal searches for `must be one of` and `invalid target_state` hit only (a) the new skill.rs:159 message, (b) the skill.rs:398 test prefix assert, (c) an unrelated doc comment (src/memory/knowledge.rs:613), and (d) historical review documents (`.coding/reviews/2026-01-03-subagent-workflow-state-review-round2.md`) quoting the old wording — immutable review records, not code. **No test anywhere pins the old PascalCase phrasing.** All other message-asserting tests in both files (`unknown skill`, `not available`, `started skill 'merge_to_main'`) target unchanged strings.

### 3. Diff hygiene — PASS

The full uncommitted diff is exactly the expected delta, nothing else:

- **`src/skill/mod.rs`** — the load_dir gate (108-120) + updated doc comment (76-80) + regression test (252-306), with the gate's eprintln carrying the fixed lowercase phrasing.
- **`src/tool/workflow/skill.rs`** — the single reworded message line (159).
- **`.coding/backlog.jsonl`** — 67608f1b `pending`→`in_flight` (+ `plan_id: 7e9d03ec`), 68c4c9a5 `pending`→`done`. Benign bookkeeping.
- **`.coding/plans/bc09914f.md`** — step 4 `[ ]`→`[x]` for the completed show_knowledge_activity plan. Benign, unrelated to this plan's code.
- **Untracked:** `.coding/plans/7e9d03ec.md` (this plan), `.coding/knowledge/bug/2027-01-04-ui-skill-start-with-hand-edited-target-state-sub.md` (BUG record), `.coding/knowledge/decision/2027-01-04-show-knowledge-activity-default-on-for-graph-mem.md` (decision from the other plan), `.coding/reviews/2027-01-04-skill-target-state-load-dir-gate-review.md` (round-1 report). All benign.

Since round 1, the delta is precisely: the two reworded message strings + the round-1 report file appearing as untracked — matching the task brief's "only code delta since round 1".

### 4. Sanity — PASS

- **No new imports / dead code:** mod.rs already imports `WorkflowState` (line 25, pre-existing — used by `SkillSpec`); the gate uses only `matches!`/`eprintln!`/`continue`; the test reuses `tempdir`/`write_skill`/`super::*`. skill.rs is a one-string-literal change. Nothing that could trip `#![deny(warnings)]` — consistent with the parent's re-run (root 1984 passed / 0 failed, src-tauri 186 passed / 0 failed, warning-free).
- **Multi-platform neutral:** pure Rust string formatting; no paths, no OS APIs.
- **Doc comments accurate:** the `load_dir` doc (76-80) correctly describes the target_state skip; its PascalCase `(Planning/Executing/Complete)` phrasing is rustdoc referring to enum variants by their Rust names — idiomatic and correct for developer-facing docs (L1 was about the user-facing diagnostics, which are now lowercase).

### Notes (non-findings)

- **Received-value echo stays PascalCase:** both diagnostics still interpolate the *received* value via `Display` (e.g. `'Subagent'` for a TOML that said `"subagent"`). That is the parsed variant's canonical name, not corrective guidance — the valid set (the part a user corrects against) is now in the accepted form. Unchanged from round 1 and outside L1's scope; not a defect.
- **Historical review docs quote the old wording** (`.coding/reviews/2026-01-03-subagent-workflow-state-review-round2.md:27`) — immutable records of what the code said at review time; not stale code.

Round-1 L1 is fixed correctly at both sites with no collateral changes; the core fix remains as passed in round 1. Nothing remains to address.

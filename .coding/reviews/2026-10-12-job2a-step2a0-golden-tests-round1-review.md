## Verdict: PASS

**Delta:** commit `1b90d9a` on wt/mnemo (the only commit since base `9c7015b`, confirmed via git log) — `src/agent/prompt.rs` +40/−0 and `src/model_resolver.rs` +61/−0, both pure test-module insertions, plus `.coding/` plan-frame bookkeeping (31689c21.md +27, 64962938.md +1/−1). The uncommitted working-tree diff is only the harness's round-1 review stamp on the plan frame (step-5 checkbox + `## Reviews / 1 1b90d9a…`), which correctly records this review's base. All seven checks verified; zero findings.

**What I actually read:** the commit (stat + full message + the tail of the full diff; the two source hunks read line-by-line in the clean tree at HEAD, which equals post-commit state — prompt.rs :905-1014, model_resolver.rs :815-934); the plan (`.coding/plans/31689c21.md`, all 30 lines) and its uncommitted diff; step file §"Step 2a.0" (job-02a-agent-policy-seam.md :60-139, incl. D7); master-spec anchors :913-914, :780-801, :230-243; and the code under test — prompt.rs :22-126 (the three head consts), :500-544 (`build_stable_head`); model_resolver.rs :40-229 (ModelContext + trait docs), :308-407 (`resolve`/`resolve_reviewer_model`/`resolve_summarize_model`), :600-669 (test helpers); src/config/general.rs :810-910 (ModelsConfig field list + documented priority chain); src/config/mod.rs :104-123 (`resolve_model_ref`); Cargo.toml :75-90 (sha2).

Reviewed-state: 1b90d9aeb67091813403d4ef7097967dc17abc30

### (1) ADD-ONLY PURITY — VERIFIED
- `git show --stat 1b90d9a`: exactly 4 files — prompt.rs +40/−0, model_resolver.rs +61/−0 (the commit's single deleted line is in `.coding/plans/64962938.md`, bookkeeping); no manifest, no src-tauri, no other file.
- Hunk arithmetic: `@@ -961,6 +961,46 @@` → 40 pure insertions at prompt.rs:961; `@@ -859,6 +859,67 @@` → 61 pure insertions at model_resolver.rs:859; trailing context `mod tests` on both.
- Each file has exactly ONE `#[cfg(test)] mod tests` (prompt.rs :874, model_resolver.rs :607 — sole cfg(test) sites per search). Each addition is one blank line + one complete test, inserted between pre-existing tests strictly inside that module: prompt.rs `workflow_stable_head_bytes_unchanged` :963-1002 sits between `preamble_carries_core_working_principles` (:917-962) and `stable_head_contains_universal_closing_rules` (:1005+); model_resolver.rs `workflow_policy_routing_table_unchanged` :861-921 sits between `routing_config_never_changes_the_resolve_chain` (:823-860) and `returns_none_when_no_overrides` (:924) — exactly the plan's insertion anchors (plan steps 1-2). Zero non-test bytes moved anywhere.

### (2) GOLDEN-VALUE HONESTY — VERIFIED
- The test computes `build_stable_head(&Constitution{...})` — the production head builder (graph: production callers `src/agent/loop_impl.rs::stable_head` :481 and `build_system_prompt` :582) — and digests the COMPUTED output; `GOLDEN_HEAD` is only the right-hand side of the assert_eq, so the constant cannot be self-fulfilling.
- Fixture is the step file's fixed Constitution verbatim (step file :86-87); both fields non-empty after trim, so both constitution sections land in the head (build_stable_head :515-525).
- `GOLDEN_HEAD` is well-formed: `len=12351|sha256=` + exactly 64 lowercase hex chars (counted: 32 byte-pairs), matching the left side's `format!("len={}|sha256={}", …)` byte-for-byte; both length and digest are byte-based (`head.len()` / `as_bytes()`), so the multi-byte em-dashes in the fixture/markers count consistently.
- Fill provenance: the commit message documents the fill loop (three runs per test: compile-fix + fill + green — the sanctioned deviation from the step file's two) and the green evidence — `cargo test --workspace` 3009 passed/0 failed, `-p mnemo` 2982, `-p mnemo --features browser,embeddings` 3009, all exit 0. These tests are the tree's only change since base 9c7015b, so the green run IS proof the frozen constants equal the untouched tree's output (the dispatch's pre-authorized verification route; as a read-only reviewer I have no shell, so the green runs are accepted on the worker's documented evidence — independently corroborated by (3), where all ten frozen routing winners re-derive by hand from the code).

### (3) ROUTING-MATRIX SEMANTICS — VERIFIED; ALL 10 CELLS RE-DERIVED FROM THE CODE
`resolve()` (model_resolver.rs :309-385) walks: arm 1 skill map (:316-323) → arm 2 subagent (:325-330) → arm 3 bug-fixing kind gate (:343-352; requires `Some(BugFixing)` AND state Executing|Reviewing) → arm 4 state slot (:364-384; `Reviewing => models.executing` :372, `Skill => None`, `Subagent => None`). Executing every fixture cell by hand against this chain:

1. (Planning, –, –, –) → arm 4 → **m-planning** ✓
2. (Executing, –, –, –) → arm 4 → **m-executing** ✓
3. (Reviewing, –, –, –) → arm 4's Reviewing→executing → **m-executing** ✓ (no reviewing state slot — the 2026-12-06 reviewer-spawn-only decision, comment :367-372)
4. (Complete, –, –, –) → arm 4 → **m-complete** ✓
5. (Subagent, –, sub, –) → arm 2 → **m-subagent** ✓ (arm 4's Subagent→None never reached)
6. (Skill, "new_release", –, –) → arm 1 skill HashMap hit → **m-skill-release** ✓
7. (Executing, –, –, BugFixing) → arm 3 engaged → **m-bug-fixing** ✓
8. (Executing, –, –, Research) → arm 3 not engaged (no research slot — `ModelsConfig` has no research field) → **m-executing** ✓
9. (Complete, –, –, BugFixing) → arm 3's `matches!` excludes Complete (plan finished) → **m-complete** ✓
10. (Executing, –, sub, BugFixing) → arm 2 precedes arm 3 → **m-subagent** ✓ (subagent outranks the kind's state override)

All ten match `GOLDEN_TABLE` cell-for-cell — an independent derivation corroborating the fill's honesty. Leak-check: `resolve()` never reads `models.reviewing`/`models.summarize` (sole consumers are `resolve_reviewer_model` :387-402 and `resolve_summarize_model` :404+, per the trait docs :112-127/:159-171 and the field docs general.rs :841-862); `m-reviewer-spawn`/`m-summarize` appear nowhere in the golden string, so the joined assert pins the no-leak property. The chain order matches the documented priority (general.rs :821-825). Fixture soundness: the "openai" endpoint is registered by `config_with_endpoints` (:615-640), and `Config::resolve_model_ref` (src/config/mod.rs :110-118) validates endpoint existence only — so the synthetic ids resolve cleanly, every arm validates, every winner is unambiguous, and no cell can fall to `<none>`.

### (4) sha2 USAGE — VERIFIED
`Sha256::digest(head.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>()` (:994-997) is a correct per-byte zero-padded lowercase hex fold → exactly 64 chars; the sanctioned sha2-0.11 drift fix (the digest's output implements no LowerHex), documented as deviation (1) in the commit message. No new dependency: `sha2 = "0.11"` pre-exists at Cargo.toml:83 (lib crate — the MCP PKCE dep), and the commit touches no manifest.

### (5) FIVE STRUCTURAL MARKERS — ALL PRESENT, VERBATIM, IN TODAY'S CONSTS
- `CORE PRINCIPLES:` — prompt.rs :30 (CODING_SYSTEM_PREAMBLE :26-55)
- `WORKFLOW LIFECYCLE — a plan-first state machine.` — :68 (WORKFLOW_LIFECYCLE :67-98; contiguous substring across the line continuation)
- `APP RULES — universal hard rules` — :114 (APP_RULES :113+)
- `# HARD RULES — GLOBAL (agent.md)` — :517 and `# HARD RULES — PROJECT (agent.md)` — :522 (build_stable_head's constitution pushes :515-525)

The test's marker array (:981-987) matches these five verbatim.

### (6) BUILD HEALTH — VERIFIED ON WORKER EVIDENCE
deny(warnings) at both crate roots makes the worker's green suite runs (all exit 0, counts in the commit message) prove the additions are warning-free; no `#[allow]` anywhere in the diff; imports are clean (`use sha2::{Digest, Sha256}` inside the test body, both used; the resolver test relies on the module's pre-existing `use std::collections::HashMap` :611 and `use super::*` for PlanKind/WorkflowState). Not re-run here (no shell as reviewer); the documented runs already cover every feature combination this diff touches — and the diff touches no feature-gated path anyway.

### (7) DOCUMENTATION SYNC — CLEAN
Add-only test step; the step file's file list (job-02a :70-73) prescribes no README/PLAN.md/endpoint-doc change and none is needed. Both tests carry doc comments naming their master-spec anchors (prompt.rs :966-969 → :913/:237-240; model_resolver.rs :864-870 → :914/:786-787), and the anchors check out against the master spec's :913-914 golden-test names and the :785-788 byte-identity acceptance these tests enforce.

### Constitution one-liners
- Documentation sync — clean (§7).
- Multi-platform neutrality — pure-Rust tests (String / SHA-256 / HashMap), no OS-specific surface; neutral by construction.
- File-tools-first — nothing to flag: the diff is two test-module appends; no shell-based mutation anywhere.
- Warning-free — proven via deny(warnings) + the worker's green suites; no `#[allow]` in the diff (§6).
- `.coding/**` bookkeeping — accurate: the commit's plan frame (31689c21.md) matches the shipped work step-for-step (all five steps' prescriptions match the actual diff, markers, fill, and verify evidence); the 64962938.md one-line touch is coordinator frame status consistent with 2a.0's execution (its own steps/artifacts all remain complete); the uncommitted diff is exactly the harness's round-1 stamp. Lockfile/generated noise: none in the delta.

### Known context acknowledged (not findings, per dispatch)
- Workspace clippy -D warnings red from pre-existing 1.95 drift — backlogged, out of scope; this commit's verify ran cargo build/test, not clippy.
- The fill loop's three runs per test (compile-fix + fill + green) vs the step file's "two runs max" — sanctioned, documented in the commit message as deviation (2).
- `PlanKind::Research` compiled as-is (variant exists) — documented as deviation (3); consistent with arm 3 gating on `Some(BugFixing)` only.

### Non-blocking observation (no action required)
The leak-check is enforced implicitly (a spawn-slot leak changes a winner → the joined-table assert fails) rather than via explicit `!winners.contains("m-reviewer-spawn")` asserts. The guarantee is equivalent today; explicit asserts would only sharpen a future 2a.6 re-fill's failure message. Fine to leave as-is in a characterization pin.
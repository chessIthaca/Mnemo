## Verdict: FINDINGS (0 high, 3 low)

Review of plan bb8a0ca9 (uncommitted diff: `src/tool/mod.rs` arm flips + regression test + `.coding/` knowledge bookkeeping). The fix is **correct and secure**: all three flipped arms verified, no other state gate for browser tools exists, approval flow untouched, mutating tools still NeedsApproval and still prompt in every prompting mode. The 3 low findings are stale doc comments / a mislabeled test comment that contradict the new behavior (documentation-sync rule).


## Scope reviewed

- `git diff HEAD`: `src/tool/mod.rs` (3 arm flips + test replacement), `.coding/knowledge/spec/2026-08-27-git-tool-structured-restore-subcommand-paths-sou.md` (marked superseded — correct hygiene for the new merged-in spec).
- Untracked: `.coding/knowledge/bug/2026-08-27-agent-can-t-drive-the-visible-browser-tab-in-com.md` (BUG knowledge file — root cause documented), `.coding/knowledge/spec/2026-08-27-git-tool-structured-restore-subcommand-merged-in.md`, `.coding/plans/bb8a0ca9.md`. All benign bookkeeping.

## Correctness — PASS

1. **Three arm flips verified.** `ToolCategory::Browser => true` at Planning (`src/tool/mod.rs:328`), Reviewing (`:403`), Complete (`:440`). Executing (`:362`) and ExecutingResearch (`:387`) were already `true` and are untouched by the diff. All five base filters now uniformly allow browser tools; new comments accurately explain the rationale (drives user-visible tab / headless pages, never project files; NeedsApproval is the guard).
2. **No other state gate for browser tools exists.** Repo-wide `ToolCategory::Browser` search: only the five arms, the tool `category()` impls, and the new test. `src/workflow/mod.rs::allowed_tools` (`:325`) maps state → `ToolFilter::from_state` / Skill / Reviewer allow-lists �� no browser-specific logic. `src/agent/dispatch.rs:93-112` re-enforces the *same* `wf.allowed_tools()` filter (the exact site that produced the user's error at `:105`) — consistent with the fix; no second, divergent gate.
3. **Skill and Reviewer arms untouched and still strict.** Skill (`:464-480`): browser tools only via the catch-all allow-list. Reviewer (`:481-491`): strict allow-list + `current_plan` only. The diff does not touch them; browser tools remain denied there unless explicitly listed. Reviewer contract (query, never mutate) and `write_review_report` authorship are unaffected — the filter is name-based and no `safety()` impl changed.
4. **Regression test exercises the changed path.** `browser_tools_available_in_every_base_state` (`src/tool/mod.rs:1276-1328`) calls `ToolFilter::allows` directly on the three changed arms (+ Executing) with the four live mutating tool names at `SafetyLevel::NeedsApproval`. Verified against the old arms it would have panicked pre-fix (Planning was `safety == AutoRun` → false; Reviewing was `false`). The old `reviewing_hides_browser_tools` is fully removed — no test anywhere still pins the old behavior (searched `hidden in Planning|stay hidden|hides browser|hidden during` — remaining hits are unrelated: progressive-disclosure inspection gate in `factory.rs:1424/1435`, Agent-category comment `mod.rs:292`, Skill/Reviewer list test `mod.rs:1702`).
5. **No crash paths** — pure boolean match-arm change plus test; no new unwraps/panics/unsafe.

## Security — PASS

- **Mutating browser tools remain NeedsApproval**: `safety_levels_are_set` (`src/tool/browser/mod.rs:455-470`) pins the offscreen mutations; live `browser_eval/navigate/click/type` are NeedsApproval per the factory registration (`src/agent/factory.rs:856-861`) and docs. The diff touches no `safety()` impl.
- **Approval flow unchanged and ordered after the filter check**: dispatch → workflow filter (`dispatch.rs:102`) → plan-mutation gate (`:121`) → `never_auto_for` force-prompt (`:241`) → `needs_approval` (`:243`) → safety-rules shortcut (`:249`).
- **No auto-approve hole opened**: `is_project_scoped` (`src/agent/approval.rs:124-`) classifies browser tools under "any other tool — false (conservative)", so under `AutoApproveProject` mutating browser calls still prompt; under `ApproveEachAction`/`AutoReadApproveWrites` they always prompt. `Autonomous` auto-runs everything by the mode's own contract — identical to Executing-state availability before this change, so this diff does not widen worst-case exposure; it only extends to Planning/Reviewing/Complete what Executing already had. (Browser tools do not override `never_auto_for` — a user-authored matching safety rule could still auto-approve one, pre-existing and unchanged, noted for completeness.)
- **`write_review_report` authorship / reviewer allow-lists**: unaffected (point 3).

## Findings

### L1 (low) — Stale module doc comment contradicts the new behavior
`src/tool/browser/mod.rs:10-11`: "Read tools are AutoRun + visible in every workflow state; **mutations are NeedsApproval + hidden in Planning/Complete**." — now false: mutations are visible in every state, gated only by the approval prompt. This is exactly the "doc comment that contradicts the new one" the plan asked to check for; the constitution's documentation-sync rule explicitly covers module doc comments. **Fix:** update the sentence to state mutations are visible in every workflow state and gated per-call by their NeedsApproval approval prompt.

### L2 (low) — Stale `register_browser_tools` doc comment, same contradiction
`src/agent/factory.rs:837-838`: "…mutations (navigate/close/eval/click/type) are NeedsApproval + **hidden in Planning/Complete by the ToolFilter**." — same falsehood. (The live-tool comment at `:856-866` makes no state claim — fine.) **Fix:** drop/replace the "hidden in Planning/Complete" clause.

### L3 (low) — New test comment mislabels `offscreen_browser_navigate` as read-only and asserts a wrong safety level
`src/tool/mod.rs:1303-1312`: the loop commented "The read-only browser tools stay available everywhere too" iterates `["browser_screenshot", "offscreen_browser_navigate"]` passing `SafetyLevel::AutoRun` — but `offscreen_browser_navigate` is a *mutation* (NeedsApproval, pinned by `safety_levels_are_set:460`). The assert passes only because the Browser arms now ignore `safety`; the label misleads future readers, and the loop also misses pinning the *offscreen* mutating tools at their true safety level in the four states. **Fix:** either swap in a genuinely read-only name (`offscreen_browser_list_pages`/`browser_snapshot`) for that loop, or relabel it "other browser tools" and additionally assert `offscreen_browser_navigate`/`offscreen_browser_eval` at `SafetyLevel::NeedsApproval` across the four filters (which strengthens the regression).

## Constitution / project checks

- **Documentation sync:** README.md and PLAN.md contain no browser-state-gating claims (verified by search) — nothing to update there; `.coding/browser-debugging.md` tables say only "mutations need approval", consistent with the new behavior. The stale module docs are findings L1/L2. *Pre-existing observation, not a finding against this diff:* README.md:110 still says "the headless `browser_*` tools work everywhere" and references `game_*` — stale from the earlier rename (should read `offscreen_browser_*` / `browser_*`); worth a follow-up backlog item.
- **Multi-platform neutrality:** pure filter-logic change in shared code — no new platform-specific code, paths, or shell syntax. Live `browser_*` tools remain `cfg(windows)` (the sanctioned WebView2 exception); the headless tools are uniformly available on both platforms, and the change applies identically.
- **Bug-plan checklist:** regression test exercises the changed path directly (verified fail-before/pass-after by arm inspection); root cause documented in the BUG memory (id `31103faa`) + `.coding/knowledge/bug/2026-08-27-…` + plan `bb8a0ca9`; full `cargo test` reported green (1564 passed) under `deny(warnings)` at both crate roots.

## Verdict summary

The behavioral change itself is correct, minimal, and does not weaken any approval gate. All three findings are comment/doc-level and must be fixed before commit (fix-every-finding rule): L1 + L2 are contradictions of the shipped behavior in module/factory docs; L3 is a mislabeled test comment. No high findings; no code-path changes required.

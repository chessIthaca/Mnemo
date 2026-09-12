# Review: Forgiving git action/subcommand resolution (plan 40220c86)

Scope reviewed: ALL uncommitted changes (`git status` / `git diff HEAD`) —
`src/tool/agent/git.rs`, `src/agent/approval.rs`, `src/agent/turn.rs`,
`src/safety_rules.rs`, `README.md`, plus `.coding/*` bookkeeping (not reviewed).

## Verdict

The core change is correct. I traced `resolve_git_subcommand` /
`resolve_git_action` against every consumer (`execute`, `never_auto_for`,
`is_git_read_only`, `is_durable_tool`, `key_argument`) and ~20 edge cases:
empty/missing fields, both fields set with conflicting values
(`{"subcommand":"pop","action":"delete"}` → branch delete per documented
branch-before-stash order), case variants (`MERGE`), and the `push`
ambiguity (`{"subcommand":"stash","action":"push"}` → stash push;
`{"action":"push"}` → the push subcommand). All match the documented
resolution order. The key invariant holds: every gating consumer uses the
same resolver as `execute`, so the operation that is approved/gated is the
operation that runs.

No correctness bugs, no security regressions found. Three Low findings.

## Security (verified, no findings)

- `is_git_read_only` (approval.rs:160-182) is never more permissive: the
  `has_extra_args` guard still runs first; mutating action names in either
  field (`{"action":"delete"}`, `{"subcommand":"create"}`,
  `{"subcommand":"pop"}`, `{"action":"push"}`) all resolve to branch/stash/
  push and return false (tested, approval.rs:431-446).
- `never_auto_for` (git.rs:385-406) resolves either field, so
  `{"action":"push"}` / `{"action":"merge"}` still force the prompt;
  `{"subcommand":"stash","action":"push"}` correctly does NOT (stash push is
  not a core op). Tested (git.rs:970-985). Dispatch still checks
  `never_auto_for` before safety rules (dispatch.rs:199-212) — unchanged.
- Read-args forwarding stays restricted to status/diff/log via the RESOLVED
  subcommand (git.rs:480-486); a swapped `{"action":"delete","args":["-f"]}`
  is rejected (test added, git.rs:1642).
- Safety-rule signatures (safety_rules.rs:428) normalize swapped calls to the
  canonical subcommand — the same signature the equivalent canonical call had
  before this change, so no rule becomes broader (e.g. `{"subcommand":"delete"}`
  now maps to `git:branch`, matching what `{"subcommand":"branch","action":"delete"}`
  always produced). A pre-existing stale rule saved as `git:delete` simply
  stops matching (less permissive, not more).

## Regression tests (verified, policy satisfied)

All three reported failure modes have tests that fail on the old code:
(1) `branch_action_branch_lists` (git.rs:1342); (2) `subcommand_delete_deletes_branch`,
`swapped_fields_branch_delete`, `action_only_branch_delete` (git.rs:1360-1437);
(3) `missing_subcommand_errors_with_valid_values` (git.rs:819). Error messages
listing valid values are asserted in `unknown_subcommand_errors` (git.rs:843)
and `branch_unknown_action_errors` (git.rs:1324). Resolver unit tests cover the
documented precedence rules (git.rs:675-817).

## Findings

### Low 1 — Frontend git ToolCard label doesn't fall back to `action`
`frontend/src/components/chat/Message.tsx:339-350` (`argLabel`): the git case
reads only `parsed.subcommand`. Calls that are newly legal — e.g.
`{"action":"commit","message":"x"}` or `{"action":"delete","branch":"x"}` —
render a bare "git" card with no verb, where the equivalent canonical call
shows "git (commit)". Cosmetic only (execution is unaffected), but the
forgiving API makes such calls more likely. Fix: fall back to `parsed.action`
when `subcommand` is absent/blank.

### Low 2 — Schema `subcommand.enum` contradicts the forgiving runtime
`src/tool/agent/git.rs:357-360`: the schema still constrains `subcommand` to
an `enum` of the 9 canonical names, while the field's own description (and the
runtime) say action names are accepted there (`subcommand="delete"`). Harmless
today for advisory-schema providers (Anthropic ignores it), but note
`ToolRegistry::schemas` sets `strict: Some(true)` whenever the provider
`supports_strict_schema` (src/tool/mod.rs:372-376): under strict constrained
decoding the model cannot emit the out-of-enum forgiving forms in the
`subcommand` field at all (the free-form `action` field still works). Either
drop the `enum`, extend it with the action names, or accept and document the
limitation.

### Low 3 — Stale doc bullet in `SafetyRules::signature`
`src/safety_rules.rs:389`: the doc list still says "- `git`: `subcommand`".
The key is now the *resolved* subcommand (either field, or a branch/stash
action name), as the new comment in `key_argument` itself explains. One-word
doc sync: "`git`: the resolved `subcommand`".

## Constitution compliance

- Doc comments on all new `pub(crate)` items: present and thorough
  (git.rs:74-83, 85-128, 130-174). ✓
- No new `#[allow(...)]`; no unused imports apparent (both new imports in
  approval.rs, turn.rs, safety_rules.rs are used). ✓
- Documentation sync: README bullet added and accurate; git.rs module doc
  paragraph accurate; PLAN.md does not document the git field API — nothing
  stale there. (Low 3 above is the only doc nit.)
- Multi-platform neutrality: no Windows-only APIs/paths/shell added; tests
  shell out to the `git` binary, portable. ✓
- Code style matches the file's existing conventions. ✓

## Verification caveat (not a finding)

I am read-only and could not run `cargo test`. The recorded log
`.coding/logs/cargo-test-review.log` predates this change (the new test names
do not appear in it), so a green run for this diff is not independently
confirmed — the closing agent must run `cargo test` (unpiped, read
`$LASTEXITCODE`) after addressing the findings, per policy. Static inspection
of the new tests shows they compile-cleanly use existing helpers
(`init_repo` leaves `main` + `feat` branches and a clean tree, matching what
`branch_action_branch_lists` and the stash tests assume).

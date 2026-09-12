## Verdict: PASS

Re-review verification of plan a41a0d82 "Graduated symbol-lookup redirect gate" (commit `66a8c10` on `wt/agenticcoding`). The prior review (`.coding/reviews/2026-12-04-c5-symbol-lookup-redirect-gate-review.md`) returned FINDINGS (0 high, 3 low). All three fixes are verified correct and complete. The working tree is clean (`git diff HEAD` empty); no code logic changed in this fix — only a comment, a file restore, and a doc note, so the 10 passing unit tests are unaffected conceptually.

---

## Fix verification

### LOW 1 — `observe_call` skip now explained (dispatch.rs:246-260) — FIXED ✓

The comment block above the C5 gate (dispatch.rs:246-260) now carries the missing explanation. The added sentences (lines 255-260):

> observe_call is also skipped: it unconditionally consumes the per-agent last_fired note, and since "search" is not a SearchNudge target, running it here would consume the note without counting a switch — leaving the agent's redirect-following graph_context call with no note to match, so the gate would never lift. Skipping it preserves the note so that graph_context call registers the switch.

**Accuracy verified against source.** `observe_call` (steering_stats.rs:427-441) executes `inner.last_fired.remove(&agent_id)` at line 429 *unconditionally* — it runs before the `if kind.targets().contains(&tool)` guard at line 432, so the note is consumed whether or not a switch is counted. SearchNudge's switch targets are the four `graph_*` tools (graph_search/graph_context/graph_impact/graph_path — per plan b30475ba's marker table and the C5 spec's own ShellTip example), not `"search"`. Therefore running `observe_call` on the intercepted search call would consume the SearchNudge note without incrementing `agent_switched[SearchNudge]`, and the agent's subsequent `graph_context` call (following the redirect) would find no note → no switch → the gate would never lift. The comment states exactly this. **Present, accurate, and correctly placed** (immediately above the gate code a maintainer would touch). ✓

### LOW 2 — Unrelated `backlog.jsonl` removal reverted — FIXED ✓

`git diff HEAD` returns completely empty (no stat, no diff, no untracked entries) — the working tree is clean. `backlog.jsonl` is not among the files touched by commit `66a8c10`, so the file matches its committed HEAD state and the unrelated removal of "done" item `18f3b86b` is no longer part of this change. The other "done" item (`c7383c50`) and all pending/failed items are intact. ✓

### LOW 3 — Intervening-marker edge case documented in C5 spec — FIXED ✓

The C5 spec (`.coding/knowledge/spec/2026-08-31-enforcement-ladder-c1-c5-alternation-absorption.md`) now carries the note, clearly labeled:

> KNOWN EDGE CASE (pre-existing C3 limitation, inherited by C5 — not a bug): the gate lifts on agent_switched[SearchNudge] > 0, which only increments when a graph-tool call IMMEDIATELY follows a SearchNudge-firing result (the per-agent last_fired note is consumed by the very next observe_call, whatever tool it is). If an actionable marker from a DIFFERENT kind fires between the two SearchNudge fires and the intercept — e.g. a shell grep that fires ShellTip (whose targets include graph_context) — then last_fired becomes ShellTip, and the agent's post-redirect graph_context call registers the switch against ShellTip, not SearchNudge. agent_switched[SearchNudge] stays 0, so the gate keeps firing on the next symbol search even though the agent did switch to graph tools. Conservative (errs toward more teeth) and narrow (requires an intervening actionable marker between the nudges and the intercept; the natural post-intercept flow is search→intercept→graph_context with nothing in between). The C5 gate deliberately reuses C3's switch semantics per the plan.

**Accuracy verified.** The mechanism described matches the code: `last_fired` is set in `observe_result` (steering_stats.rs:411-420, preferring the most recent actionable kind) and consumed unconditionally by the next `observe_call` (line 429). The ShellTip example is accurate — ShellTip's targets include `graph_context` (plan b30475ba marker table: `{search, search_read, graph_search, graph_context}`), so an intervening shell-grep that fires ShellTip would re-point `last_fired` to ShellTip, and the post-redirect `graph_context` switch would register against ShellTip, leaving `agent_switched[SearchNudge]` at 0. The note is **present, accurate, and clearly labeled as a pre-existing limitation (not a bug)**, and correctly characterizes the behavior as conservative and narrow. ✓

---

## Re-confirmation of the core implementation (unchanged)

The core C5 implementation was verified correct in the prior review and is unchanged by this fix commit (the diff touches only the comment text, the restored `backlog.jsonl`, and the spec doc note — no logic, no signatures, no test bodies):

- **Gate condition** — `should_gate_symbol_search` (steering_stats.rs:464-474) returns `Some(fired)` iff `agent_fired[SearchNudge] >= ESCALATION_THRESHOLD (2) && agent_switched[SearchNudge] == 0`; `symbol_search_redirect` (dispatch.rs:701-720) additionally requires `symbol_nudge(graph, pattern)` to resolve the pattern to an indexed symbol. Both must hold. ✓
- **Placement** — the gate (dispatch.rs:261-277) sits after the failed-reviewer gate (226-244) and before the approval gate (291+); it only short-circuits the search tool, never bypasses safety/approval/reviewer-failure. ✓
- **Singleton consistency** — the gate reads `SteeringStats::shared()` (dispatch.rs:271), the same instance the dispatch path's observers write to (dispatch.rs:433). ✓
- **`observe_call`/`observe_result` skip** — correct and load-bearing (now fully commented per LOW 1). ✓
- **Factory wiring** — `build_inner` calls `agent.with_codegraph(self.codegraph.clone())` (factory.rs:659); both `AgentLoop` constructors default `graph: None` (loop_impl.rs:519, 566). ✓
- **Platform neutrality** — pure Rust, cross-platform test fixtures (`tempfile::tempdir` + `std::fs::write`); no Windows-only APIs. ✓
- **Documentation sync** — PLAN.md and README.md updated to C5; new C5 spec supersedes C1-C4 (old spec marked `status = "superseded"`); DECISION + HOW knowledge records present. ✓

## Tests

No code logic changed in this fix — only a comment (dispatch.rs:255-260), a non-source file restore (`backlog.jsonl`), and a knowledge-doc note. Rust ignores comments, so the 10 unit tests (3 in `steering_stats.rs`, 7 in `dispatch.rs`) are behaviorally unaffected and remain green conceptually. The comment change cannot alter compilation or test outcomes. ✓

## Conclusion

All three prior low findings are correctly and completely addressed. No new findings. The C5 graduated symbol-lookup redirect gate is correctly implemented, documented, and tested.

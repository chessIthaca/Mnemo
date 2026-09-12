## Verdict: FINDINGS (0 high, 3 low)

Plan a41a0d82 "Graduated symbol-lookup redirect gate" — the C5 intercept is **correctly implemented, well-tested (10 unit tests across 2 files), well-documented, and platform-neutral**. The gate fires exactly when intended, is placed correctly in the dispatch funnel (after the failed-reviewer gate, before the approval gate), reads the same `SteeringStats::shared()` singleton the observers write to, and the decision to skip `observe_call`/`observe_result` on the intercepted call is not only correct but *load-bearing* for gate-lifting. No high-severity issues. Three low-severity findings below (one doc-comment gap, one unrelated file change, one pre-existing edge case).

---

## Correctness — PASS

**Gate fires exactly when intended.** `should_gate_symbol_search` (steering_stats.rs:464-474) returns `Some(fired)` iff `agent_fired[SearchNudge] >= ESCALATION_THRESHOLD (2) && agent_switched[SearchNudge] == 0`. `symbol_search_redirect` (dispatch.rs:701-720) additionally requires `symbol_nudge(graph, pattern)` to return `Some` — i.e. the pattern is a bare identifier / definition-prefixed / alternation that exactly names an indexed symbol. Both conditions must hold. ✓

**Correctly does NOT fire** in all the negative cases (each covered by a unit test): below threshold, after a switch, non-symbol pattern, no graph (`symbol_nudge` returns `None` via `graph.as_ref()?`), non-search tool (the `matches!(tool_name, ...)` guard at dispatch.rs:708, redundant with the caller's guard but correct defensive coding for direct test use).

**Placement is correct.** The gate (dispatch.rs:246-271) sits AFTER the failed-reviewer gate (226-244) and AFTER the `ask_user` interception (209-214), but BEFORE the approval gate (273+, `force_prompt`/`needs_approval`). So it cannot bypass safety rules, approval, or the reviewer-failure latch. It only short-circuits the search tool itself. ✓

**Singleton consistency — the critical check.** The gate reads `SteeringStats::shared()` (dispatch.rs:265). The normal dispatch path ALSO uses `SteeringStats::shared()` (dispatch.rs:433, `let steering = ...::shared()`). Same instance → the fired/switched counts the gate reads are exactly what `observe_result`/`observe_call` wrote. No stale-instance bug. ✓

**The `observe_call`/`observe_result` skip is correct AND load-bearing.** Both observers live in `dispatch_with_interrupt` (lines 434/441), which the early-return at dispatch.rs:268 never reaches. Skipping `observe_result` is right (the redirect embeds the nudge text containing `"is an indexed symbol"` = `SEARCH_NUDGE_MARK`, so calling `observe_result` on it would re-fire SearchNudge and inflate the count). Skipping `observe_call` is *essential*: `observe_call` (steering_stats.rs:429) does `inner.last_fired.remove(&agent_id)` unconditionally — it consumes the last-fired note whether or not a switch is counted. "search" is not a SearchNudge target (targets are `graph_*`), so if `observe_call` ran on the intercepted call it would consume the SearchNudge note *without* counting a switch, and the subsequent `graph_context` call would find no note → no switch → **the gate would never lift**. By skipping it, the note is preserved so the agent's `graph_context` call (following the redirect) registers the switch and lifts the gate. Verified by `symbol_search_redirect_lifts_after_switch`. ✓

**Thread-safety.** `SteeringStats::shared()` is a `OnceLock<Arc<SteeringStats>>` (steering_stats.rs:333-336) — thread-safe init. `should_gate_symbol_search` takes `self.lock()` which recovers from poisoning (341-346). Read-only query, no mutation. The TOCTOU window between the gate's read and the next `observe_call` is benign (worst case: one extra intercept after a switch just landed). ✓

**`symbol_nudge` called correctly.** `symbol_search_redirect` passes `&self.graph` (`&Option<Arc<CodeGraph>>`) and `pattern` (`&str`) to `symbol_nudge(graph: &Option<Arc<CodeGraph>>, pattern: &str)` (search.rs:565). Types match. ✓

**Factory wiring.** `build_inner` calls `agent.with_codegraph(self.codegraph.clone())` (factory.rs:659). `self.codegraph` is `Option<Arc<CodeGraph>>` (factory.rs:167); `AgentLoop::with_codegraph` takes `Option<Arc<CodeGraph>>` (loop_impl.rs). Both `AgentLoop` constructors default `graph: None` (loop_impl.rs:519, 566). Type-consistent; any missed constructor would fail to compile (Rust requires all fields initialized). ✓

**Return type.** `execute_tool_call -> (ToolResult, Vec<AgentCommand>)` (dispatch.rs:46). The gate returns `(redirect, Vec::new())` — `ToolResult::success(...)` + empty vec. Matches. ✓

## Bugs — none found

**False-positive surface (legitimate text search matching a symbol name):** `symbol_nudge` only returns `Some` for bare identifiers / definition-prefixed / alternation patterns that resolve to an indexed symbol (case-sensitive exact match via `graph.symbol_id`). A genuine text search for a word that happens to be a symbol name *would* trip the gate — but only after 2 ignored nudges, and the redirect message explicitly offers the escape hatch ("retry with a regex metacharacter or glob filter to disambiguate from a symbol lookup"). This is the intended teeth, not a bug. The gate never fires for regex patterns (metachars fail `is_bare_identifier`) or non-symbol words. ✓

**Redirect message clarity:** "SEARCH INTERCEPTED: the pattern 'X' names an indexed symbol. You have used search for symbol lookups {N} times this session without switching to the graph tools." + the embedded nudge with `graph_context(id="...")` + the escape-hatch instruction. Clear and actionable. ✓

## Security — no concerns

The gate is a read-only query on advisory counters + a synthetic `ToolResult::success`. It cannot be abused or bypassed unsafely: it only *redirects* (returns a success result), never executes code, never touches the filesystem, and the worst case is a redundant redirect. No untrusted input reaches a dangerous sink. ✓

## Documentation sync — PASS

- **PLAN.md** — steering section accurately describes the C5 graduated gate (advisory for 2 ignored fires → intercept; lifts on first graph-tool switch). File list includes `loop_impl.rs`. ✓
- **README.md** — steering enumeration updated to mention the intercept; "third time" → "second time" consistent with `ESCALATION_THRESHOLD = 2` (amended in plan c5cc9a84). ✓
- **New C5 spec** (`.coding/knowledge/spec/2026-08-31-...-c1-c5-...md`) — accurate, carries `supersedes = "2026-08-29-...-c1-c4-..."` frontmatter. ✓
- **Old C1-C4 spec** — `status = "superseded"` added to frontmatter. ✓
- **HOW** (`2026-08-31-search-intercepted-redirect-...md`) — accurate redirect guidance. ✓
- **DECISION** (`2026-08-31-searchnudge-graduated-gate-...md`) — accurately documents the "never gates" → graduated-gate policy shift with rationale. ✓
- All new functions have doc comments (`should_gate_symbol_search`, `symbol_search_redirect`, `with_codegraph`, `graph` field). ✓

## Multi-platform neutrality — PASS

All new code is pure Rust: no Windows-only APIs, paths, or shell syntax. Tests use `tempfile::tempdir()` + `std::fs::write` (cross-platform). The only sanctioned Windows-only surface (WebView2 Browser tab) is untouched. ✓

## Code style — PASS

Follows existing C3/C4 patterns. All new functions have doc comments. No `#[allow(...)]` to silence warnings. The redundant `matches!(tool_name, ...)` guard inside `symbol_search_redirect` is intentional defensive coding for direct test invocation (covered by `symbol_search_redirect_ignores_non_search_tools`). ✓

---

## Findings

### LOW 1 — Comment explains `observe_result` skip but not the load-bearing `observe_call` skip (dispatch.rs:246-254)

The comment at dispatch.rs:253 states "observe_result is never called (the fired count is not inflated by the redirect's own 'is an indexed symbol' text)" — but it does not explain *why* `observe_call` is also skipped. As analyzed above, the `observe_call` skip is **essential for gate-lifting**: `observe_call` unconditionally consumes the per-agent `last_fired` note (steering_stats.rs:429), and since "search" is not a SearchNudge target, running it on the intercept would consume the note without counting a switch — leaving the subsequent `graph_context` call with no note to match, so the gate would **never lift**. A future maintainer who reads "does NOT call observe_call" and assumes it's an oversight could reintroduce `observe_call` on the intercept path and silently break gate-lifting (the gate would fire forever). Recommend adding one sentence to the comment explaining that `observe_call` is skipped to preserve the `last_fired` note so the redirect-following `graph_context` call can register the switch that lifts the gate.

### LOW 2 — Unrelated `backlog.jsonl` change: one "done" item removed (`.coding/backlog.jsonl`)

The diff removes one line — backlog item `18f3b86b` ("Evaluate why the agent keeps stopping mid-task instead of chaining consecutive obvious steps", `status:"done"`, `note:null`) — while preserving the other "done" item (`c7383c50`) and all pending/failed items. This is unrelated to the C5 gate plan. It is *not* the full-wipe clobber symptom (memory 707580e9 — that wipes the file to empty), since only one item is removed and others survive. Still, the removal is unexplained. Verify it was intentional (e.g. a deliberate cleanup or a backlog-store prune) and not an accidental side-effect of the session. No code impact either way.

### LOW 3 — Gate-lifting can persist after a graph-tool switch when an intervening actionable marker fired (steering_stats.rs / dispatch.rs)

The gate lifts on `agent_switched[SearchNudge] > 0`, which only increments when a graph-tool call *immediately* follows a SearchNudge-firing result (the `last_fired` note is consumed by the very next `observe_call`, whatever tool it is). If an actionable marker from a *different* kind fires between the two SearchNudge fires and the intercept — e.g. a `shell` grep that fires `ShellTip` (whose targets *include* `graph_context`) — then `last_fired` becomes `ShellTip`, and the agent's post-redirect `graph_context` call registers the switch against `ShellTip`, not `SearchNudge`. `agent_switched[SearchNudge]` stays 0, so the gate keeps firing on the next symbol search even though the agent did switch to graph tools. This is a **pre-existing C3 limitation** (the C5 gate deliberately reuses C3's switch semantics, per the plan), not a regression, and the behavior is *conservative* (errs toward more teeth). It is narrow (requires an intervening actionable marker between the nudges and the intercept, and the natural post-intercept flow is search→intercept→graph_context with nothing in between). Consider noting this in the C5 spec so it isn't mistaken for a bug if observed in the wild.

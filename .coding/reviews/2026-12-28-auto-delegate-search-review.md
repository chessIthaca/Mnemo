## Verdict: FINDINGS (1 high, 3 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 7b77cf0c / backlog b804012f (auto-delegate symbol and memory searches inside `search`/`search_read`). Scope: the full `git diff HEAD` (11 files, +897/−90) plus the 4 untracked files in `git status`.

**Summary.** The core design holds up well under scrutiny: `resolve_exact` is genuinely exact-only (`GraphView::resolve` is exact-first and the `.find(|s| s.name == name)` filter enforces case-sensitive name equality; the `::` shortcut is safe because `GraphView::context` is a strict `by_id` lookup that can never fuzzy-match); the `Mutex<Option<DelegatedKey>>` handling is sound (no await under the lock, trivial critical sections, and the async-side read → blocking-side write ordering is benign under the sequential await); `merged_note` has exactly two call sites and both were updated with the delegation block riding first; the steering coherence works — delegated first lines carry both the `SEARCH_NUDGE_MARK` marker and `AUTO-DELEGATED` in both the fast-path and prepend forms, the escape repeat is fire-only, and the C5 gate (`fired >= threshold && switched == 0`) can never arm on delegation from a clean state (pinned by the new regression test). Docs (README, PLAN.md, module docs, schema descriptions) match the code, the knowledge-file amendments are coherent, and the new HOW correctly supersedes the 2026-08-31 one via frontmatter. Multi-platform neutral and no security concerns (patterns reach the store parameterized; everything else is output text).

The one real defect: **the escape hatch is dead for glob-narrowed symbol delegations** — the block promises an escape the code never grants on the prepend path.

---

## HIGH

### H1. Escape hatch never engages for glob-narrowed symbol delegations (search.rs + search_read.rs)

`symbol_delegation_block` (codegraph.rs) unconditionally bakes the escape line into every delegated block: `" (re-issue this exact search to get the plain file search instead):"`. But in both execute flows the `DelegatedKey` is recorded **only** on the fast-path return:

```rust
if let Some(block) = delegated {
    if memory_delegated || args.glob.is_none() {
        *delegation_state.lock().unwrap() = Some(key);   // only here
        return ToolResult::success(block);
    }
    prepended = Some(block);                              // key NOT recorded
}
```

(search.rs:1217–1224; the identical shape in search_read.rs.) For a symbol hunt **with** a glob — e.g. pattern `foo(` + glob `frontend/**/*.ts`, exactly the shape the test suite itself constructs — the block is prepended above the results, the key is never stored, and re-issuing the query computes `escape = state == Some(key)` → `false`, rebuilds the block, and prepends it again. The promised plain-file-search escape is unreachable; the model can ping the same prepended block indefinitely.

This contradicts the feature's own contract as written everywhere else: the block text, README.md ("re-issuing the SAME query (pattern+glob+literal) skips delegation and runs the plain file search"), PLAN.md, both module docs, both schema descriptions, and DECISION record clause (1) STICKY ESCAPE KEY. It is also untested — no test repeats a glob-narrowed delegation, which is why it slipped through.

**Fix (one line, either shape is consistent with the docs):** record the key on the prepend path too (move the state write above the `if memory_delegated || args.glob.is_none()` branch), or omit the escape line when the block will be prepended rather than fast-pathed. Recording the key is the better match: the escape repeat then runs the plain glob-narrowed search with the advisory nudge, exactly what the block promises, and the "sticky until a different query delegates" semantics are unchanged. Add the missing test: glob-narrowed delegate → immediate repeat → plain search results, no `AUTO-DELEGATED` block.

---

## LOW

### L1. C3 escalation can queue on a delegated answer (steering_stats.rs ordering)

In `observe_result` (steering_stats.rs:452–501) the C3 escalation loop reads `agent_switched` **before** the new auto-switch block (lines 490–501) increments it. Interleaving: ≥1 ignored advisory nudge (fired=1, switched=0), then a delegated answer → at the escalation check fired=2 ≥ `ESCALATION_THRESHOLD` && switched==0 → the one-shot "the 'SearchNudge' marker has fired 2 times this session without a switch" NOTE is queued — even though this very result IS the steering acted upon. The C5 intercept gate is fully protected (switched becomes 1 immediately after, and the new regression test pins two delegations from a clean state never gate); only the softer C3 NOTE leaks, and only in this narrow interleaving. Fix: move the auto-switch block above the C3 escalation loop so a delegation can never be counted as an ignored fire at check time.

### L2. `edge_summary` "+N more" is computed from the capped group, not the true total

`edge_summary` (codegraph.rs) derives `+N more` from `rows.len() - 3`, but `GraphView::context` caps each edge group at `CONTEXT_GROUP_CAP = 50` (query.rs:40). A symbol with 200 callers renders "+47 more" instead of "+197 more". `ContextView` already carries the true counts (`incoming_total`/`outgoing_total`) — use `total.saturating_sub(3)` (or the group length when under the cap) for the remainder. Display-only, but the number is the model's signal for "should I open the full 360° view".

### L3. Test-coverage gaps in the duplicated wiring

The delegation wiring is deliberately duplicated per tool, so each gap needs its own twin: (a) search_read's `memory_targeted_query_delegates_and_skips_the_read` tests only the delegate phase — the search.rs twin also tests the escape repeat, this one doesn't; (b) no test pins the "a different delegating query replaces the key" half of the stickiness contract (delegate A → delegate B → repeat A delegates again); (c) the glob-prepend escape repeat is untested (currently blocked by H1 — add it with the H1 fix).

---

## Verified-clean checklist

- **Exact-only delegation (design decision 1)** — holds. `resolve_exact` = `::`-form validated via strict `by_id` `context()`, else `resolve()` filtered to `s.name == name` (case-sensitive). Fuzzy-only alternations fall through to the advisory `alternation_nudge` as specified.
- **Marker coherence (decision 2)** — holds. Fast-path and prepend forms both carry `is an indexed symbol` + `AUTO-DELEGATED` on the first line; escape repeat is fire-only; memory delegation carries no SearchNudge marker (KnownMemoryHit stays uuid-only, as documented).
- **Advisory-nudge survival (decision 3)** — holds. `symbol_hunt_names` extraction is behavior-preserving for the nudge (the alternation-first reorder can't change outcomes: no pattern is simultaneously alternation-shaped and def-prefix/bare/intent-shaped, since every extracted name passes the `is_bare_identifier` gate). Byte-pinned sentence unchanged; assertions correctly relocated into the escape-repeat phase.
- **Fast-path/prepend split (decision 4)** — implemented as specified (memory always fast-paths; symbol fast-paths only without a glob) — but see H1 for the escape-key consequence on the prepend arm.
- **Best-effort fall-through (decision 5)** — holds: `view().ok()?`, `recall().ok()?`, empty-recall → `None`, total resolution miss → `None`; tested at execute level for the memory miss.
- **Mutex/state handling** — sound: no await while locked, trivial critical sections, no panic path inside the lock (no poisoning risk), sequential await makes the async-read/blocking-write ordering benign; parallel-call races degrade to "both delegate" (harmless).
- **`merged_note` call sites** — exactly two (search.rs:1243, search_read.rs:244), both updated; delegation rides first; doc comment updated.
- **Multi-platform neutrality** — clean: pure Rust string/SQLite logic, `std::sync::Mutex`, globs in forward-slash convention, no `cfg(windows)`, no platform-specific APIs or paths.
- **Security** — clean: patterns reach the memory store parameterized; interpolated pattern text lands only in tool-output strings; no shell, no path traversal surface.
- **Documentation sync** — README.md, PLAN.md, both module docs, both schema descriptions, and the four knowledge files all match the code (modulo H1, where the docs describe the intended contract the prepend arm breaks). The tools-array budget guardrail test still passes with the longer schema text.
- **Bookkeeping** — backlog.jsonl (ead0d61c → failed/superseded by b804012f, deee3b8f → done) is consistent with the memory records; the new DECISION + HOW files are coherent and the HOW's `supersedes` frontmatter pairs with the old file's `status = "superseded"`.
- **Commit-content note (informational, not a finding):** the untracked set includes `.coding/knowledge/decision/2026-12-28-tool-cards-are-frameless-by-design-state-via-tex.md`, which belongs to the already-committed frameless-tool-cards plan (51bbe72) — it should ride this commit deliberately (it's a legitimate repo file that was left uncommitted), alongside this plan's DECISION/HOW files and `.coding/plans/7b77cf0c.md`.

**Bottom line:** fix H1 (one-line state-write move + the missing escape-repeat test), and ideally L1–L3 while in the file; everything else is ready to commit.

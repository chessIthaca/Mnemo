# Full four-axis review — master synthesis (2026-09-08 round)

**Round:** 2026-09-08 · HEAD `afd527d` on `wt/agenticcoding` (round target `2bdedd7` — same lineage; the run-all batch landed mid-round) · four parallel read-only reviewer subagents, re-spawned once after the spawn-cancellation race and the UI-event-blackout app restarts disrupted the first attempt (both since fixed in the running app — see "Round disruption" below).

**Reports & verdicts:**

| Axis | Report | Verdict |
|---|---|---|
| Model pipeline | `.coding/reviews/2026-09-08-full-review-model-pipeline.md` | FINDINGS (0 high, 1 low) |
| Performance | `.coding/reviews/2026-09-08-full-review-performance.md` | FINDINGS (0 high, 2 low — 1 refuted on verification) |
| Memory system | `.coding/reviews/2026-09-08-full-review-memory.md` | FINDINGS (0 high, 4 low) |
| Code quality | `.coding/reviews/2026-09-08-full-review-quality.md` | FINDINGS (0 high, 4 low) |

**Total: 0 high, 11 low — one refuted on spot-verification, leaving 10 actionable lows.** Zero correctness / data-loss / security findings on any axis; the actionable set is robustness gaps, waste, and stale docs. The codebase is in the best shape of any full round so far.

## Per-axis summaries

**Model pipeline (0H/1L).** Request assembly, SSE streaming, tool-call parsing, error handling/failover, auto-compaction, context management, model routing, and run-all/parallel orchestration all verified solid: one fallback per `run_turn_attempt` with endpoint-only (never provider) stickiness; fill-rate-dial compaction with the re-compaction loop bound and prefix-cache byte-stability; incremental `TokenAccounting`; run-all guarded at every exit point (dispatch lock, landing lock, done-orphan guard, lanes-in-flight gate) with real-git regression tests. The one low: the 429-fallback viability check compares context *windows*, not the live conversation size — silently disabling failover for mixed-window configurations of the same model id (fails safe: one actionable turn failure, no sticky entry).

**Performance (0H/2L, one refuted).** Streaming/display pipeline, startup path, and indexing core in excellent shape: delta batching 16ms/64KiB on both sides of the IPC boundary, mtime fast-path indexing, background embedder/index loads, bounded state everywhere, no lock-across-await on hot paths, no unbounded structures. LOW-2 (actionable): the parallel run-all fill serializes per-item provisioning under `DISPATCH_LOCK` (worktree checkout + checkpoint + recall block + cold graph index per item, all inside the guard), and each lane pays a full cold code-graph parse. LOW-1 (refuted — see verification notes): the claimed non-source-file reads do not happen; the extension gate has existed since the original finish-gate commit.

**Memory system (0H/4L).** The most mature subsystem in the codebase: store (SQLite + FTS5 + cosine KNN, injection-safe FTS quoting, `recall` vs `recall_peek` bump/no-bump split), auto-recall (per-turn cache keyed on the latest user message, invalidated only on summarization), typed-record lifecycle (files-as-truth, slug-collision suffixing, supersede-reconciles-unchanged-predecessor invariant), indexer (content-hash drift, correctly-scoped removal detection so concurrent lanes cannot wipe each other), consolidation (RAII dedup flag, log-on-Err on every swallowed durable write), embedder (hash-first, ONNX always backgrounded), startup reconciliation, and run-all interactions (`recall_peek`, boundary-token escaping, never-fails dispatch) all verified against the code. Four lows: three stale-doc items and one worktree tool-result ambiguity.

**Code quality (0H/4L).** The committed tree is in the best shape of any round: every fresh feature (backlog position param + soft-delete, parallel run-all worktrees, boundary-token escaping, bug-fixing model slot, fill-rate compact gate, finish-gate re-index) meets or exceeds the project's documentation and test bars; zero `#[allow(...)]`, zero TODO/FIXME debris, doc-comment constitution held, prior rounds' fixes all verified landed. All four findings sit in the never-reviewed *uncommitted* delta: a residual ordering window in the new `has_ever_started` latch, a CWD-dependent diagnostic sink, a missing regression test for the listener-registration failure path, and two cosmetic nits.

## Consolidated findings (10 actionable + 1 refuted)

| # | Axis | Sev | Location | Symptom | Fix direction |
|---|---|---|---|---|---|
| Q1 | quality | LOW | `src/runtime/channels.rs:1048-1055` | `set_running(true)` latches `has_ever_started` BEFORE flipping `running` — between the two stores the handle reads "started + idle", exactly what the cleanup filter cancels (nanosecond-scale residual of the race the fix eliminates; Relaxed reordering can widen it on ARM64) | Swap the stores (`running` first, then the latch); optionally Release ordering or one atomic state word |
| Q2 | quality | LOW | `src-tauri/src/ipc/events.rs:1036-1060` | `diag_line`'s file sink resolves `.coding/logs/emit-diag.log` against the process CWD — absent in packaged builds (exe dir / `/`), exactly where the diagnostic is needed most | Anchor CWD-independently like the watchdog's hang reports (`std::env::temp_dir()`, `src-tauri/src/main.rs:545,629`) or the app data dir |
| Q3 | quality | LOW | `frontend/src/hooks/useAgentEvents.ts:244-266` | The listener-registration `.catch` (FATAL console.error + uiDiag — the one failure leaving the window with NO listener) has no regression test | Source-contract test asserting the registration promise carries a `.catch` reporting via console.error + uiDiag |
| Q4 | quality | LOW | `src-tauri/src/ipc/events.rs:1478`; `frontend/src/hooks/useAgentEvents.ts:144` | Cosmetics in fresh code: ~14-space run in a test assertion message; `AGENT_EVENT_LABEL` should be `AGENT_EVENT_CHANNEL` for cross-side diagnostic comparison | Collapse whitespace; rename |
| M1 | model-pipeline | LOW | `src/agent/loop_impl.rs:1701-1710` | 429-fallback viability rejects an alternate whenever `alt_window < current_window`, regardless of live tokens — a 429 on a 200k-window model never fails over to a 128k endpoint even at 20k live tokens | Thread the live token count; accept when `alt_window >= live_tokens + headroom` (about the alternate's summarize threshold); keep the window comparison as fallback |
| P2 | performance | LOW | `src-tauri/src/ipc/run_all.rs:3263-3268`, `:3540+`; `src-tauri/src/ipc/spawn.rs:140-185` | Parallel fill provisions items strictly one at a time under `DISPATCH_LOCK` (checkout + checkpoint + recall block + cold graph index per item); each lane also rebuilds the code graph from scratch | (a) snapshot/record all N selections under the lock, provision concurrently outside it; (b) seed each worktree's `codegraph.db` from the main tree's DB (content-hash keying makes it safe by construction) |
| Me1 | memory | LOW | `src-tauri/src/ipc/memory_debug.rs:225-238` | Doc comment claims `recall` bumps access counts and "there is no read-only recall path today" — the code below calls `recall_peek`, and production auto-recall uses `recall_peek` too | Rewrite the doc comment to describe the peek (read-only ranking preview) |
| Me2 | memory | LOW | `src-tauri/src/main.rs:1363-1364` | Cross-machine re-embed comment justifies itself with "the memory DB lives in .coding/ (committed to git)" — it is gitignored (`.gitignore:46`); the check itself is still valid (model swap, legacy fingerprints) | Update the rationale; drop the git-travel claim |
| Me3 | memory | LOW | `PLAN.md:234-238`; `README.md:42`, `:53` | Docs list six retired memory tools (`memory_recall`, `memory_list`, `plans_search`, `reviews_search`, `past_fixes`, `context_pack`) — all replaced by `memory_search` | Update to the current six-tool surface; note `memory_search`'s browse mode replaces `memory_list` |
| Me4 | memory | LOW | `src/agent/factory.rs:1140-1198` + `src/tool/memory/mod.rs:167-170` | Worktree (run-all lane) agents bind the MAIN-tree knowledge store (by design — shared corpus), but the tool result reports a path the lane's own file tools cannot read, inviting a not-found detour | One-line message change: say the record landed in the MAIN project tree when the agent's root differs from the store's root |
| P1 | performance | REFUTED | `src/codegraph/mod.rs:525-597` | Claim: the finish-gate escalation reads non-source files' full bytes. **False** — the extension gate (`mod.rs:546-555`, "Only source files carry symbol rows — skip everything else before reading") has existed since the original commit `3d13884`; the file matches HEAD | Not actionable. Surviving polish: the naive `bytes.windows().any()` scan is O(n*m) — `str::find` (Two-Way) on the remaining source files |

## Verification notes (main-agent spot checks)

Four claims were checked against the tree; three confirmed, one refuted:

- **Q1 CONFIRMED** — `channels.rs:1048-1055`: `has_ever_started.store(true)` executes before `running.store(true)`.
- **Me4 CONFIRMED** — `factory.rs:1140-1198`: `register_memory_tools` binds `self.knowledge` / `self.plans_dir` (factory-wide main-tree stores) for every agent, with no root-spec branch.
- **M1 CONFIRMED** — `loop_impl.rs:1701-1710`: `alt_cm.max_tokens() < self.context_manager.read()...max_tokens()` compares windows, not the live token count.
- **P1 REFUTED** — `codegraph/mod.rs:546-555` gates on `Lang::from_extension(...).is_some()` BEFORE `std::fs::read`; `git log -S "skip everything else"` dates the gate to `3d13884` (the original finish-gate commit, in the round's target tree), and `git diff HEAD` shows the file unmodified. The reviewer's premise (non-source files read) does not hold; per the plan's protocol the finding is skipped as factually wrong, with the `str::find` scan polish queued separately.

## Cross-cutting observations

1. **Convergent recommendation — codegraph seeding.** Two reviewers independently arrived at the same fix (performance LOW-2 lever b; model-pipeline proposal 3): seed each spawned run-all worktree's `codegraph.db` from the main tree's DB. Content-hash keying makes it safe by construction (unchanged files skip the tree-sitter parse; changed files just re-parse). It is the biggest single run-all win: cuts per-item provisioning from a full parse to a read+hash sweep, reduces dispatch latency, and stops early `graph_search` calls from falling back to tree walks.
2. **The uncommitted delta carries 4 of the 10 actionable lows** — all cheap, none blocking it from landing. The delta itself (spawn-cancellation fix + event-delivery hardening) is verified working in the running app and has three test layers; it should land with Q1-Q4 applied.
3. **Doc-staleness cluster (Me1-Me3)** — the only README/PLAN drift found this round; the memory report otherwise verified README's Memory section accurate against the implementation.
4. **Zero highs; findings are increasingly polish-shaped.** No correctness, data-loss, or security findings on any axis. The actionable set: robustness gaps (Q1, M1), throughput waste (P2), docs (Me1-3), and a message ambiguity (Me4).
5. **The round itself stress-tested the app.** The spawn-cancellation race (killed 3 of the 4 original reviewers; only model-pipeline survived) and the UI event blackout (total Rust-to-TS event loss from process start) were both diagnosed and fixed DURING this round — that work is the uncommitted delta. The re-spawned reviewers then ran to completion without incident, and the emit-diag log verified the full event chain live (forwarder, emit ok, listener registered, first event DELIVERED).

## Improvement roadmap

**Quick wins (a day or less each):**
- Land the uncommitted delta with Q1-Q4 applied (backlog item 1) — also closes the UI-blackout backlog item f20bff16.
- Memory docs batch Me1-Me3 (item 5) and lane message clarity Me4 + gist markers (item 6).
- Precise 429 viability M1 (item 3) — small, test-covered, unblocks failover for mixed-window configs.
- Codegraph scan polish: `str::find` + a re-parse cap note (item 12).

**Structural (multi-day):**
- Codegraph seeding (item 2) — biggest single win; convergent recommendation.
- Concurrent run-all provisioning (item 4) — removes serial lane startup; matters most for short items and high concurrency.
- Sticky-endpoint expiry (item 7) — a 429 currently pins traffic to the alternate for the whole session.
- Dedicated summarization model slot (item 8) — route unattended compaction summaries to a cheaper model.
- Memory UX polish (item 9) — tab list staleness, lane-written knowledge surfacing, auto-recall store-version counter.
- Decompose `run_all.rs`'s two largest functions (item 10) and split `indexer.rs` per-source indexers (item 11).

## Round disruption (context)

The round was disrupted twice: (1) the spawn-cancellation race — four reviewers spawned in one assistant message, three silently cancelled before their first turn (root cause: `cleanup_inactive_subagents` filtered on `!is_running()` alone, and a freshly spawned agent is `running == false` until its first `Started` event; fixed by the `has_ever_started` latch + filter change, with three test layers); (2) the UI event blackout — no Rust-to-TS events reached the frontend from process start while invokes kept working (fixed by the bounded buffering + registration surfacing + emit diagnostics in the same delta; verified live via `.coding/logs/emit-diag.log`). Both fixes are uncommitted on `wt/agenticcoding` and land via backlog item 1. The one-spawn-per-assistant-message protocol remains the safe play until the delta lands.

## Backlog items queued (12)

1. Land the spawn-cancellation + event-delivery delta with the four quality-review fixes (Q1-Q4) — **top of queue**; closes f20bff16.
2. Seed run-all worktree codegraphs from the main tree's DB (P2 lever b; convergent).
3. Precise 429-fallback viability via live token count (M1).
4. Concurrent run-all provisioning outside DISPATCH_LOCK (P2 lever a).
5. Memory docs batch (Me1-Me3).
6. Run-all lane memory-message clarity (Me4 + gist truncation marker + gist path pointer).
7. Sticky-endpoint expiry / recovery probe.
8. Dedicated summarization model slot.
9. Memory UX polish (tab staleness + lane-written knowledge surfacing + auto-recall store version).
10. Decompose run_all.rs's two largest resolution functions.
11. Split indexer.rs per-source indexers into submodules.
12. Codegraph scan polish (str::find + re-parse cap note) — the surviving half of refuted P1.
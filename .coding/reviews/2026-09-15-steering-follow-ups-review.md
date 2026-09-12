## Verdict: FINDINGS (1 high, 0 low)

Review of plan 0fa048a9 ("Steering follow-ups") — all uncommitted changes on wt/agenticcoder (13 modified + 5 new files). One high finding: a garbled, duplicated line in PLAN.md's steering bullet. Everything else in focus areas A–D verified correct.

---

## Finding 1 (HIGH) — PLAN.md steering bullet is corrupted: duplicated line + broken sentence

**File:** `PLAN.md` lines 107–113 (Workflow-tools section, steering bullet).

**What's wrong:** The bullet that should read `(recalled_context_block, passive recall_peek shared by create_plan and spawn_agent; agent/steering_stats.rs counts per marker kind how often the next call followed the nudge — detection is tool-scoped, so a content-bearing result from a tool that emits no marker can never fire a kind falsely — surfaced via the get_steering_stats IPC command in the Trace tab header)` currently reads:

```
107:   stay nudge-free. Rider-side steering lives in `tool/steering.rs`
108:   (`recalled_context_block`, passive `recall_peek` shared by `create_plan`
109:   often the next call followed the nudge — detection is tool-scoped, so a
110:   often the next call followed the nudge — detection is tool-scoped, so a
111:   content-bearing result from a tool that emits no marker can never fire a
112:   kind falsely — surfaced via the `get_steering_stats` IPC command in the
113:   Trace tab header).
```

Lines 109 and 110 are byte-identical (duplicated), and the clause `and spawn_agent; agent/steering_stats.rs counts per marker kind how` was dropped — the sentence now jumps from "shared by `create_plan`" straight into "often the next call followed the nudge". The result is ungrammatical and duplicated; the concept the bullet previously conveyed ("counts per marker kind how often the next call followed the nudge") is also lost.

**Fix:** restore the full sentence on one line, e.g.:
`(`recalled_context_block`, passive `recall_peek` shared by `create_plan` and `spawn_agent`; `agent/steering_stats.rs` counts per marker kind how often the next call followed the nudge — detection is tool-scoped, so a content-bearing result from a tool that emits no marker can never fire a kind falsely — surfaced via the `get_steering_stats` IPC command in the Trace tab header).`

(Deleting one of the two duplicate lines and re-adding the dropped clause.)

---

## Focus area A — detector-table ↔ nudge-site parity: PASS

All five markers match what the emitters actually emit today; each detect() gate's tool list matches the real emitters exactly, and each scoping rule (first-line / prefix / contains) was verified against the emitter source:

- **SearchNudge "is an indexed symbol"** — `src/tool/agent/search.rs:233-246` (`symbol_nudge`) + `with_note` (307-312, prepends `note: {n}\n\n{body}`) + `merged_note` (318-324, fallback+nudge joined "; ") — the note is ALWAYS line 1: both `search` result paths (index at 438, walk at 527) and all four `search_read` paths (210/232/267/293) route through `with_note`. The literal-fallback note merges into the same line-1 note. Truncation (`cap_tool_output`, agent/mod.rs:61-68, and read_files' `truncate_to_boundary`) cuts from the END, so the prepended note survives. When the note is absent, line 1 is the "N matches in M files" summary, which can never contain the marker — first-line scoping is sound, including the payload-echo case (tested in `search_nudge_is_first_line_scoped`). No other emitter of the phrase exists outside search.rs/search_read.rs.
- **ShellTip "TIP: for file-content search"** — `shell.rs:41-43` GREP_NUDGE, `insert_str(0, …)` at 261 — position 0, so always line 1; applied to both stdout-only and stdout+stderr combined output; no other tool prepends GREP_NUDGE. Test `shell_tip_is_first_line_scoped` covers the stdout-echo case.
- **GraphMiss "No symbols matched"** — emitted ONLY by `miss_hint` (`codegraph.rs:84-95`), used by all four graph tools (graph_search 170, graph_context 242, graph_impact 330, graph_path 407/417) exactly as the gate lists them; the hint is a JSON field inside pretty-printed output (`run_query`, 58-61). Crucially, `Symbol` (extract.rs:126-141) and `ContextView` (query.rs:45-57) serialize ONLY id/name/kind/file/start_line/end_line — no doc comments, no source text — so the whole-output `contains` scan cannot false-fire on payload echoes (even though steering_stats.rs's own doc comments now contain the phrase). Near-miss branch uses different text ("no exact symbol; try one of the candidates' ids") so it never fires.
- **ReadNudge "SYMBOL NUDGE:"** — `read_files.rs:265` single leading prefix `format!("SYMBOL NUDGE: {}\n\n{output}", …)`; `starts_with` is correct; one note with "; "-joined clauses (marker appears exactly once — pinned by existing test at 815). Truncation cuts the tail, prefix survives.
- **RecallRider "RECALLED CONTEXT"** — `steering.rs:142-147` header, appended to the plan summary; `contains` correct (not necessarily line 1). Gate restricted to `create_plan` results — correct: spawn_agent's rider goes into the spawned TASK (first prompt), not the tool result, so counting it there would be wrong; the test `recall_rider_fires_but_never_switches` still passes with the create_plan-scoped gate.
- **Tool-name strings** match registration exactly (`src/tool/mod.rs`: graph_context/graph_impact/graph_path/graph_search, search, search_read, read_files, shell, create_plan). Both dispatch arms pass `parsed_call.name` (dispatch.rs:408, 432); no other observe_result call sites exist outside tests.
- Detector tests: `each_kind_fires_only_from_its_own_tool` (wrong-tool + foreign-marker-on-own-tool matrices), three scoping tests, and all prior switch/consume tests updated to the 3-arg signature — good coverage of the cross-tool misfire class that round-2's review flagged.

## Focus area B — age-gate semantics: PASS

- `steering.rs:101-102`: `min_created = now - 14*86_400`; `hits.retain(|h| h.memory.created_at >= min_created)` — **>= boundary** semantics, exactly as specified; boundary test keeps `BOUNDARY` and drops `BOUNDARY - 1`.
- **Bug-pass exemption placement**: the retain runs at line 102, BEFORE the `record_type: Bug` pass appends its hits (108-122) — structurally exempt, no special-casing; test `stale_bug_hit_still_rides_from_the_bug_pass` proves a month-old BUG record rides while a stale broad hit alongside it is dropped.
- **plan.rs `create_plan_rides_recalled_context`** (1518-1565): unchanged, stays green — the 2001 Semantic-tier seed titled "BUG: …" is record_type `Bug` (derived from title prefix, `MemoryRecordType::from_title`, mod.rs:1243-1245 — the prefix wins), so it is dropped by the broad pass's age gate and rides ONLY via the bug pass; the assertions (RECALLED CONTEXT + "BUG: kettle crashes" + passive access_count==0) verify exactly that path. `create_plan_without_store_has_no_rider` unchanged.
- **spawn_agent seed** (1009-1023): updated `1_000_000_000` → `SystemTime::now()` with a comment documenting the deliberate change; test still asserts the rider rides and access_count stays 0.
- **No regression on the without-store path**: `task_unchanged_without_store_or_hits` (a) no store → task byte-identical, (b) disjoint-vocab stale seed → no rider, task unchanged; both assertions intact.
- `now_secs()` fallback `unwrap_or(0)` is safe (pre-1970 → gate min_created negative → everything retained). `recalled_context_block_at` test-injection shape is clean; production wrapper test uses the wall clock.

## Focus area C — frontend: PASS

- **No loss of information**: the change is purely additive — MemoryEntryCard never displayed raw output (memory entries render the compact activity card instead of the ToolCard; raw output lives in the ToolCard expansion for tool entries at Message.tsx:869), and the parsed hits only extend what the card can show. The card's expanded list renders tier + title + per-hit score (`toFixed(2)`) for every parsed hit; the gate is now `hits.length > 0` (Message.tsx:126) instead of the auto-recall-only gate.
- **Regex correctness against the real line shape**: the emitter is `retrieval.rs:43` — `"[{}] {} (id: {}, score: {:.2}, strength: {:.2}){}"` with an optional `[superseded]` AFTER the closing paren — exactly the shape the unit-test fixture uses. The global regex `^\[(\w+)\]\s+(.+?)\s+\((?:id: [^,)]+, )?score: (\d+(?:\.\d+)?)` matches: tier = \w+ (all four tiers qualify), title = non-greedy up to the id-group's paren (titles containing inner parens like "…trust + health (5a1eb5b1) — MERGED into main (6ed4593a)" still capture the full title because the optional `id: ` group must follow the paren, verified by trace), `{:.2}` score always matches `\d+(?:\.\d+)?`, `[superseded]` sits after the matched region, and the optional id-group covers legacy `(score: …)` lines. Header parse: `"N memories matched:"` → N, `"no memories matched the query"` → 0 (mod.rs:422/424) — both covered by tests, including `no matches` chip rendering.
- **Reducer/type wiring**: `parseMemoryEntry` returns the widened type (types.ts now `score?: number` on hits); `reduceToolResult` attaches `hits: hits?.length ? hits : undefined` so non-search memory tools stay non-expandable (test asserts `hits` undefined for memory_write); zero-hit searches get `[]` → card non-expandable.
- **Source-contract test**: `new URL(rel, import.meta.url)` from `src/lib/toolCardPaths.test.ts` resolves `../components/chat/Message.tsx` and `../hooks/agentEventReducer.ts` correctly (both exist); it runs in the same node-env suite (readFileSync already exercised by the CSS-contract pattern the comment cites) — and the plan's `npm test` run (678 passed) confirms it executes and passes.

## Focus area D — project checks: 1 finding (docs)

- **README.md**: steering bullet updated accurately — tool-scoped detection ("a content-bearing result from a tool that emits no marker can never fire a kind falsely"), 14-day gate with bug exemption, and the expandable memory-search cards clause all match the code.
- **PLAN.md**: workflow-tools bullet (age gate + bug exemption wording) matches the code; the steering bullet is corrupted (finding 1, above).
- **Multi-platform neutrality**: no platform-specific APIs — the only new time source is `std::time::SystemTime` (portable); no cfg(windows), no path/shell syntax in the changed code.
- **No `#[allow(...)]`** anywhere in the diff; the build is warning-free by construction (`#![deny(warnings)]` and the claimed green `cargo test`).
- **Doc comments**: all new items documented — `MarkerKind::detect` (per-kind rule doc), `SEARCH_NUDGE_MARK` (public-output-contract note), `now_secs`, `recalled_context_block_at`, `STEERING_MAX_AGE_DAYS`; module docs in steering.rs and steering_stats.rs updated to describe the gate and the scoping.
- **.coding/ records**: the DECISION record delivers the plan's deliverable (do NOT de-emphasize explicit memory_search yet; measure with get_steering_stats for ~2 weeks; age gate is the "cheap half"; concrete comparison plan); the new SPEC supersedes the old nudge spec (old file marked `status = "superseded"`, matching memory record 1c73bfdc); the two HOW records log the session's search-usage tally as instructed; the plan file and the new backlog row (8863159f, pending) are present.

## Closing

Verified evidence base: full `git_diff HEAD` (all 13 M + 5 ?? files), emitter sources (search.rs / search_read.rs / shell.rs / codegraph.rs / read_files.rs / steering.rs / retrieval.rs / mod.rs / plan.rs / spawn_agent.rs / dispatch.rs / steering_stats.rs), frontend sources (Message.tsx / agentEventReducer.ts / types.ts / toolCardPaths.test.ts), docs (README.md / PLAN.md), and the new knowledge files. Claimed test results (cargo test 1656 passed, npm test 678 passed, both builds clean) are consistent with the code as read; the reviewer did not re-run them.

One high finding to fix: the PLAN.md duplicated/broken steering bullet (finding 1). After the fix, the change is ready to commit.

# Review: `fix/memory-write-message` — humanize memory_write confirmation

Date: 2026-08-20 · Branch: `fix/memory-write-message` · Reviewer: spawned reviewer (read-only)

**Scope reviewed:** all uncommitted changes vs HEAD — `src/tool/memory/mod.rs` (new `tier_summary` helper, human-readable write message + structured `data`, 2 new tests), `frontend/src/hooks/agentEventReducer.ts` (new-format parser + legacy fallback), `frontend/src/hooks/agentEventReducer.memory.test.ts` (new + legacy pins), `.coding/` bookkeeping churn (backlog prune, stack.json swap, new plan file — app-generated, not reviewed for logic).

**Verification basis:** full diff via `git diff HEAD`, read of every touched code path plus all consumers traced (turn.rs tool-message loop, channels.rs event serialization, Message.tsx ToolCard/MemoryEntryCard rendering, GraphView access log). Tests were reported green by the spawner (cargo 1129/0/1 ignored, zero warnings; frontend 346 tests; `npm run build` exit 0); this reviewer has no shell and did not re-run them — findings below are from code reading, not re-execution.

## Findings

### Low — stale function-level doc comment on `parseMemoryEntry` (doc accuracy)

`frontend/src/hooks/agentEventReducer.ts:241-247` — the function's doc comment still reads:

> "For `memory_write`, the tier + title come from the call's args (the model supplied them); for `memory_recall`/`memory_consolidate`, they come from the result output… The snippet is a truncated content excerpt in all cases."

This is wrong for the code the diff just landed, and it was already wrong before: `parseMemoryEntry(name, result)` has **no access to the call's args** — the tier (and now the title) for `memory_write` are parsed from `result.output` (new format first, legacy fallback second). Also "truncated content excerpt in all cases" is inaccurate for `memory_write`: the snippet is a 160-char slice of the confirmation *message*, not the memory content (true of the old code too, but this change is the natural moment to fix it). The diff updated the inline comments (:254-265) but left the function-level doc contradicting them.

**Fix:** rewrite the doc comment to: for `memory_write`, tier + title parse from the result output (new `Saved {phrase} "title" ({tier} memory)` format with a legacy `wrote {tier} memory (id: …)` fallback; title unavailable in the legacy format); snippet is a 160-char slice of the result output for write/consolidate, the recalled content line for recall.

### Low — titles containing double quotes (or a tier-phrase lookalike) degrade frontend parsing (cosmetic, model-controlled)

- `src/tool/memory/mod.rs:117-121` embeds the title **verbatim** between double quotes with no escaping. A title like `say "hi" now` produces `Saved a durable fact "say "hi" now" (semantic memory) — …`.
- `frontend/src/hooks/agentEventReducer.ts:260` parses the title with `/"([^"]+)"/` — a **first-match** of the first quoted run. For the example above it captures `say ` (trailing space), so the memory entry's title span renders a partial fragment.
- Same class of issue at `:257`: the tier regex `/\((working|episodic|semantic|procedural) memory\)/i` matches anywhere in the string. A title that itself contains the literal text `(procedural memory)` **before** the real marker makes the badge show the wrong tier (first match wins).

Bounded impact: this is display-only, in an opt-in debug line (Settings → "Show memory activity"), tier/title/snippet all render React-escaped, and the snippet still shows the full sentence. But it is exactly the "format-string edge case" the plan asked to sanity-check, and the failure mode is silent mislabeling.

**Fix (either side, ideally both):**
- Rust: escape inner double quotes in the title when interpolating (e.g. replace `"` with `'` or `\"`) — makes the frontend regex correct by construction; or
- Frontend: parse both fields with one boundary-anchored regex that binds the title to the closing tier marker and the tier marker to end-of-string-or-` — `, e.g. `/Saved [^"]*?"(.+)" \((working|episodic|semantic|procedural) memory\)(?: — |$)/i` — a fake tier phrase inside the title can't satisfy the `(?: — |$)` lookahead position and inner quotes are absorbed by the greedy `(.+)" \(` boundary.

Add a pin test in `agentEventReducer.memory.test.ts` for a quote-containing title (would fail today, passes with the fix).

## Informational (verified, no action required — recorded so these don't get re-litigated)

1. **No UUID re-leak via `ToolResult.data`.** The full result (incl. `data`) is serialized to the frontend (`src/runtime/channels.rs:442-452`; `frontend/src/lib/types.ts:59` `data?: unknown`), but **no frontend code reads `.data`** and no render path stringifies the whole result: `CallDetail` renders only `call.result.output` (`Message.tsx:598-600`), `MemoryEntryCard` renders the parsed tier/title/snippet (`Message.tsx:105-115`). The LLM also sees only `result.output` — the tool message is built from `result.output.clone()` alone (`src/agent/turn.rs:1283-1291`); `data` is never fed back to the model. The id is genuinely machine-only, matching the code comment's claim.
2. **`data.id` currently has zero consumers.** Unlike `write_review_report`'s data payload (consumed at `turn.rs:1242-1251`), nothing reads the memory write's id. The code comment documents this as deliberate (recall is query-based); harmless future-proofing. Not a defect.
3. **No other consumers of the legacy `"wrote {tier} memory"` string.** Project-wide search finds it only in the intentional legacy-fallback test (`agentEventReducer.memory.test.ts:44,48`), the new Rust test's doc comment, and `.coding/` bookkeeping text. The two parser branches are provably disjoint: legacy output `wrote semantic memory (id: …)` cannot match the new tier regex, which requires the literal `(semantic memory)` parenthetical.
4. **No injection surface.** The title is model-controlled text embedded in the message, but every render path is React text interpolation (auto-escaped) — no `dangerouslySetInnerHTML` anywhere in `frontend/src`, and no markdown rendering of tool output in these paths. The model is also the source of the title, so no privilege boundary is crossed on the LLM side either.
5. **Unbounded title length in the message.** No Rust-side truncation (`MemoryWriteArgs` / `Memory::new` cap nothing); the schema says "A short label" but doesn't enforce it. Display is bounded (160-char snippet slice, CSS `truncate` on the title span) and the model only inflates its own context, consistent with the pre-existing `memory_recall` output embedding titles verbatim. If desired, a `chars().take(N)` + ellipsis in the message would be cheap defense-in-depth — optional.
6. **CRLF warning on `src/tool/memory/mod.rs`** (`CRLF will be replaced by LF the next time Git touches it`): the edit introduced CRLF into the working copy; `git add` normalizes to LF on checkin (the diff already compares clean, no `^M` artifacts). The committed file stays LF-consistent with HEAD. No action; do not bypass normalization when committing.
7. **`write_message_covers_every_tier` iterates a fixed 4-tuple list** — a future fifth tier compiles (`tier_summary`'s exhaustive match forces an update there) but this test wouldn't notice. Nit only.

## Verified correct (spot-checks)

- `title.clone()` borrow flow: `let title = args.title;` → clone into `Memory::new`, original used in `format!` then moved into `json!` (last use) — correct, and pinned by `data["title"] == "auth fact"`.
- Episodic empty-suffix path: `if !suffix.is_empty()` gate → `Saved a session summary "T" (episodic memory)` with no dangling ` — `. Longest message (working tier + short title) fits the 160-char snippet slice; longer titles truncate mid-suffix — cosmetic only.
- Tier word comes from `MemoryTier::as_str()` (`src/memory/types.rs:26-33`), lowercase, matching the frontend regex — no Display/`as_str` mismatch.
- **Regression-test quality — the tests genuinely fail against the old code:** old output `wrote semantic memory (id: …)` contains neither `Saved a durable fact "auth fact"` nor `data` (old code returned `data: None`, so `result.data.expect(...)` panics) → both Rust tests fail; the new-format TS pin fails against the old parser (legacy regex doesn't match the new string → `tier` undefined). Legacy pin retains the old string and asserts `title` stays undefined — fallback is itself pinned.
- **Constitution:** doc comment present on `tier_summary` (`mod.rs:137-142`) and on both new tests; no `#[allow(...)]` introduced anywhere; regression tests present on both sides of the boundary (Rust message + TS parser); tests reported green with zero warnings under `#![deny(warnings)]` (spawner-verified).
- Tool schema is not invalidated by the change (args unchanged); optional polish: the `title` description (`mod.rs:82`, "used in recall listings") could now also mention the write confirmation, but it is not inaccurate.
- `GraphView`'s memory-access log (`formatAccessLine`, `GraphView.tsx:118-123`) renders store-driven `MemoryAccessEntry` fields, not the tool message — unaffected.
- `.coding/` churn (backlog item 62 → in_flight, done-items 53-57 pruned, 60/61 → done, stack.json top-of-stack swap, new plan file) is app-generated bookkeeping consistent with the workflow; no logic to review.

## Verdict

The change does what the backlog asked, cleanly, with genuine regression pins on both sides of the Rust/TS boundary, and the headline risk (UUID re-leak via `data`) is confirmed absent. Two **Low** findings should be fixed before commit (stale doc comment on `parseMemoryEntry`; quote-in-title parsing degradation + pin test); everything else is informational.

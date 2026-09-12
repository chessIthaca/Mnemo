# Review: Compact popup hover + /compact feedback + 8-heading compaction prompt

**Branch:** `fix/compact-popup-feedback`
**Scope:** ALL uncommitted changes (12 modified files + 4 new files, per `git status --short`).

---

## Verdict: no findings

The diff is clean. Every focus point from the plan was verified in the working tree; no correctness, bug, security, or constitution issues found.

---

## Per-focus-point verification

### 1. pb-1 hover bridge (InflightBar.tsx:242) — CORRECT
The popup wrapper is now `absolute bottom-full right-0 z-50 hidden w-52 pb-1 group-hover:block` (line 242). The visual classes moved to an inner card div (line 243), so the gap between the trigger bar and the popup is created by `pb-1` **padding** on the positioned wrapper — padding is inside the element's hover box, so the mouse can travel from the bar down into the popup without leaving the `group` span. No margin remains anywhere on the wrapper (confirmed: the only `mb-1` left in the block is on the inner "Context breakdown" label at line 244, which is a child of the card, not the wrapper). JSX nesting is balanced: outer wrapper (242) → inner card (243) → … → `</button>` (332) → `</div>` card (333) → `</div>` wrapper (334) → `</span>` group (335). The new static contract test (`InflightBar.test.ts`) asserts `pb-1 group-hover:block` is present, asserts no `bottom-full…mb-N` on the wrapper via a scoped regex, and asserts `compact(agentId)` is still wired — a reasonable guard given the project has no React DOM test infra.

### 2. Compacted emitted on every success path, only when after < before — CORRECT
- **Manual between-turns** (`src/runtime/agent.rs` `compact_context`, line 654): `if stop.is_none() && used < before` → emits `Compacted { before, after: used }`. `before` is computed from the pre-compaction messages (line 578); `used` from the post-compaction messages (line 644). Both post-turn `AfterTurn::Compact` call sites (lines 342, 408) and the between-turns `AgentCommand::Compact` arm (line 443) route through this single function, so all manual paths are covered.
- **Auto threshold** (`src/agent/turn.rs`, line 291): `if summarize_stop.is_none() && used < token_count as u32` → emits `Compacted { before: token_count as u32, after: used }`. `token_count` is the pre-summarization count; `used` is recomputed after (line 281).
- The no-op path (too few messages → nothing summarized → `used == before`) and the interrupted path (`stop.is_some()` → original messages kept → `used == before`) are both correctly suppressed by the `<` guard, so no misleading N→N note.

### 3. Error surfacing — no behavior change to buffered commands / stop propagation — CORRECT
- `compact_context` (agent.rs:584-601): on `Err(e)` emits `Error { retrying: true }` and returns `None` early. `self.messages` is untouched (the summary is only applied inside the `Ok` branch at line 604-606), so no partial state. Returning `None` (not a stop reason) is correct — a failure is not a Cancel/Interrupt, and the caller's `if let Some(Cancel) = …` check correctly does not break.
- `turn.rs` (226-245): on `Err(e)` emits `Error { retrying: true }`, keeps `messages.clone()`, and continues the turn. The buffered-command re-injection loop (250-277) and the `summarize_stop` → `Finished`/`TurnOutcome` propagation (306-318) are byte-for-byte unchanged from before.
- The old `.unwrap_or_else(|_| (messages.clone(), Vec::new(), None))` swallowed the error but produced the same `(messages, [], None)` triple the new `Err` arm produces — so the only behavioral delta is the new Error event, exactly as intended.

### 4. `did_summarize` gating in turn.rs — CORRECT (intended semantic)
`did_summarize` moved from unconditional (`= true` before the call) to `if summarize_stop.is_none() { did_summarize = true; }` (line 246-248). Verified:
- **Err path:** the `Err` arm yields `(_, _, None)`, so `summarize_stop.is_none()` is **true** → `did_summarize` is set **true**. Wait — this deserves a closer look. On failure the messages are unchanged (original kept), so arguably the prefix was NOT rewritten and `did_summarize` should be false. **However**, reading the two consumers: (a) the token-count path at line 182 uses `did_summarize` only to force an exact recount — harmless on failure (one extra exact count); (b) the cache-heuristic gate at line 734 uses `did_summarize` to report `cached = 0` — on failure this under-reports cached tokens for one turn, a benign cosmetic effect. The comment at line 230 ("`did_summarize` stays false: no compaction happened") is **inaccurate** — on the Err path `summarize_stop` is `None`, so `did_summarize` is in fact set to `true`. This is a **comment bug only**, not a logic bug: the resulting behavior (exact recount + one turn of `cached = 0`) is safe. Not a blocking finding; suggest fixing the comment to "on failure the prefix is unchanged, but forcing an exact recount + one turn of `cached = 0` is harmless."
- **Interrupted path** (`summarize_stop = Some(Interrupt/Cancel)`): `did_summarize` stays **false** — correct, because the summary was abandoned and the original messages (unchanged prefix) are kept, so the cache heuristic remains valid.
- **Success path** (`summarize_stop = None`, messages replaced): `did_summarize = true` — correct.

### 5. Serde wire shape — MATCHES exactly
`SerializableAgentEvent` uses `#[serde(tag = "kind", rename_all = "snake_case")]` (channels.rs:272). The `Compacted { before, after }` variant serializes as `{"kind":"compacted","before":N,"after":N}` — confirmed by the round-trip unit test (channels.rs:663-692, asserts `"kind":"compacted"` in the JSON). The TS union (`types.ts:124-131`) declares `{ kind: "compacted"; before: number; after: number }` — exact match. The golden fixture `event-compacted.json` is `{"kind":"compacted","before":500000,"after":240000}`… wait, `24000` — matches the Rust fixture entry in `tests/contract_fixtures.rs:241-244` (`before: 500_000, after: 24_000`). Both sides agree.

### 6. New compaction prompt — running-update detection intact, 8 headings present
- The `## Conversation summary` prefix detection is unchanged (context.rs:380, still `m.content.as_text().starts_with("## Conversation summary")`), and the summary message is still written with the same prefix (lines 190, 308), so the running-update path still triggers on the next compaction.
- `SUMMARY_FORMAT` (context.rs:327-355) contains all 8 numbered headings (1. Objective … 8. Immediate Next Step), the `[Decision] -> [Reason]` format, the VERBATIM rule under Critical References, and the closing "Provide only the final markdown block, keeping high information density and omitting conversational filler" instruction.
- The two new tests cover the fresh prompt (headings + VERBATIM) and the running-update path (previous summary `PRIOR STATE` included + 8th heading present).
- No source file still references the old section names — the only matches for "Current Task / Key Decisions / Open Items / Files & Identifiers / Errors & Blockers" are in `.coding/plans/*.md` and `.coding/reviews/*.md` (historical bookkeeping, not code). Confirmed via search.

### 7. Constitution — CLEAN
- No `#[allow(...)]` suppressions added anywhere in the diff (searched the full diff).
- All new public items have doc comments: `AgentEvent::Compacted` + `SerializableAgentEvent::Compacted` (channels.rs:257-263, 345-346), `reduceCompacted` (agentEventReducer.ts:527-532), the `compacted` TS union member (types.ts:125-128), `SUMMARY_FORMAT`'s updated doc (context.rs:322-334), both new `compact_context` tests, and both new `context.rs` tests. `pushTranscriptEntry` / `fmtTokens` were pre-existing and documented.
- `frontend/src/vite-env.d.ts` (new) is the standard one-line `/// <reference types="vite/client" />` — needed to type the `?raw` import in `InflightBar.test.ts`; correct.
- `frontend/vitest.config.ts` adds the new test file to `include` — required for it to run; correct.
- The new frontend tests (`useAgentStore.test.ts`, `ipc-contract.test.ts`) and the golden-fixture round trip (`tests/contract_fixtures.rs`) all exercise the new event through the real dispatch path.
- The two new `compact_context` regression tests (agent.rs:2538-2682) are proper defect-regression tests: the failure test asserts an Error event is emitted (would fail against the old `.unwrap_or_else` swallow) and messages are preserved; the success test asserts a Compacted event with a real reduction.

### Minor (non-blocking) observations
- **Comment inaccuracy** at `src/agent/turn.rs:230` — "`did_summarize` stays false: no compaction happened" is wrong on the Err path (see focus point 4). The behavior is safe; only the comment misleads. Suggested rewording above.
- **`.coding/plans/stack.json`** is modified (plan-stack bookkeeping) and **`.coding/plans/c2d3f2d0-….md`** is new — both are sandboxed bookkeeping state, expected to be committed alongside the feature per the closing sequence.
- The `reduceCompacted` reducer uses `pushTranscriptEntry`, which caps the transcript via `capTranscript` (agentState.ts:399) — the F3 unbounded-transcript fix is preserved; the compacted note cannot grow the array without bound.

## Security / bugs / correctness
None found. No new IPC surface (the `compact` command already existed); no new untrusted-input path; the Error event strings format an `anyhow::Error` into a user-visible message, which is the established pattern in this codebase.

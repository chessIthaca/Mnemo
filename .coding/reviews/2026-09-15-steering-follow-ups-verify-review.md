## Verdict: PASS

Round-2 verification of plan 0fa048a9 ("Steering follow-ups: tool-scoped detection, recency-gated riders, expandable memory hits") at commit c40d6b7 on wt/agenticcoder. Round-1's single HIGH finding (corrupted PLAN.md steering bullet) is FIXED; the commit contains everything round-1 reviewed plus the review report; no code drift; marker-string parity intact. One informational note (plan-file checkbox, not a finding).

---

## Finding 1 (HIGH, round-1) — PLAN.md steering bullet: FIXED

`PLAN.md` lines 107–113 now read:

```
107:   stay nudge-free. Rider-side steering lives in `tool/steering.rs`
108:   (`recalled_context_block`, passive `recall_peek` shared by `create_plan`
109:   and `spawn_agent`; `agent/steering_stats.rs` counts per marker kind how
110:   often the next call followed the nudge — detection is tool-scoped, so a
111:   content-bearing result from a tool that emits no marker can never fire a
112:   kind falsely — surfaced via the `get_steering_stats` IPC command in the
113:   Trace tab header).
```

- The previously dropped clause (`and spawn_agent; agent/steering_stats.rs counts per marker kind how`) is restored on line 109.
- No duplicated lines (the byte-identical 109/110 pair from round-1 is gone; the sentence now flows once through lines 108–113).
- The full required text is present verbatim: `(recalled_context_block, passive recall_peek shared by create_plan and spawn_agent; agent/steering_stats.rs counts per marker kind how often the next call followed the nudge — detection is tool-scoped, so a content-bearing result from a tool that emits no marker can never fire a kind falsely — surfaced via the get_steering_stats IPC command in the Trace tab header).`
- Verified in the committed state via `git show c40d6b7` (PLAN.md is among the commit's changed files) and the working tree (PLAN.md unchanged since the commit).

## Commit contents sanity check: PASS

`git show --stat c40d6b7` lists 19 files: the 13 modified + 5 new files round-1 reviewed, plus the review report `.coding/reviews/2026-09-15-steering-follow-ups-review.md` (73 lines, committed with the fix) — exactly the expected set:

- **Modified (13):** `.coding/backlog.jsonl`, `.coding/plans/0fa048a9.md`, `PLAN.md`, `README.md`, `frontend/src/components/chat/Message.tsx`, `frontend/src/hooks/agentEventReducer.memory.test.ts`, `frontend/src/hooks/agentEventReducer.ts`, `frontend/src/lib/toolCardPaths.test.ts`, `frontend/src/lib/types.ts`, `src/agent/dispatch.rs`, `src/agent/steering_stats.rs`, `src/tool/agent/spawn_agent.rs`, `src/tool/steering.rs`
- **New (5):** three `.coding/knowledge/` records (DECISION recall-rider-vs-explicit-memory-search measurement plan; two HOW search-usage tallies), plus the review report and the plan file updates as committed
- Commit message documents the HIGH finding and its fix, and the re-run matrix (1656 cargo tests, 678 vitest, both builds clean).

**Working tree:** `git status --short` shows a single modification: `.coding/plans/0fa048a9.md`, whose only delta is the step-4 checkbox flip (`- [ ] 4.` → `- [x] 4.`) — the plan-completion bookkeeping the workflow writes when the final step is marked done. No content change, no source/docs drift; all round-1 reviewed content is committed. Informational note only.

## Spot-check — marker strings vs emitters: PASS (no drift)

All five emitter strings are intact in the committed state, and the emitter files (`search.rs`, `shell.rs`, `codegraph.rs`, `read_files.rs`) appear in NEITHER c40d6b7's changed-file list NOR the working-tree diff — so nothing changed in them since round-1's verification, and the detector-table ↔ nudge-site parity round-1 established holds by construction:

- `SearchNudge` "is an indexed symbol" — `src/tool/agent/search.rs:243` (`symbol_nudge`) ✓
- `ShellTip` "TIP: for file-content search" — `src/tool/agent/shell.rs:41` (`GREP_NUDGE`) ✓
- `GraphMiss` "No symbols matched" — `src/tool/agent/codegraph.rs:87` (`miss_hint`) ✓
- `ReadNudge` "SYMBOL NUDGE:" — `src/tool/agent/read_files.rs:265` (leading prefix, `starts_with` contract) ✓
- `RecallRider` "RECALLED CONTEXT" — `src/tool/steering.rs:143` (`\n\nRECALLED CONTEXT ({} hit(s)) — ...` header) ✓

## Closing

Evidence base: round-1 report (`.coding/reviews/2026-09-15-steering-follow-ups-review.md`), `git show --stat c40d6b7`, `git diff HEAD` + `git status`, PLAN.md lines 100–119, and a direct grep of all five marker emitters. Round-1's only finding is fixed verbatim; everything else verified as reviewed. Verdict: **PASS**.

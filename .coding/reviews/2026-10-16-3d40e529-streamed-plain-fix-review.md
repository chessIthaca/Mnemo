## Verdict: FINDINGS (0 high, 1 low)

One low finding: `settleTextIntoTranscript`'s fresh-append branch (a late delta landing after a non-assistant tail) — the "never drop text" guarantee's other half — has no regression pin. Both landed late-delta tests end the turn on a trailing assistant entry, so they exercise only the merge branch; a regression that drops the tail again for error/tool-card tails would keep the whole suite green. Everything else verified and passes: the fix meets all four acceptance criteria, the three deliberate behaviors are preserved, both delta writers behave identically, and the four streaming-test adaptations are honest (assertions intact, mid-turn context faithful to the real event flow). Root cause, plan and BUG record are consistent with what shipped. Full verification detail appended below.

Reviewed-state: 0d16ab7ef11bd2943156ed409ee7966d9e0f6d78

## Scope — what was actually read

Round-2's base revision is the commit under review itself (`0d16ab7`, HEAD of `wt/stream-plain`); the intervening commit delta is empty and the uncommitted remainder is `.coding` bookkeeping only. Since no prior round completed a report against the code, this review covered the **full delta `4f39f06..0d16ab7`** per the task's work order:

- Commit message + stat (`git show 0d16ab7` — 11 files: the 8 listed code/test files plus `.coding` bookkeeping).
- `agentState.ts` 488–732: `capTranscript` (495–501), the flush contract + `flushStreamingText` (674–686, untouched), `settleTextIntoTranscript` (707–718).
- `agentEventReducer.ts` 190–335 (`reduceStarted`, `appendAnswerEntry`, `reduceTextDelta`) and 1360–1460 (`reduceFinished` — unchanged, not in the delta).
- `useAgentStore.ts` 1120–1216: `appendStreamingText` settle/running branches.
- `InflightBar.tsx`: imports (Markdown/MarkdownLink/CodeBlock), panel (`settled={!running}` at 586), `ActivityLine` gate (595–657); `running` confirmed as the live agent flag driving phase label, elapsed timer and compact gating.
- `Message.tsx` 265–359 (settled-assistant overrides — byte-identical component overrides; file NOT in the commit, so the deliberate plain streaming path at 278–296 is untouched) and `Markdown.tsx` 1–55 (module-level lazy + Suspense fallback).
- All four test files at the regression/adaptation sites (useAgentStore.test.ts 100–234 + 1900–1948; useAgentEvents.dispatch.test.ts 483–546; InflightBar.test.ts 267–292; ipc-contract.test.ts 128–160).
- `.coding/plans/3d40e529.md` (both root causes, preserve list, red→green design), the BUG memory file, `backlog.jsonl` line 89, and `.coding/plans/5444368c.md` (bookkeeping riding the commit).

Suites: a read-only reviewer cannot re-run tests (no shell); the parent re-ran both green in-session before this spawn (`npm test` in `frontend/` exit 0, 1333 passed; `cargo test --workspace` exit 0, 424 passed), matching the commit message's evidence.

## Acceptance criteria

1. **Markdown at turn end, no user action, no plain remnant — PASS.** Gate `entry.kind === "answer" && settled` (InflightBar.tsx:617) with `settled={!running}` (586). `running` flips false inside the same `reduceFinished` state update that flushes, so settled answers render markdown in one render pass; the transcript path already worked and `Message.tsx` is untouched. The markdown path uses the same `Markdown` component with byte-identical overrides as `Message.tsx` (`a: MarkdownLink`; the `code` override dropping `node`/`ref`/`key`; `CodeBlock` for fenced code; same prose wrapper classes minus the transcript-only `chatProseCap` variants).
2. **Text-before-tool-card flush ordering preserved — PASS.** `flushStreamingText` and its contract comment (agentState.ts:674–686) are unchanged; `reduceFinished`/`reduceToolStart` are not in the delta.
3. **Late-delta regressions genuinely red — PASS.** Three regression tests, matching the BUG record's names verbatim: useAgentStore.test.ts:1919 (reducer path), useAgentEvents.dispatch.test.ts:493 (batched writer, late rAF flush after finished), InflightBar.test.ts:277 (render gate). Red-by-construction pre-fix: pre-fix both writers appended into `streamingText` unconditionally, so the `expect(streamingText).toBe(""))` + merged-transcript assertions fail, and the InflightBar source contract fails with no gate in the source.
4. **Suites green — PASS** per the parent's in-session runs (reviewer cannot re-run; noted above).

## Preserved deliberate behaviors — all three intact

- **Per-token PLAIN streaming while running:** `Message.tsx:278–296` untouched (not in the commit); InflightBar's live answer path stays plain — the gate excludes running answers, with a comment documenting the perf choice (InflightBar.tsx:613–616).
- **activityLog NOT cleared at flush:** `flushStreamingText` never touches the log; it clears only in `reduceStarted` (next turn) — the 2026-08-22 behavior, restated in the new comments.
- **Reasoning stays plain in the thinking box:** the gate is answer-only; reasoning color/render path unchanged.

## Risk focus

- **(a) Settle correctness — verified.** Merge branch preserves `entryId`/`ts` (spread of the trailing entry into a fresh array — pure, no mutation of the old transcript); append branch goes through `capTranscript` (entry cap + image budget — the funnel every append site uses) and stamps `entryId`/`ts` directly (documented convergence with `applyAgentEvent`'s stamping). The settle path never touches `streamingText` and never bumps `liveCompletionTokens` (the bump lives only in the running branch; both writers skip it while idle). Empty-text early return keeps references — no gratuitous re-render churn.
- **(b) Render gate — verified.** Running answers, reasoning and info/error entries keep the plain render. `settled` flips exactly once per turn end (atomic with `running=false`); the next `started` clears the log in the same state update, so there is no stale-plain flash of old answers when a new turn opens. `Markdown` is a module-level `lazy` with a stable identity — no remount loop; the one-time raw-text Suspense fallback is Markdown.tsx's shared, documented transient form (in practice the chunk is already loaded by the transcript long before a settle).
- **(c) Writer consistency — verified.** `reduceTextDelta` (agentEventReducer.ts:288–311) and `appendStreamingText` (useAgentStore.ts:1148–1183) carry line-for-line identical settle logic; the batched writer reads `running` at flush time via `set((s)=>…)`, correct for the late-flush case. Both keep the `appendAnswerEntry` mirror, so the thinking box answer also receives the late tail — transcript and box stay consistent.
- **(d) Test-edit honesty — verified.** The four streaming tests (liveTokens, text_delta-appends, mirrors — useAgentStore.test.ts; event-text-delta — ipc-contract.test.ts) gained a `started` dispatch plus explanatory comments; every original assertion survives intact (token estimate, streamingText accumulation, mirror coalescence), and each genuinely requires the started post-fix (an idle delta settles and skips both the token bump and streamingText). The mid-turn context is faithful to production — deltas only arrive between `started` and `finished`, and the dispatcher flushes buffers before structural events. No loosened expectation found. The InflightBar regression is a `?raw` source-contract test — the file's established convention (vitest runs in node env, no DOM), red pre-fix, pinning the gate, the Markdown component, the MarkdownLink override, and the preserved plain line.
- **(e) Bug-plan checks — verified.** Regression tests exercise all three changed paths; the two root causes are documented in the plan's Bug section and the commit message; the BUG memory file exists and is accurate (symptom → root causes → fix pointer `0d16ab7`/plan 3d40e529 → the exact landed test titles).

## Findings

### F1 (low): the settle helper's fresh-append branch has no regression pin — the dropped-tail guarantee is unpinned for non-assistant tails

- **Where:** `frontend/src/hooks/agentState.ts:707–718` — `settleTextIntoTranscript`'s else branch (fresh capped assistant entry when the transcript tail is not an assistant entry).
- **Gap:** Both landed late-delta regression tests end the turn on a trailing assistant entry, so they exercise only the merge branch (useAgentStore.test.ts:1919: `started → "Hello" → finished` flushes an assistant tail, the late `" world"` merges; dispatch.test.ts:493: same shape through the batched writer). The append branch — the fix's own hardening for a turn ending on an error/tool card (or a finished turn with no streamed text) — runs only incidentally (e.g. useAgentStore.test.ts:212 "reasoning �� answer keeps both entries" settles an un-started delta through the append branch but asserts only `activityLog`). If the else branch regressed to a no-op, the pre-fix tail-drop would silently reappear for exactly that case and every current test stays green — the one outcome the bug-plan rule exists to prevent.
- **Suggested red-able pin** (reducer harness, same file): `started → finished` with no deltas (no assistant tail to merge into) → late `text_delta " world"` → assert `streamingText === ""` AND `transcript[transcript.length − 1]` is a fresh `assistant` entry with text `" world"` (optionally a second variant ending the turn on a tool card). Genuinely red pre-fix: the delta landed in idle `streamingText` and no transcript entry existed; green post-fix.
- **Severity: low** — the primary defect is covered red→green on both writers; this is the coverage gap on the fix's own defense-in-depth branch.

## Constitution lines

- **Documentation sync — PASS.** All changed functions carry updated doc comments explaining the 076f601e rule (reducer, helper, store writer, ActivityLine). README.md has no streaming/thinking-box claims to go stale (searched — no matches); PLAN.md's mentions are the tech stack and the unchanged transcript-rendering design.
- **Multi-platform neutrality — PASS.** Pure React/TS delta; no platform APIs, paths, or shell syntax.
- **File-tools-first — PASS.** No shell-based file mutation in the delta.
- **Warning-free build — PASS** (frontend-only change; Rust side green per the parent's `cargo test --workspace`).
- **Bookkeeping accuracy (one line each):** `backlog.jsonl` item 076f601e (in_flight, plan link) matches the shipped state; plan frame 3d40e529's review stamps match the harness rounds; the BUG memory is accurate; the commit's riding bookkeeping `plans/5444368c.md` (+40) is the closeout frame of the already-landed second-instance plan (steps checked; review stamps ae23b30/f8700c8 match the known wt/mnemo commits) — accurate vs. what shipped on main via PR #84.
- **Process remark (one line):** the code change was committed at the round-2 base itself and no prior round completed a report against it, so the full commit was reviewed per the task's work order; the mechanical round-2 delta is only the `.coding` bookkeeping verified above.

**Bottom line:** the fix is correct and complete against the plan and acceptance criteria; add the F1 regression pin, re-run the frontend suite, and this is done.
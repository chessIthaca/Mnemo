## Verdict: PASS

Round-3 closing review of plan 3393bc7d ("Fix Mnemo2 streaming DOM and real tool activity") on wt/mnemo. Base 0718b3d9b754b034a0ef728220605b1d44b69199 is still HEAD with no intervening commits, so the entire delta is the uncommitted working tree (8 modified + 8 untracked). The round-3 additions are exactly the two LOW-1 regression tests the round-2 report required: both are deterministic and genuinely red-capable against the pre-fix code, every round-1 and round-2 finding remains closed with its regression lock, and no regressions were found anywhere in the changed set.

## Scope read (empty-diff rule — dirty tree)

Read in full or in verified hunks: `Markdown.retry.test.ts` (new, all 74 lines); `LeadChat.test.ts` (all 375 lines, including the new "does not open a result disclosure when the output is empty" test at 219-253); `Markdown.svelte` (module script, instance script, markup; the style tail is round-1 carry-over); `ToolActivity.svelte` (all 75 lines); `LeadChat.svelte` (all 193 lines); `leadState.ts` tool-reducer surface (lines 40-552: normalizeToolResult, appendLiveOutputTail, startToolEntry, appendToolArguments/appendToolOutput, finishToolEntry, leadEntriesFromTranscript, applyLeadSnapshot, applyLeadEvent, leadEventNeedsSnapshot); `leadState.test.ts` lines 290-623 (identity no-ops, 16 KiB cap, split surrogate, transcript restore, turn scoping, post-snapshot scoping); `types.ts` 150-194; `.coding/design/mnemo2.md` 248-272; `.coding/plans/3393bc7d.md` (all 36 lines); both knowledge records; both prior round reports; git status/log/diff of the whole delta.

Stability cross-check: every round-2 line reference resolves to identical content today — Markdown.svelte:2-10 (retry wrapper), ToolActivity.svelte:48/31, leadState.ts:133-141 and 380-395, leadState.test.ts:393-421/494-538/540-622, Markdown.test.ts:14-17/60-116, and the LeadChat scroll/unmount tests (unmount moved 325→361, exactly the 36-line insertion shift caused by the new test). The only differences from the round-2-verified tree are `Markdown.retry.test.ts` (new file) and the empty-result test insert. Gate arithmetic is consistent: round-2's 17 files / 86 tests + 1 new file (1 test) + 1 new LeadChat test = 18 files / 88 tests, matching the reported gate.

Reviewed-state: 0718b3d9b754b034a0ef728220605b1d44b69199

## LOW-1 closure — the two new regression tests

**LOW-1(a): `Markdown.retry.test.ts` "retries a rejected renderer import on a later render" — locks L2, red-capable, no `vi.resetModules()` masking.**

- The component under test is statically imported once (`import Markdown from "./Markdown.svelte"`, line 3) and that same evaluated module is reused across both phases. `vi.resetModules()` (line 65) clears the module registry only for the *mocked* `../lib/markdown` — it cannot re-evaluate the already-bound component module, so the `<script module>`-scoped `markdownModule` cache (Markdown.svelte:2-10) persists across the reset. The cache is exactly the system under test, and it is not bypassed.
- The attempt counter lives in a `vi.hoisted` object (lines 5-8) — test-file scope, surviving the registry reset — and the factory (10-16) throws on the first instantiation and returns the same hoisted `renderMarkdown` vi.fn on later ones, so the spy assertion cannot go stale across module re-instantiation. The isolated file (separate from Markdown.test.ts's always-resolving gate mock) is the correct design.
- Phase 1: `flushFrame()` runs `renderFrame` → `loadMarkdown()` → the import rejects (factory throws, `attempts === 1`) → the component's rejection handler nulls `markdownModule` (the L2 fix, Markdown.svelte:5-8) and the render-time `.catch` keeps the frame. Asserted: textContent "First" comes from the plain-text fallback (`hasRendered` false), and `renderMarkdown` was never called.
- Phase 2: the reset simulates the transient chunk failure being gone; `rerender({source:"Second"})` re-runs the effect and the next frame calls `loadMarkdown()`. With the fix, the nulled cache forces a second `import()` → the factory re-runs (`attempts === 2`) → resolves → the version-guarded commit writes the rendered HTML. Asserted: `attempts === 2` (the second import actually occurred), `renderMarkdown` called with "Second" (the commit uses the newest source), textContent contains "Second".
- Red proof: against the pre-fix cache-forever code, `loadMarkdown()` returns the still-cached rejected promise — no second import, `attempts` stays 1, `renderMarkdown` never called, and because `fallbackCaptured` pins the fallback to the first source while `hasRendered` stays false, textContent never shows "Second": `vi.waitFor` times out and the test fails. The reset is what makes the retry *able* to succeed; the fix is what makes the retry *happen at all*. No masking.
- Determinism: explicit Map-driven RAF stub, act-wrapped microtask settling, `vi.waitFor` polls on in-memory mocked promises only; exactly one frame flush per phase means exactly one import attempt per phase — `attempts === 1` and `=== 2` are structural, not timing-dependent. `beforeEach` resets counter and spy.

**LOW-1(b): `LeadChat.test.ts` "does not open a result disclosure when the output is empty" — locks L3, red-capable.**

- Renders two entries through the real LeadChat → ToolActivity path: `{success: true, output: ""}` ("noop") and `{success: false, output: ""}` ("guard"), with empty arguments and liveOutput.
- Locates each card by its real aria-label ("noop — Completed" / "guard — Failed", from ToolActivity.svelte:16), asserts the status badge ("Completed"/"Failed", line 31) still renders — the keep-the-badge half of the L3 fix — and asserts `queryByText("Result")` / `queryByText("Error")` is null within each card.
- Red proof: against the pre-fix unconditional `<details>` (gated on `entry.result` only), the empty-output success rendered `<summary>Result</summary>` → `toBeNull()` fails. With the gate at ToolActivity.svelte:48 (`entry.result && entry.result.output`) the disclosure does not exist. `normalizeToolResult` (leadState.ts:71-92) guarantees `output` is a string, so the falsy gate covers exactly the empty case.
- Determinism: fully synchronous render and assertions, unique aria-labels per card, `setLeadEntries` replaces the whole list, `within()`-scoped queries.

## Round-1 and round-2 finding closure — all still closed, no regressions

- **L1 (turn scoping)** — present: `started` strips `streamIndex` from every tool entry that has one (leadState.ts:383-387) and `startToolEntry` preserves a finalized entry's result/arguments/liveOutput, returning only a tool-name refresh (133-135); only running entries take the new stream index (136-141). Locked by "scopes argument fragments to the current turn" (494-538) and "preserves restored finalized calls and scopes live calls after a snapshot" (540-622).
- **L2 (renderer retry)** — present (Markdown.svelte:2-10) and now locked by Markdown.retry.test.ts.
- **L3 (empty disclosure)** — present (ToolActivity.svelte:48) and now locked by the empty-result test.
- **L4a (split surrogate)** — "drops a split surrogate at the live-tail cut" (393-421): the cut lands after `"\uD83D\uDE00"` and asserts length exactly cap-1, content `"a".repeat(cap-1)`, first code unit "a" �� the pre-fix tail would keep the orphaned 0xDE00 low surrogate.
- **L4b (LeadChat unmount)** — "disconnects the resize observer and cancels a pending frame on unmount" (361-374): asserts `observer.disconnect` and `cancelAnimationFrame` both fire on unmount.
- **L4c (post-snapshot live events)** — the 540-622 test: a restored finalized entry keeps `{"old":1}` and its result across `started`, a re-announced finalized id, and a same-index live call.
- Previously verified behaviors re-confirmed at their round-2 line positions: unknown/late output identity no-ops (305-351, `toBe`), 16 KiB tail bound (353-391), host-tool-as-team-work honesty (255-283), scroll suite (285-359), Markdown retention/coalescing/unmount suite (Markdown.test.ts unchanged), sanitized-output and text-interpolation boundaries.
## Behavior goals — unchanged and satisfied across the full changed set

Real tool events/transcript only: the reducer consumes wire `SessionEvent` kinds (`tool_call_start`/`tool_call_arg_delta`/`tool_output_delta`/`tool_result`/`host_tool_call`) and reconstructs from persisted transcripts; ToolActivity renders reducer-produced entries only, and the only hand-built entry data lives in test files (the sanctioned pattern — `fixtureState` is test-only and the `check:no-mock` assertion still rides the build gate). Ordered, non-duplicated cards: live upsert by call id keeps thread position (leadState.ts:129-155), transcript correlation via the toolIndex map with unmatched results retained as result-only entries (227-292). Turn-scoped routing and finalized-not-reopened: the L1 closures above. Bounded live tail: 16 KiB cap with split-surrogate drop (53, 94-101), cleared on finalize (203). Markdown: frame retention (html written only in the version-guarded async commit, 62-64), one-pending-frame coalescing (50-53), stale-completion rejection (62), import retry (2-10), unmount cleanup (29-37 plus the destroyed/version guards). LeadChat scroll: 48px near-bottom threshold (11), re-check inside the frame callback so a queued frame cannot override a later user scroll-up (51-58), typeof-guarded ResizeObserver on the content div (23-27), unmount disconnect + cancel (29-36). Sanitized `@html` only in Markdown's committed output; the pre-first-render fallback and all tool arguments/output/results are text interpolation in `<pre>`.

## Constitution checks

- **Documentation sync** — pass. The round-3 delta adds tests only; the design doc (`.coding/design/mnemo2.md:255-265`) already documents the real tool cards, live+transcript feeding one reducer, no synthesized activity, host-tool-as-team-work honesty, retained-frame coalesced rendering, and the 48px/ResizeObserver scroll — all still accurate against the code. The README was swept in round 1 and nothing in the delta touches its statements. No stale or missing docs.
- **Multi-platform neutrality** — pass. Only cross-platform web APIs appear in the delta (`requestAnimationFrame`, `ResizeObserver` behind a typeof guard, `<details>`, Lucide icons); no Windows-only API, path, or shell syntax in source or tests.
- **File-tools-first** — pass. Nothing in the changed set indicates shell-based mutation; all files are conventionally formatted and internally consistent.
- **Warning-free build** — pass per gates. No Rust files changed (crate-level `deny(warnings)` untouched); `npm --prefix mnemo2 run check` reported 0 errors / 0 warnings; no `#[allow]` or analogous silencing anywhere in the delta.
- **Security** — pass. No new `{@html}` anywhere; both new tests exercise text-interpolated or sanitized paths only. The retry test's mock returns `<p>${source}</p>`, but that is test-file code mirroring the real renderer's output shape; the production sanitizer (`lib/markdown`) is unchanged and still covered by its own suite.

## Bookkeeping accuracy (one line each)

- `.coding/plans/3393bc7d.md` — accurate: steps 7-9 describe exactly the code and tests now in the tree (their `[ ]` checkboxes remain for the parent to tick at completion; step 10 commit + finish is legitimately open); the Reviews section stamps rounds 1-3 all at base 0718b3d, matching HEAD.
- `.coding/knowledge/decision/2027-01-11-mnemo2-lead-dom-rendering-fix-scope.md` — accurate: the recorded frontend-only scope (no Rust IPC, no mock activity, retained frame, near-bottom intent) is exactly what shipped.
- `.coding/knowledge/how/2027-01-11-review-mnemo2-ui-without-screenshots.md` — accurate and honored by this round: source inspection and tests only, no screenshots, no CDP, no extra browser instances.
- `.coding/plans/601647da.md` (+7 lines) — the pre-existing unrelated Phase-2 reviews/cost-report bookkeeping, unchanged from rounds 1-2 and correctly left untouched per the task.
- Round-1 and round-2 reports — present on disk with the FINDINGS verdicts this round closes against.
- Generated/lockfile noise — none in the changed set.

## Process remarks (one line each)

- The entire implementation remains uncommitted since round 1 (HEAD = base 0718b3d, no intervening commits), so the uncommitted diff necessarily re-shows the round-1/2-verified hunks next to the two round-3 test additions; since both prior rounds ended in FINDINGS, I verified every reported finding's fix is present in the delta (all are) and line-confirmed the implementation is byte-stable against the round-2-verified state rather than re-line-reviewing it.
- I could not re-run the gates myself (read-only reviewer, no shell); the parent's reported gates (test 18 files / 88 tests passed; check 0 errors / 0 warnings; build incl. no-mock passed) are arithmetically consistent with round-2's 17/86 plus the two new tests, and the static analysis above found nothing the gates could be masking. No Rust gate is implicated — no Rust files changed.

## Closing

LOW-1 is closed by two deterministic, red-capable regression tests that lock exactly the L2 retry and L3 empty-disclosure fixes; all six round-1 findings remain fixed and test-locked; the full changed set still satisfies the three behavior goals and the constitution. Nothing remains to fix — the tree is ready to commit to wt/mnemo with this report and finish.

Reviewed-state: 0718b3d9b754b034a0ef728220605b1d44b69199
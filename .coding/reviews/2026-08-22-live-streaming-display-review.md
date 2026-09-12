# Review — Live streaming display independent of rAF (RDP-safe)

**Branch:** fix/live-streaming-display · **Plan:** "Live streaming display independent of rAF (RDP-safe)" · **Date:** 2026-08-22

Reviewed all uncommitted changes (`git diff HEAD`): `frontend/src/hooks/deltaFlush.ts`, `frontend/src/hooks/deltaFlush.test.ts`, `frontend/src/hooks/useAgentEvents.ts`, `frontend/src/hooks/useAgentEvents.dispatch.test.ts`, `README.md`, plus `.coding/` bookkeeping (out of scope). Committed history was not reviewed.

## Verdict: no findings

The change does exactly what the plan set out to do, and I traced every focus point through the code. Details below.

## Focus-point verification

**1. Hybrid policy correctness (per-frame batching preserved).** `routeFlush` (useAgentEvents.ts:335-341) routes to `flush()` only when `shouldFlushSync(bufferedChars) || shouldFlushNow(lastFlushAt, deps.now())`, else `scheduleFlush()`. When rAF is healthy the frame fires first (frame callback = `flush`, which runs `cancelPending()` first), so the fallback timer is cancelled before it can fire — dense-burst behavior is unchanged, and the test at useAgentEvents.dispatch.test.ts:137-153 ("a delta arriving < MAX_FLUSH_INTERVAL_MS later batches and waits for the frame") asserts exactly this, including `timerTools.count() === 0` after the frame fires. `scheduleFlush` (useAgentEvents.ts:315-324) is single-flight on the rAF id (`if (deps.rafId.current !== 0) return;`) and arms the fallback timer only when `deps.timerId.current === null` — no double scheduling on either side.

**2. flush() cancels both + stamps lastFlushAt.** `flush()` (useAgentEvents.ts:267-307) calls `cancelPending()` (lines 255-264), which cancels the pending rAF (caf + zero the ref) AND the pending timer (clearTimer + null the ref), then stamps `lastFlushAt = deps.now()` (line 269). Because the timer callback and the frame callback both route through `flush()`, the timer path stamps too — so the next sparse delta can time-gap flush. Test coverage: useAgentEvents.dispatch.test.ts:209-232 ("a sync flush cancels the pending frame AND timer — no double flush") and the timer-path test at 169-191.

**3. First-wins / no double-append.** Both callbacks route through `flush()`, which calls `cancelPending()` before appending anything — so whichever side fires first cancels the other side, and the other side is a no-op (its ref was zeroed/null'd). The timer callback additionally nulls its own id *before* calling `flush()` (useAgentEvents.ts:320), so a re-entrant flush can't observe a stale timer id. Tests assert the no-double-append property from both directions: frame-first (137-153, late timer not fired but count asserted 0), timer-first with a late frame fired afterward (169-191), and sync-flush with both a late frame and a late timer fired (209-232).

**4. clearStreamingBuffer / drainStreamingBuffer.** `drainStreamingBuffer` (useAgentEvents.ts:112-136) deletes the agent's entries from all three buffers, and cancels frame + timer **only when all three maps are empty** (lines 122-135) — other agents' pending flushes survive. `clearStreamingBuffer` (146-153) is a thin wrapper injecting `cancelAnimationFrame`/`clearTimeout`. The BufferHandle registered by the hook (494-500) includes `timerIdRef`, so the handle/timer wiring is consistent. Tests: drain + cancel with frame id 7 / timer id 42 (242-270) and the other-agent-still-buffered case asserting no cancellation (272-294). The `/clear` call site (InputBar.tsx:359, also 388, 476) passes the same single `AgentId` argument — signature compatible.

**5. Unmount cleanup / StrictMode.** The unmount cleanup (useAgentEvents.ts:510-526) calls `dispatcher.flush()`, which runs `cancelPending()` — the timer is cancelled on unmount; no leak. The `useEffect` deps are unchanged (four stable store selectors, line 527); StrictMode double-mount swaps `activeDispatch` and re-registers `activeBuffer` with the same refs, and the final flush on the first unmount drains anything buffered before the remount — sound.

**6. 64 KiB cap preserved.** `shouldFlushSync` is byte-for-byte unchanged (deltaFlush.ts:23-25, `>= MAX_BUFFERED_DELTA_CHARS`) and still routed first in `routeFlush`; the >64 KiB sync flush test (193-207) still passes with no frame. The old predicate tests were re-written rather than dropped: the cap boundary (64 KiB - 1 → false, 64 KiB → true) is asserted at deltaFlush.test.ts:28-31, and the wiring-level cap test lives in the dispatcher suite.

**7. Test coverage would fail without the fix.** The dispatcher tests drive the real `createAgentEventDispatcher` (not just predicates):
- Deleting the `shouldFlushNow` route from `routeFlush` → the "stalled rAF … lands IMMEDIATELY" test (155-167) and "first delta flushes immediately" (129-135) both fail.
- Deleting the fallback timer from `scheduleFlush` → "fallback TIMER flushes the buffered batch" (169-191) fails (`timerTools.count()` never reaches 1).
- Deleting the timer-cancel from `drainStreamingBuffer` → the `cancelledTimers`/`timerIdRef.current === null` assertions (266-269) fail.
No vacuous tests found: every `it` asserts at least one non-tautological behavior, and the fake clock/timers are advanced explicitly. One nuance: the clock-skew guard test (`shouldFlushNow(2000, 1500) === false`, deltaFlush.test.ts:25-26) is a pure-predicate test — a regression that *removed* the guard while keeping `shouldFlushNow` would still be caught there, though a wiring-level guard test doesn't exist; not a finding (the guard is trivially cheap and the predicate test pins it).

**8. Types / strictness.** No `any`, no unused imports (checked every changed file; `vi` is used for the mock, `MAX_BUFFERED_DELTA_CHARS` import removed from deltaFlush.test.ts along with its uses). All new public exports carry doc comments: `MAX_FLUSH_INTERVAL_MS` (deltaFlush.ts:27-32), `shouldFlushNow` (34-44), `BufferHandle` (useAgentEvents.ts:80-91), `StreamCancelDeps` (94-101), `drainStreamingBuffer` (103-111). Node-env safety: `setTimer`/`clearTimer`/`now` are injected (DispatcherDeps 203-209), the test fakes them (makeTimers, 57-74), and the vitest config pins `environment: "node"` (vitest.config.ts:9) with `../lib/tauri` mocked — no browser globals touched. `ReturnType<typeof setTimeout>` typing is consistent between `DispatcherDeps.timerId`, `scheduleFlush`'s `setTimer`, the hook's `timerIdRef`/injections, `StreamCancelDeps.cancelTimer`, and the test's `timerIdRef`/`cancelledTimers` — the injected `setTimer` returns `number` in the tests, which structurally satisfies `ReturnType<typeof setTimeout>` in this codebase (lib DOM, `strict: true`, tsconfig.json:5,14), and `npm run build` runs `tsc` over `src` including the test files.

**9. Constitution compliance.**
- *Regression tests:* present and meaningful — the new dispatcher tests fail without the fix (see 7). The R2 64 KiB-cap wiring guard remains intact.
- *No suppressions:* no `#[allow]`/`@ts-ignore`/`eslint-disable` added anywhere.
- *Docs synced:* README's streaming-display bullet extended (README.md:51 — the hybrid policy, 40 ms time-gap flush, per-frame batching, and fallback timer are all described). PLAN.md contains **no** rAF/"flush per animation frame" claims to update (searched — zero matches).
- *Multi-platform neutrality:* pure TS timers (`setTimeout`/`clearTimeout`/`Date.now` + injected rAF); nothing Windows- or macOS-specific crept in; no `cfg`/platform branches. The change is RDP-agnostic in code (rAF stall handling benefits any occluded state).

**10. Other checks (no issues found).**
- *Timer growth:* exactly one fallback timer can be armed at a time (single-flight guard + cancel-on-flush); `scheduleFlush` can never accumulate timers, and the timer is always cancelled or fired — no unbounded timer growth.
- *Clock skew:* `lastFlushAt` starts at 0 so the first delta always time-gaps (comment at useAgentEvents.ts:249-252 documents this); `shouldFlushNow` returns false for a future `lastFlushAt`, so a backwards clock jump can't cause a flush storm. `Date.now()` is monotonic enough for a 40 ms window.
- *Empty flush stamping:* `flush()` stamps `lastFlushAt` even on an empty buffer (e.g. unmount flush), which is intentional per the plan (prevents a fresh delta from immediately re-flushing after a no-op flush) and is what the focus point asked to verify.
- *Store-append ordering:* `flush()` clears each map before appending (useAgentEvents.ts:272-305), so a re-entrant dispatch during a store action can't double-append — unchanged pre-existing structure.
- *Backlog.json:* the id-84 streaming-display backlog item was moved to `pending` (it was previously mislabeled `failed` with a "plan loop never ran" note), and id-80 marked `failed` — consistent with this plan being the response to item 84. Bookkeeping-only, no functional impact.

## Scope note

The plan document for this change (`e57bb809…md`) is untracked (`.coding/plans/`), as are the other `.coding/` mutations; these are bookkeeping and out of scope per the task, but they will be included in the final commit as expected by the closing sequence.

**Conclusion:** No findings. The hybrid policy is correctly implemented, single-flight on both sides, first-wins with no double-append path, the drain path preserves other agents' pending flushes, unmount cancels the timer, the 64 KiB cap is untouched, tests are non-vacuous and would catch each of the named regressions, docs are synced, and no security/memory/type issues were found.

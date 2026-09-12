## Verdict: PASS

Round-2 verification of commit `fd49bdb` on `feat/reviewer-surface-and-failure-protocol` against round-1 report `.coding/reviews/2026-09-04-reviewer-surface-watchdog-review.md` (1 high + 4 low). Inspected current sources + `windows_sys` / fix-site searches; working tree assumed clean per task (fixes landed in that commit).

### H1 — Windows FFI / `MAX_STACK_FRAMES` gating — FIXED

Every `use windows_sys::…` and every capture item in the stack/WCT section is `#[cfg(windows)]`, including `MAX_STACK_FRAMES`, `ThreadGuard`, capture fns, and `mod wct`. Banner comment at ~L512–514 documents the gate.

Crate-wide `windows_sys` hits are only in `src-tauri/src/watchdog.rs`, all under windows cfg:

- `GetCurrentThreadId` at start / tests (`#[cfg(windows)]`)
- `thread_times_ms` / `filetime_to_ms` (`#[cfg(windows)]`)
- Module-level capture imports + bodies (`#[cfg(windows)]`)
- `mod wct` (`#[cfg(windows)]`)

No ungated `windows_sys` reference remains. Non-Windows builds keep empty stack/wait-chain sections and the platform CPU string. Multi-platform neutrality restored.

### L1 — Thread handle leak / resume invariant — FIXED

`ResumedOnDrop` → `ThreadGuard`: `Drop` calls `ResumeThread` then `CloseHandle`.

Paths:

1. `OpenThread` null → return, no handle owned.
2. `SuspendThread` == `u32::MAX` → manual `CloseHandle`, return (guard never created; thread not suspended).
3. Suspend success → `let _thread_guard = ThreadGuard(handle)`; every early return (`GetThreadContext` fail, empty walk) and normal completion drops the guard → resume + close exactly once.

Outer retry loop relies on drop between attempts so the target is never left suspended across retries. No residual `ResumedOnDrop` symbol.

### L2 — Doubled `STEP MARKER:` successor titles — FIXED

`supersede_plan_markers` strips `"STEP MARKER: "` / `"ACTIVE: "` before formatting:

`STEP MARKER: {base_title} — {reason}`

Seeded titles like `STEP MARKER: fix-crash plan` / `ACTIVE: fix-crash plan` become `STEP MARKER: fix-crash plan — COMPLETE` (finish) or `— ABANDONED` (abandon). Existing finish test still asserts live successors contain `— COMPLETE` and the unrelated marker stays; abandon path in `plan.rs` still uses the shared helper with `"ABANDONED"`.

### L3 — `set_rect` note ordering — FIXED

`try_apply_rect(..., on_apply: impl FnOnce())` invokes `on_apply()` only on the Applied path, immediately before `apply_rect`. Busy/Noop return earlier without calling it.

`browser_webview_set_rect` passes a closure that `note`s `"webview rect apply"`. Tests: Applied → Noop → Applied fires `on_apply` exactly twice; ring contract string is `"webview rect apply"`. No-flood property held (no note on Noop/Busy).

### L4 — README sync — FIXED

- Hang-watchdog bullet: stack walk (module!symbol + file:line) + wait-chain (blocked-on object), marked Windows-only enrichment, plus activity — matches shipped report sections.
- Multi-agent bullet: strict reviewer tool surface (reads/git/web_fetch/graph/memory·backlog query + `write_review_report`; no ask/mutate/plan/finish) and failed-reviewer protocol (ask user: retry other model / abandon / self-review; no blind respawn).

Wording matches behavior described in PLAN/agent/prompt and the implementation.

### Non-blocking round-1 gaps — addressed

- `const _: () = assert!(size_of::<WaitChainNodeInfo>() == 280);` in `mod wct` (MSVC layout guard: 8 + 256 + 8 + 4 + 4 pad).
- Doc comment on `wct::object_status` (maps unknown status → `Error`). Present; indentation is slightly uneven vs `object_type` but syntactically inside the impl and compiles.

### New-issue scan (fix commit / relevant files)

No new issues found in the fix-relevant surface:

- Handle close ordering is correct (resume before close).
- Marker strip is prefix-exact with trailing space (matches production marker shape).
- `on_apply` cannot run on Busy (mutex not held) or Noop (rect unchanged).
- `thread_times_ms` still correctly cfg-gated; its `CloseHandle` resolves via the module-level windows import (also cfg-gated).
- No `#[allow(...)]` introduced at these sites.
- Browser WebView2 remains the sanctioned Windows-only exception; watchdog enrichment is cleanly gated.

### Summary

All five round-1 findings are fully fixed in `fd49bdb`; the two noted gaps (layout assert + `object_status` docs) are present. No regressions or new findings from the fixes themselves. **PASS.**

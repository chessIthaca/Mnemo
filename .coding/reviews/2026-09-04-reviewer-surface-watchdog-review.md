## Verdict: FINDINGS (1 high, 4 low)

Review of **all** uncommitted changes on `feat/reviewer-surface-and-failure-protocol` (~28 tracked files; full `git diff HEAD`). Plan goals: strict reviewer surface, `backlog_list`, finish main-only, failed-reviewer protocol + Continue button, abandon marker supersede, watchdog stack/WCT capture, WebView2 ring notes, docs.

Tests claimed green (root 1423 / tauri 153 / frontend 523) were not re-run here (read-only review).

---

### High

#### H1. `watchdog.rs` Windows FFI imports are not `cfg(windows)`-gated — non-Windows builds break

**File:** `src-tauri/src/watchdog.rs` (~L512–526)

Module-level items are unconditional:

```rust
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{ ... };
// ...
const MAX_STACK_FRAMES: usize = 64;
```

while `windows-sys` is only a `[target.'cfg(windows)'.dependencies]` entry and the *functions* are `#[cfg(windows)]`.

On macOS/Linux:

1. `use windows_sys::...` fails with unresolved import (crate absent for that target).
2. Even if the uses were fixed alone, an un-gated `MAX_STACK_FRAMES` would be unused under `#![deny(warnings)]`.

This violates constitution multi-platform neutrality: enrichment must be Windows-gated without breaking other targets. The module docs claim non-Windows degrades gracefully; the new import block prevents compiling that path.

**Fix:** Wrap the entire stack/WCT import block, `MAX_STACK_FRAMES`, `ResumedOnDrop`, capture helpers, and `mod wct` in a single `#[cfg(windows)]` region (or prefix every `use`/item). Confirm with `cargo check` from `src-tauri` under a non-Windows target or CI equivalent.

---

### Low

#### L1. Thread handle leak after stack capture

**File:** `src-tauri/src/watchdog.rs` — `ResumedOnDrop`

On the SuspendThread-success path the handle is stored in `ResumedOnDrop`, which only calls `ResumeThread` in `Drop` and never `CloseHandle`. The SuspendThread-failure path correctly closes. Every successful capture (hang fire + `capture_thread_stack_walks_another_thread` test) leaks one `HANDLE`.

Resume-on-all-paths is correct and is the critical invariant; close is missing hygiene. Prefer drop order: `ResumeThread` then `CloseHandle` in `Drop` (or a second RAII wrapper).

#### L2. `supersede_plan_markers` successor titles double the prefix

**File:** `src/memory/finish_capture.rs` (~L218–220)

```rust
format!("STEP MARKER: {} — {reason}", marker.title),
```

When `marker.title` is already `STEP MARKER: …` or `ACTIVE: …`, successors become e.g. `STEP MARKER: STEP MARKER: doomed plan — ABANDONED`. Finish previously used `plan.title` (`STEP MARKER: {plan.title} — COMPLETE`). Supersede-by-id still works (abandon test passes on counts), but history titles regress and the doc comment’s clean “— COMPLETE / — ABANDONED” shape is misleading.

**Fix:** Use a neutral title such as `format!("STEP MARKER: plan {plan_id} — {reason}")` or strip known prefixes before formatting.

#### L3. `browser_webview_set_rect` notes after apply, not before

**File:** `src-tauri/src/ipc/browser_webview.rs`

Module contract: notes are pushed **before** controller ops so a hang names the in-flight call. `set_rect` calls `try_apply_rect` first and only notes on `RectOutcome::Applied` afterward. If `apply_rect` / compositor wedges the main thread, the ring will not show an in-flight rect op (unlike ensure/navigate/overlay/destroy).

Flood avoidance is reasonable; either note a cheap pre-call `"webview rect apply"` (accept more ring traffic) or document set_rect as the deliberate exception.

#### L4. README not synced for watchdog enrichment / reviewer contract depth

**Files:** `README.md` (unchanged) vs `PLAN.md` / `agent.md` / `prompt.rs` (updated)

- Hang-watchdog bullet still mentions only CPU busy-vs-blocked + activity, not stack walk or WCT wait-chain lines.
- Reviewer blurb stays a one-liner; expanded strict surface + failed-reviewer protocol live only in PLAN/agent/prompt.

Constitution review expectations treat README feature-list drift as incomplete docs. Not a logic bug; update the two bullets to match shipped behavior.

---

### Checked — no findings

**Reviewer surface / security**

- `ToolFilter::Reviewer` is a strict name allow-list (`current_plan` + listed only); no Skill-style auto-grant of Memory / `ask_user` / backlog.
- `schemas()` no longer unconditionally advertises Memory tools.
- `set_reviewer_allowlist` + `tool_allowlist_reviewer` wire `ToolFilter::Reviewer` from `spawn.rs` when `role == "reviewer"`.
- `REVIEWER_BASE_TOOLS` is query-only (git_log/show, web_fetch, graph_*, memory recall/list + retrieval, backlog_list, write_review_report); mutation names asserted absent in spawn tests.
- `finish` added to main-agent-only dispatch gate and turn schema filter; subagent-in-Reviewing test covers it.
- Reviewing arm drops `backlog_add` / `backlog_status`; `backlog_list` remains read-only everywhere (including skills + reviewer when listed).

**Failed-reviewer protocol**

- `AgentHandle.role` + `AgentManager::role`; console + GUI forwarders set `reviewer_failure_pending` only for `role == "reviewer"` && !success && no report; distinct `reviewer_failure_suggestion_text`.
- Dispatch denies `spawn_agent` + `role: reviewer` while latched; `ask_user` clears latch before the question path.
- Tests: console latch/text, generic non-reviewer failure, dispatch deny, ask_user clear.

**Continue button**

- `failed` on FINAL error; cleared on started/finished/clearConversation; InputBar `!running && failed`; store test covers the matrix.

**abandon_plan markers**

- Snapshot plan id under workflow lock, drop lock, then `supersede_plan_markers` (no lock across await); factory `with_memory`; regression test seeds two plan markers + one unrelated.

**Watchdog capture (logic, aside from H1/L1)**

- `AlignedContext` `repr(C, align(16))` addresses windows-sys CONTEXT align-8 / `ERROR_NOACCESS` (998).
- `ResumedOnDrop` resumes on every success-suspend path (including early `GetThreadContext` failure returns).
- Outer retry loop resumes between attempts for mid-syscall `ERROR_PARTIAL_COPY`.
- `StackWalk64` passes `SymFunctionTableAccess64` / `SymGetModuleBase64` (x64 unwind).
- WCT hand bindings: `WAITCHAIN_NODE_INFO` flat layout size matches MSVC (`280` with trailing pad); enums 1-based; AdvAPI32 link; session closed after call.
- Report sections + empty-platform fallbacks; helper-thread stack test (not self-suspend); wait-chain graceful test.
- Capture is best-effort empty-vec, no panic paths evident.

**browser_webview notes / locking**

- `note` no-ops when `state.watchdog` is None; short `std::sync::Mutex` on the ring only.
- Browser path holds a Tokio mutex; watchdog never takes the browser mutex → no AB-BA with the ring.
- Ensure create/move, navigate, overlay enter/exit, destroy note before controller work; builders + `url_host` unit-tested; ring ordering test present.

**Other constitution**

- No `#[allow(...)]` in the diff.
- New `pub` APIs reviewed (`with_role`, `role`, `set_reviewer_allowlist`, `reviewer_failure_pending` setters, `BacklogListTool`, `supersede_plan_markers`, etc.) have doc comments; minor inner `wct::object_status` lacks one (private module — not filed separately).
- No bash/Linux shell assumptions in library code; Windows enrichment remains the intended gate (once H1 is fixed).
- Scope matches the plan; `.coding/plans/stack.json` churn is session bookkeeping only.

---

### Test coverage vs changed paths

| Area | Coverage |
|------|----------|
| Reviewer strict filter / schemas memory exclusion | `tool/mod.rs`, `workflow/mod.rs` |
| Reviewer allowlist intersection + mutation exclusion | `ipc/spawn.rs` |
| finish main-only; respawn gate; ask_user clears latch | `agent/tests.rs` |
| Failed-reviewer text + parent latch (GUI text unit + console integration) | `events.rs`, `console.rs` |
| backlog_list behavior + visibility | `backlog.rs`, `tool/mod.rs` |
| abandon marker supersede | `plan.rs` |
| Continue `failed` flag | `useAgentStore.test.ts` |
| Stack walk / WCT / report formatting / ring notes | `watchdog.rs`, `browser_webview.rs` |

Gaps (non-blocking): no automated non-Windows `cargo check` for H1; no `size_of::<WaitChainNodeInfo>()` static assert against an expected 280; set_rect pre-call note untested by design.

---

### Summary

Ship blockers: **H1** (gate Windows imports/items so macOS/Linux still build). Then address handle close (L1), marker title formatting (L2), set_rect note ordering or docs (L3), and README sync (L4). Reviewer hardening, failed-reviewer protocol, Continue UX, abandon supersede, and the stack/WCT design (RAII resume, align(16) CONTEXT, WCT layout) otherwise look correct and well tested on Windows.
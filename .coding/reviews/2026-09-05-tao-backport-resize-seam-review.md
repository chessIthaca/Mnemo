## Verdict: FINDINGS (1 high, 2 low)

Review scope: ALL uncommitted changes (13 modified files + untracked `vendor/`, `src-tauri/tests/`,
`frontend/src/components/resizeHandleMotif.test.ts`, `.coding/knowledge/` records, plan
`ef05c3d9`), reviewed via `git diff HEAD` + direct reads of every untracked path. The vendored
patch was checked against the actual upstream PR #1215 diff (fetched from GitHub) — not just
against its own documentation.

---

### HIGH-1: DeltaBatcher flush timer is re-armed per event — starved indefinitely under sustained streams (`src-tauri/src/ipc/events.rs:413-425`)

`tokio::time::sleep(DELTA_FLUSH_INTERVAL)` is constructed inline in the `tokio::select!`
expression, i.e. **re-created (deadline reset to now+16 ms) on every loop iteration** — and every
received event starts a new iteration. A fresh `Sleep` returns `Pending` until its deadline while
`rx.recv()` returns `Ready` the moment an event is queued, so recv wins whenever an event arrives
within the 16 ms window. The timer therefore only fires after a **≥16 ms idle gap on the fan-in
channel** — it is not a 16 ms maximum batch age.

Consequence: an aggregate delta rate above ~62.5 events/s postpones the flush indefinitely. This
channel is the fan-in for **all** agents, so one fast model (>62 tok/s — common for modern
providers/local models) or 2–3 concurrently streaming agents (30–40 tok/s each, a headline
`spawn_agent` scenario) starves the timer for the whole burst. Pending deltas then wait for a
structural event (turn end) or the 64 KiB cap — live streaming text can stall for many seconds,
which contradicts the documented semantics (`DELTA_FLUSH_INTERVAL` doc: "16 ms ≈ one 60 Hz frame
… adds no perceptible latency", events.rs:203-208; PLAN.md "Agent event delivery" row: "16 ms
timer") and is a streaming-UX regression vs. the pre-change per-token emission, in exactly the
multi-agent cases the app targets. No data loss or reordering (cap + structural flush bound it;
frontend concatenation is append-safe), so severity is UX/contract, not corruption.

Note the in-module `delta_batcher_tests` cover the pure batcher well but not this loop, which is
where the defect lives.

**Fix:** arm the deadline once when the first delta lands and do not reset it per event — e.g.
keep an `Option<Pin<Box<tokio::time::Sleep>>>` (created when `is_empty()` flips false, taken/reset
only after it fires or the batcher drains), or compute
`tokio::time::sleep_until(tokio::time::Instant::now() + DELTA_FLUSH_INTERVAL)` at arm time and
pin it across iterations. (A small async test with a paused tokio clock — `tokio::time::pause` +
`advance`, available via tokio's `test-util` feature already in the dev-dependencies — can then
pin the "flushes ≤16 ms after the first delta even while more deltas arrive" behavior.)

### LOW-1: pending deltas dropped if all senders drop without a terminal event (`src-tauri/src/ipc/events.rs:426-429`)

If every sender is dropped while deltas are pending (agent aborted/cancelled without emitting
`Finished`/`Error`/`Exited`), the select resolves on `recv() → None` and the loop breaks **without
a final `flush_all()`** — the trailing batch is silently lost. Normal terminal events are
structural and flush first, so this only bites abrupt teardown, but it is the exact "final tokens
of a cancelled stream vanish" shape: on the `None` branch, run `flush_all()` and emit before
`break`.

### LOW-2: `vendor/` not in `[workspace] exclude` (root `Cargo.toml:90-94`)

The workspace declares `members = ["src-tauri"]` but not `exclude = ["vendor"]`, so any cargo
invocation from inside `vendor/tao` (e.g. cd-ing there to diff against upstream or check the
tree) fails with "current package believes it's in a workspace when it's not". One-line fix:
`[workspace] exclude = ["vendor"]`. (Workspace builds are unaffected — root/`-p mnemo-app` builds
and tests are green.)

---

## Verified correct (both change sets)

**tao backport fidelity vs upstream PR #1215** (diff fetched and compared hunk-by-hunk):
- `event_loop.rs:878-921` — `peek_next_key_message` / `next_key_message_for_keyboard` (incl. the
  Alt+F4 `VK_F4` exclusion) / `more_ime_char_coming` are byte-equivalent to upstream, including
  the re-entrancy WARNING doc.
- Hoist sites correct: keyboard peek at `event_loop.rs:1028` **before**
  `KEY_EVENT_BUILDERS.lock()` at `:1031`; IME peek at `:1060` **before** `window_state.lock()` at
  `:1062` — exactly the upstream ordering that removes the non-reentrant `parking_lot`
  self-deadlock.
- `keyboard.rs:78-85` — `process_message` takes `next_key_message: Option<MSG>` (no `hwnd`), no
  `PeekMessageW` anywhere in the file, `get_kbd_state()` hoisted out of the `LAYOUT_CACHE` lock
  with lock scopes narrowed to `from_message`/`finalize` (lines 116-155), WM_CHAR branch derives
  `more_char_coming` from the passed peek (185-188) — matches upstream.
- `minimal_ime.rs` is a byte-match of the upstream rewrite; `keyboard_layout.rs:23-25` adds the
  free `get_agnostic_mods()` and privatizes the method; `update_modifiers` uses it
  (`event_loop.rs:852`).
- `vendor/tao/Cargo.toml` is the crates.io-normalized 0.35.3 manifest with only the version
  renumbered to 0.35.4 — satisfies `tauri-runtime-wry`'s `^0.35` pin (0.35.4 < 0.36.0), `windows`
  0.61 deps untouched; `Cargo.lock` shows tao as path-patched (no source/checksum).
- **Multi-platform neutrality:** every patched file is under `src/platform_impl/windows/`
  (compiled only on Windows); macOS/Linux paths of the vendored tree are untouched by the patch;
  `tao_backport.rs` reads source files present on all checkouts, so it guards cross-platform.
- **Regression guard genuinely fails on pristine 0.35.3:** `keyboard.rs`/`minimal_ime.rs` contain
  `PeekMessageW`, the helper-name lookup panics, and the manifest says 0.35.3 — all four tests
  fail without the fix. Source-level guard (not runtime) is properly justified: third-party race,
  no deterministic reproduction. All test fns documented.

**DeltaBatcher pure semantics** (all correct, well-tested in `delta_batcher_tests`):
per-(agent, kind, tool-call-index) keying; flush sort Reasoning→Text→ToolCallArg (variant order
asserted); structural events flush same-agent deltas **before** passing through (order preserved);
other agents' buckets never touched by an unrelated agent's structural event (tested); 64 KiB cap
flushes immediately and mirrors the frontend's `MAX_BUFFERED_DELTA_CHARS = 64 * 1024`
(`deltaFlush.ts:11` — verified); empty buckets removed, never emitted; empty-string deltas can't
leak (bucket removed on flush). Pre-emit side effects (watchdog ring — deltas correctly skipped by
`watchdog_note`, running-state flips, Run-All resolution, approval/question sender storage) all run
per-event on the original event before `push`, so their ordering vs. the UI stream is unchanged.
Cap path correctly includes the just-pushed bucket.

**Resize-seam fix:** sweep for `cursor-*-resize` across all `.tsx` finds exactly five handles —
App.tsx:805 (`col`), InflightBar.tsx:211 (`ns`), FileViewer.tsx:374 / GraphView.tsx:779 /
LlmTraceView.tsx:979 (`row`) — all now paint `bg-bg-primary` between their hairlines; the only
other `-resize` hits are comments, textarea auto-resize, and the body cursor set during drag —
**no handle missed**. Hover wash intact everywhere: `hover:bg-cyan-500/20` retained on all five,
and `.hover\:…:hover` (specificity 0,2,0) beats `.bg-bg-primary` (0,1,0), so the cyan wash still
replaces the painted background; `transition-colors` now uniform (InflightBar gained it). Grip
pill `bg-slate-500 group-hover:bg-slate-400` present in all five. The source-contract test
(10 assertions) is registered in `frontend/vitest.config.ts:56` and demonstrably runs (10/10 in
the verified `npm test`); `?raw` imports are vite-native with an existing precedent
(`MainPanel.tabStyle.test.ts`).

**Constitution compliance:**
- Doc comments on all new functions incl. private ones (`emit_payload`, batcher methods, test
  fns); no `#[allow]` added anywhere; warning-free build proven by green `cargo test` under
  `#![deny(warnings)]` (root 1483 passed, `-p mnemo-app` 165+4 passed).
- Documentation sync: README hang-watchdog paragraph updated (root cause + fix pointer), PLAN.md
  gains the tao-backport and Agent-event-delivery rows plus the enriched watchdog row,
  `watchdog.rs` module doc extended, `vendor/tao/PATCHES.md` documents provenance/invariants,
  both BUG knowledge records present under `.coding/knowledge/bug/` with
  symptom→root-cause→fix→regression-test; the prompts-toml `superseded`/`MERGED` decision pair is
  internally consistent (old record flagged, new record carries the `supersedes` pointer + merge
  commit).
- Security: the patch introduces no new native calls (same `PeekMessageW` with identical
  arguments, merely hoisted); no network/exec surface added; `[patch.crates-io]` scope is
  workspace-wide by design and documented.

## Observations (no action required)

- `Cargo.lock` shows `windows-sys` downgrades across unrelated crates (0.61.2 →
  0.60.2/0.59/0.52/0.48) — a side effect of the registry→path re-resolution for tao; the
  coexisting versions are legal and everything builds/tests green, but reviewers of a future
  `cargo update` may be surprised.
- `tao_backport.rs` asserts the keyboard-path hoist by byte offset but has no equivalent ordering
  assertion for the IME path (`more_ime_char_coming` before `window_state.lock()`); the
  upstream-fidelity check above covers it today — worth adding if the vendor tree is ever
  refreshed.
- Plan `ef05c3d9` step 4's regression-test field is populated; root cause + BUG records are in
  place for both bugs, satisfying the bug-plan review extras.

**Summary:** the tao backport and the resize fix are ship-quality; fix HIGH-1 (timer starvation)
and the two LOWs before commit — all three are small, localized to `events.rs`/`Cargo.toml`.

## Verdict: FINDINGS (0 high, 2 low)

The headline+body shape contract is correctly implemented and enforced at exactly the two production write paths; the image-only exemption reasoning holds against the code; all boundaries are pinned by tests; docs are synced; no security or platform issues. Two low-severity findings: a draft-restore clobber edge in the frontend, and hardcoded limits in the tool-schema prose.

## Scope reviewed

All uncommitted changes on `wt/agenticcoding` (8 modified files + 3 untracked): the shared validator in `src/backlog.rs`, the agent tool in `src/tool/workflow/backlog.rs`, the IPC command in `src-tauri/src/ipc/backlog_cmds.rs`, the error surface in `frontend/src/components/views/BacklogView.tsx`, the new `frontend/src/lib/tauri.test.ts`, the vitest allow-list entry, README/PLAN docs, and the `.coding/` side-car changes.

## Verified correct

### Validator logic (`src/backlog.rs:76-109`)

- **Boundaries**: exactly-100-char headline accepted / 101 rejected (`normalize_rejects_overlong_headline`, src/backlog.rs:1133-1147); exactly-4000-char item accepted / 4001 rejected (`normalize_rejects_oversize_text_and_accepts_the_boundary`, :1149-1160; tool-side `oversize_text_errors` pins the 4001 single-line case → "4000" error). The total-length check runs *before* the headline check, so an oversize single line gets the length error — pinned.
- **Whitespace-only**: rejected (`text.trim().is_empty()`, :78-80). `"Headline\n\n"` and `"   \n  \n "` correctly reject — the whole-string trim makes them empty/single-line (`normalize_rejects_headline_only_with_trailing_blank_lines`, :1126-1130).
- **Trailing whitespace on the headline**: `trim_end()` before the length check (:89) — lenient and correct. The stored text keeps internal whitespace (only the whole-string ends are trimmed), which `splitHeadline` (BacklogView.tsx:251-260) renders fine.
- **CRLF**: `split('\n')` leaves `\r` at line ends; `headline.trim_end()` (:89) and `line.trim()` (:99) strip it, so CRLF text validates identically to LF; the frontend split uses `/\r?\n/`. (The web textarea normalizes to LF per the HTML spec, so CRLF only arrives via direct IPC — handled anyway.) The 4000-char cap counts `\r`s — stricter, not looser; acceptable.
- **Body detection is structural** (any non-whitespace line after the first counts) — matches the doc comment ("the blank line is described, not enforced") and the rendering (`Headline\nbody` renders identically to the blank-line form).

### Image-only exemption (`src/backlog.rs:121-126`) — design reasoning holds

BacklogInput's guard (BacklogView.tsx:140) deliberately lets whitespace-text + images through, and the bare `normalize_item_text` would have rejected those with "must not be empty" — silently breaking the UI's image-only add path. `normalize_new_item_text` exempts exactly that case (returns `Ok("")`), delegates everything else. The frontend guard and the backend exemption are exact inverses — no gap in either direction (empty text + no images is guarded client-side AND rejected server-side). The regression test `new_item_image_only_passes_with_empty_text` (src/backlog.rs:1165-1177) fails if the exemption is dropped back to the text-only validator. The agent tool takes no images param, so it correctly uses the text-only validator directly (src/tool/workflow/backlog.rs:143).

### Both write paths — and only those

Repo-wide search for `BacklogStore::add` callers and `BacklogItem {` construction confirms exactly two production add sites: the agent tool (src/tool/workflow/backlog.rs:148, validated at :143) and the IPC command (src-tauri/src/ipc/backlog_cmds.rs:172, validated at :169). All other hits are tests, fixtures, or the legacy-JSON load/migration path (src/backlog.rs:278 — a read path, not a write path). No bypass path exists.

### Grandfathering

Store `add` (src/backlog.rs:354) and `edit` (:596) remain unvalidated; `backlog_edit` (backlog_cmds.rs:256-266) deliberately unvalidated, documented in the command's doc comment (:155-157). Existing single-line items stay editable without forced restructuring.

### IPC error path

`IpcError::from(String)` (src-tauri/src/ipc/error.rs:47-51) yields `{kind:"error", message}`; the frontend `errMsg` (frontend/src/lib/tauri.ts:137-145) extracts `.message` — pinned by the new DTO test in tauri.test.ts:25-29. The rejection returns before the store lock and before `emit_backlog_changed` — no partial write, no spurious UI refresh event. The agent tool's rejection likewise returns before the notifier fires (pinned by the rejected-add case in `notifier_fires_after_each_successful_add`, src/tool/workflow/backlog.rs:606-609).

### Frontend error surface (BacklogView.tsx:82-88, 139-158, 200-206, 231-238)

- Error cleared at attempt start (:145) and on textarea change (:205) — no stuck error on a successful retry or on edit.
- Draft restore captures the raw (untrimmed) text + images before the optimistic clear and restores both on catch — no stale closure in the normal flow, no double-restore.
- `role="alert"` for screen readers; `backlogAdd` (tauri.ts:1556-1561) propagates the rejection (no catch), and BacklogInput's `handleAdd` is its only frontend caller.

### Security

No panics on adversarial input (`unwrap_or("")` at :89, `chars().count()`, no indexing/slicing); error messages expose only lengths and the shape description, never the input content; validation runs before any lock/disk I/O; O(n) work bounded by the length check. Image contents are validated where they were before (`write_image_files`) — unchanged surface.

### Documentation sync

README.md Backlog bullet — the new shape-contract sentence is accurate (both write paths, inline rejection + draft restore, image-only exempt, grandfathered). PLAN.md "Backlog + Run-All" paragraph accurate (shared validator, image-aware variant, grandfathering, backlog_edit non-validation). Module docs updated in all three code files. `useAgentStore.ts:589` ("Cleared after a successful `backlogAdd`") remains accurate. The new knowledge record (vitest allow-list) is accurate.

### Multi-platform neutrality

Pure Rust string logic + React state — no paths, no OS APIs, no `cfg(windows)`. Clean.

### Test quality — regression value confirmed

Every new test fails without the fix it pins: the two tool-rejection tests would see `success: true` pre-change; the schema test pins the description keywords; the source-contract test (`backlog_add_routes_through_the_shape_validator`, backlog_cmds.rs:508-539) pins the exact call including `&images` and the IpcError mapping — its slice logic is sound (the first `pub async fn backlog_add` occurrence is the real function at :159, and the first `\n}\n` after it is the function's own closing brace since inner braces are indented), consistent with the repo's established `include_str!` pattern. The 4 pre-existing single-line test texts were correctly updated to the shape (they exercise persistence/notifier wiring, not the validator). The vitest allow-list entry is present (frontend/vitest.config.ts:81) — without it the file would silently not run.

Accepted limitation (not a finding): the error surface itself (handleAdd's state transitions) has no component-level test — handleAdd is a non-exported async closure and the suite is node-environment pure-unit tests; the errMsg DTO-extraction tests pin the part the surface depends on. Consistent with the suite's design.

## Findings

### LOW-1 — Draft restore can clobber text typed during the in-flight add

`frontend/src/components/views/BacklogView.tsx:154`

`handleAdd` optimistically clears the draft (:143-144), then on rejection unconditionally does `setText(input)` / `setAttachedImages(images)`. If the user types into the now-empty textarea while the `await backlogAdd` is still in flight — possible when the failure is slow (store-lock contention during a concurrent run-all persist, slow disk) rather than the fast validator rejection — the restore overwrites their new keystrokes with the old rejected draft. The validator-rejection path returns before the store lock, so the common window is a few ms (untypeable); this only bites on slow non-validation failures. Suggested guard: restore only when the draft is still empty, e.g. `if (!useAgentStore.getState().backlogDraft) setText(input);` (same for images). Severity LOW: rare window, and the pre-change behavior lost the draft entirely on any failure, so this is still a strict improvement.

### LOW-2 — Tool-schema prose hardcodes the limits the consts enforce

`src/tool/workflow/backlog.rs:122`

The text-param description hardcodes "Max 4000 chars total" and "headlines over 100 chars" while enforcement uses `MAX_ITEM_TEXT_CHARS` / `MAX_HEADLINE_CHARS` (src/backlog.rs:45,51). If either const is ever tuned, the description — the model's source of truth — silently drifts, and `add_schema_declares_the_required_shape` (:565-578) only asserts the keywords, not the numbers. This follows the pre-existing convention (the old description also hardcoded 4000 against a private const), so it is drift-risk hardening rather than a regression: build the description with `format!` from the consts (`json!` accepts expression values).

## Side-car changes reviewed

- `.coding/backlog.jsonl`: item 45a4eb88 gained harness bookkeeping fields only (note/plan_id/plan_title from the steer-halt — text unchanged); new item 3b395d27 (browser stop/reload buttons, user-requested, unrelated to this plan) is a legitimate add — and itself carries the headline+body shape.
- `.coding/knowledge/decision/2027-01-07-frontend-vitest-*.md`: accurate decision record for the allow-list discovery (71 → 72 files).
- `.coding/plans/024a06e1.md`: the plan file for this work.

Both findings are LOW; neither blocks landing.

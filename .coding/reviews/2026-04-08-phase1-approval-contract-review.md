# Review: Phase 1 approval contract (Architecture remediation)

**Reviewer:** read-only closing-sequence reviewer (`spawn_agent`)  
**Date:** 2026-04-08  
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`), including untracked files — not only Phase 1.

## Scope observed

**Modified**
- `.coding/plans/39f3c881-55a0-4147-a010-9199f6e83aec.md` (plan checkboxes only)
- `frontend/src/components/chat/ApprovalPrompt.tsx`
- `frontend/src/components/chat/DiffView.tsx`
- `frontend/src/components/layout/InputBar.tsx`
- `frontend/src/components/layout/MainPanel.tsx`
- `frontend/src/components/views/DiffViewer.tsx`
- `frontend/src/hooks/useAgentStore.ts`
- `frontend/vitest.config.ts`
- `src/agent/approval.rs`
- `src/agent/dispatch.rs`
- `src/agent/tests.rs`
- `src/agent/turn.rs`
- `src/tool/agent/file_edit.rs`
- `src/tool/agent/file_write.rs`
- `src/tool/mod.rs`

**Untracked**
- `frontend/src/components/chat/DiffView.test.ts`
- `frontend/src/hooks/useAgentStore.preview.test.ts`

## Phase 1 intent (checklist)

| Item | Intent | Assessment |
|------|--------|------------|
| **1a** | Populate `ApprovalPreview` on live path via `Tool::approval_preview` + wire in `dispatch.rs`; pure preview for file_edit/file_write | **Met** — trait hook + dispatch wiring + pure prepare paths + unit/integration tests |
| **1b** | FE consume preview; args fallback; LCS size guard | **Met** — ApprovalPrompt/DiffViewer prefer preview; `LCS_CELL_BUDGET`; vitest coverage |
| **1c** | DenyAll complete: turn latch; FE Deny all + Shift+D; honor `approve()` bool; deny on `/clear` | **Mostly met** — latch + FE + honor bool + clear deny; see Medium on `/clear` vs interrupt race and DenyAll × MAX_RETRIES |
| **1d** | Multi-agent visibility: tab badge; non-active banner; no auto-steal | **Met** — badge + banner + explicit Switch only |

---

## Findings by severity

### Critical

**No critical findings.**

No path that auto-approves core ops, bypasses the approval oneshot, writes during preview, or steals focus onto a pending approval was found in this diff.

---

### High

#### H1 — DenyAll synthetic errors count toward `MAX_RETRIES` and can abort the turn after 3 denials

**Files:** `src/agent/dispatch.rs` (latched skip / DeniedAll → `ToolResult::error`), `src/agent/turn.rs` (~615–620, ~700–720), `src/agent/mod.rs` (`MAX_RETRIES = 3`)

**What happens**
1. User answers **Deny all** on the first approval → that call returns `ToolResult::error(...)` and sets `deny_all_latched`.
2. Remaining calls in the same batch (and later iterations) also return errors (`… call skipped` / `… denied`).
3. `run_turn` treats every `!result.success` as a consecutive tool error and increments `tool_error_count`.
4. After **3** consecutive failures the turn aborts with  
   `Aborting turn: 3 consecutive tool errors (the model may be stuck).`

**Why this is high**
- Deny-all is a deliberate user safety choice, not a stuck-model failure mode.
- A single Deny-all on a batch of ≥3 tools (or one deny-all + two more latched skips, or a later iteration that emits more tools while latched) can end the turn with a misleading “model may be stuck” error instead of letting the model see the denials and stop/replan cleanly.
- The new integration test `deny_all_latches_and_skips_remaining_tool_calls` only drives two `execute_tool_call`s directly — it never exercises the `MAX_RETRIES` path in `run_turn`.

**Suggested fix (for main agent)**  
Do not increment `tool_error_count` for user-denial / deny-all-skip results (match on known denial prefixes, a structured `ToolResult` flag, or skip counting when `deny_all_latched` was already true / outcome was Denied/DeniedAll). Alternatively reset or freeze the counter when DenyAll latches. Add a turn-level test that DenyAll on a multi-tool batch does not emit the MAX_RETRIES abort.

---

### Medium

#### M1 — `/clear` deny-then-interrupt can leave the agent mid-batch after only one denial (not full DenyAll)

**Files:** `frontend/src/components/layout/InputBar.tsx` (~234–258), `src/agent/approval.rs` (`await_approval` Interrupt path), `src/agent/dispatch.rs`

**What happens**
On `/clear` with a pending approval the FE:
1. `approve(toolCallId, "deny")` (single deny, not `deny_all`)
2. optionally `interrupt(agentId)` if `running`
3. clears the conversation store

Backend `await_approval` races the oneshot vs `Interrupt`. Whichever wins:

| Winner | Effect |
|--------|--------|
| **Deny** then later interrupt (or interrupt after tool result) | That one call is denied; `deny_all_latched` stays **false**. Interrupt may stop the stream/turn depending on where the loop is. |
| **Interrupt first** | Outcome `Interrupted` → `"interrupted while awaiting approval"`; oneshot may still be resolved by the late deny (harmless if already finished waiting). Latch still false. |

**Why this matters**
- Phase 1c text is “deny pending on `/clear`” — the hang is fixed (good).
- But clear does **not** latch DenyAll. If interrupt fails/races and the turn continues with more tool calls in the same batch, the user can get **another** approval prompt after “clearing,” or tools can run if safety mode would auto-approve later calls.
- Using `"deny_all"` on clear would better match “I’m done with this turn” and set the latch if the deny wins the race before further `execute_tool_call`s.

**Suggested fix**  
On clear, resolve with `"deny_all"` rather than `"deny"`, and/or ensure interrupt is always sent when any pending approval exists (not only when `running`), and that interrupt is ordered so the turn cannot proceed to the next tool after UI wipe. Document the intended race winner.

#### M2 — `file_write` overwrite preview is always `NewFile` (full body), never a diff against existing content

**Files:** `src/tool/agent/file_write.rs` (`prepare_for_approval` → always `ApprovalPreview::NewFile`), `frontend/.../ApprovalPrompt.tsx` / `DiffViewer.tsx` (`new_file` → green add-only view)

**What happens**  
Overwriting an existing file still shows the entire new content as “+” lines. The user does not see what is being replaced (no unified diff vs on-disk bytes).

**Why medium (not high)**  
Not a security bypass — approval still gates the write. It **is** an approval-UX correctness gap relative to H5 (“show what will change”). Large overwrites are harder to review than `file_edit` diffs; args fallback has the same limitation.

**Suggested fix**  
If the path exists and is readable, build `ApprovalPreview::Diff` via `compute_diff(path, old, new)` (same helper as `file_edit`); keep `NewFile` only for create-only paths.

#### M3 — Preview build can do heavy CPU/IO on the agent task before the prompt appears

**Files:** `src/agent/dispatch.rs` (~141–144), `src/tool/agent/file_edit.rs` (`prepare_for_approval` reads whole file + `similar` diff), `src/tool/agent/file_write.rs` (clones full content into preview)

**What happens**  
`approval_preview` runs synchronously on the dispatch path before `ApprovalRequest` is emitted. Multi‑MB files mean:
- full read + LCS-style diff on the async runtime worker,
- large `ApprovalPreview` JSON over IPC,
- FE still renders full unified diff / new-file body (no line budget on the Rust→UI path; only client LCS fallback is guarded).

**Why medium**  
Correctness of the gate is fine (preview failures → `None`, approval still required). Risk is UI/runtime jank or memory spikes on large edits — adjacent to Perf H4, which only guarded the **client** LCS path.

**Suggested fix**  
Cap preview size (truncate diff/content with a clear meta header), or spawn blocking diff off the async worker; optionally skip embedding huge bodies and keep args-only fallback with a “preview too large” note.

---

### Low

#### L1 — `parseUnifiedDiff` mis-classifies content lines that begin with `---`, `+++`, or `@@`

**File:** `frontend/src/components/chat/DiffView.tsx` (`parseUnifiedDiff`, ~35–41)

Rust `similar` hunks prefix every content line with `+`/`-`/` `, so normal diffs are fine. A content line whose **display** text after the prefix is irrelevant; the bug is only if a **raw** diff line (no space prefix) equals `---…` etc., or if a future producer omits the leading space on context. Rare; worth a note or stricter header detection (`/^--- /`, `/^\+\+\+ /`, `/^@@ /`).

#### L2 — Client LCS “budget” fallback still materializes full remove+add lists

**File:** `frontend/src/components/chat/DiffView.tsx` (`computeDiff`, ~73–86)

When `n*m > LCS_CELL_BUDGET`, the code avoids the DP table (good) but still pushes `n+m` line React nodes. ~1400+1400 lines is OK; pathological multi‑100k-line args without a Rust preview can still freeze paint. Prefer relying on Rust preview + a hard max lines rendered.

#### L3 — `MainPanel` “other approval” banner shows only the first non-active agent

**File:** `frontend/src/components/layout/MainPanel.tsx` (~28–35, ~103–125)

`Object.entries(agents).find(...)` — if two subagents both wait, only one banner. Tabs still badge each agent (H1/H4 largely OK). Optional: “N agents need approval” or cycle.

#### L4 — `Object.entries` order is insertion order, not sorted numeric id

**File:** `frontend/src/components/layout/MainPanel.tsx` (~28–35)

Comment says “stable order by id”; JS object key order for integer-like keys is usually ascending, but the code does not sort. Low practical impact.

#### L5 — ApprovalPrompt local `resolved` UI can disagree with store if `approve()` returns false

**File:** `frontend/src/components/chat/ApprovalPrompt.tsx` (~57–69)

Honoring the bool is correct (1c). If `ok === false`, buttons stay active with no user-visible error — better than false “approved,” but a toast/error would help. Pre-existing pattern; slightly more visible now that the bool is checked.

#### L6 — No `key={toolCallId}` on `ApprovalPrompt`

**File:** `frontend/src/components/chat/Conversation.tsx` (~70–71)

If a second approval arrives without unmount (edge), React may reuse state (`showDiff` / `resolved`). Normal path clears `pendingApproval` between calls so the component unmounts. Low.

#### L7 — Plan file checkbox noise in the same uncommitted tree

**File:** `.coding/plans/39f3c881-55a0-4147-a010-9199f6e83aec.md`

Steps 5–9 marked done while step 10 (this close) is open. Fine for WIP; keep plan edits out of the Phase 1 feature commit or include intentionally.

#### L8 — Tests do not cover `file_edit` live preview on the dispatch path

**File:** `src/agent/tests.rs` (`dispatch_approval_request_includes_file_write_preview` only)

`file_edit` has unit tests for `prepare_for_approval` / trait hook; live `ApprovalRequest` integration is write-only. Asymmetry, not a known bug.

#### L9 — Protected-path preview failure is silent (`Option` swallow)

**Files:** `src/tool/agent/file_edit.rs` / `file_write.rs` (`approval_preview` → `.ok()`), `src/agent/dispatch.rs`

Protected targets get `preview: None` but still go through normal approval; execute still refuses. User may approve a “blind” call that then errors — acceptable; optional explicit preview error string would be clearer.

---

### Security

**No new security findings in this diff.**

- Preview paths re-check sandbox validation and `is_protected_write_target` before read/diff; `prepare_for_approval` does not write (`file_edit` / `file_write` purity tests assert this).
- DenyAll latch only forces synthetic denials; it does not weaken `never_auto` / safety rules / workflow `ToolFilter` (those still run when not latched; when latched, tools never execute).
- FE multi-agent UI does not auto-switch focus to a pending approval (matches UI H4).
- `approve` IPC still resolves by `tool_call_id` only (pre-existing); this change does not broaden that surface.

Constitution notes for this diff:
- Public Rust hooks (`Tool::approval_preview`, prepare helpers) have doc comments.
- No agent-initiated merge/push.
- Review report written under `.coding/reviews/` only (reviewer read-only on project source).

---

### Constitution / process

| Check | Result |
|-------|--------|
| Doc comments on new public API | OK (`Tool::approval_preview`, prepare_for_approval updates) |
| Tests added for 1a/1c core behavior | OK (with gaps H1, L8, M1) |
| FE unit tests + vitest include globs | OK (`DiffView.test.ts`, `useAgentStore.preview.test.ts`) |
| Commit to main | N/A (uncommitted on feature work) |
| Reviewer edited only `.coding/reviews/` | Yes |

---

## What looks solid (non-findings)

1. **Live preview wiring** — `dispatch` calls `tool.approval_preview` only on the interactive approval path (after filter + before oneshot); auto-approved rules skip preview (fine).
2. **Serde shape** — `ApprovalPreview` `#[serde(tag = "kind", rename_all = "snake_case")]` matches FE `kind: "diff" | "new_file"`.
3. **IPC** — `SerializableAgentEvent::ApprovalRequest` forwards `preview`; oneshot still stripped at the boundary.
4. **FE preference order** — preview → args LCS / content; `lastDiff` snapshots preview before clearing `pendingApproval` (good for post-apply Diff panel).
5. **Deny all UX** — button + Shift+D; `A` ignores Shift; editable-focus guard preserved.
6. **Honor `approve()` bool** — Approve/Deny/Deny all/Mark Safe/Allow for project all gate `setResolved` on `ok`.
7. **Multi-agent chrome** — yellow tab badge + “approve” pill + sticky banner; `setActiveAgent` only on click.
8. **`file_write` new-file preview** — `validate` then `validate_for_creation` so creates get previews (fixes the old validate-only hole for missing paths).

---

## Summary for main agent

| Severity | Count | Action |
|----------|-------|--------|
| Critical | 0 | — |
| High | 1 | **Must fix:** DenyAll / latched skips must not drive `MAX_RETRIES` turn abort (H1) |
| Medium | 3 | Fix or explicitly justify: clear→deny_all/interrupt (M1), overwrite diff (M2), preview size/CPU (M3) |
| Low | 9 | Optional polish |

**Not “no findings.”** Treat **H1** as blocking for Phase 1 close; address or written-justify M1–M3 before commit per constitution (“fix every finding… only skip if factually wrong with explicit justification”).


---

## Remediation applied (main agent, Phase 1 close)

| Finding | Fix |
|---------|-----|
| **H1** | `turn.rs`: `is_user_denial_tool_output` — user deny / deny-all skip / interrupt / channel-closed do not increment `tool_error_count`. Test: `user_denial_output_helper_matches_dispatch_messages`. |
| **M1** | `/clear` uses `deny_all` + interrupt when pending **or** running. |
| **M2** | `file_write::prepare_for_approval` returns `Diff` on overwrite; `NewFile` only for create. Test: `prepare_for_approval_overwrite_returns_diff`. |
| **M3** | Preview char budget (~120k) with truncation marker on file_edit/file_write previews. |
| **L1** | Stricter unified-diff header parse (`--- ` / `+++ ` / `@@`). |
| **L2** | LCS fallback caps painted lines (`LCS_FALLBACK_MAX_LINES`). |
| **L3/L4** | Banner sorts by id; shows count when multiple agents wait. |
| **L5** | ApprovalPrompt shows error when `approve()` returns false. |
| **L6** | `ApprovalPrompt` keyed by `toolCallId`. |
| **L7–L9** | Plan checkbox intentional in commit; L8/L9 accepted residual (write path covered; silent preview None OK). |

Post-fix: `cargo test --lib` 505 ok; vitest 17 ok; `tsc --noEmit` ok.

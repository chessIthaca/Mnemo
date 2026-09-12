## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan 33383b61 "Backlog plan chip: human-friendly clickable plan identifier" (backlog f45513b2, user-reported).

**Summary:** The implementation matches the plan's stated design on every axis I could verify — title recording at the stamp moment (correct plans-dir access, correct file-format contract, correct write-order, correct refresh-on-every-Executing-entry semantics), display-only note stripping with the stored note and `extract_checkpoint_sha` untouched, payload/TS/fixture/contract alignment including the skip-if-none absent case, sound SSR-limitation adaptation, real regression-pinning tests, multi-platform-neutral clipboard usage, and no new attack surface. Two LOW findings: a doc-comment misattachment in `run_all.rs` (the stamp's doc now documents the wrong function) and a missing one-phrase README touch for the new user-visible chip affordance.

**Scope reviewed:** `src/backlog.rs`, `src/project/git_ops.rs` (doc comment only), `src-tauri/src/ipc/{run_all.rs, backlog_cmds.rs, contract_fixtures.rs}`, `frontend/src/lib/{types.ts, ipc-contract.test.ts, ipc-fixtures/dto-backlog-changed-payload.json}`, `frontend/src/components/views/{BacklogView.tsx, BacklogView.test.tsx, BacklogView.test.ts}`, `.coding/backlog.jsonl` (bookkeeping), untracked `.coding/plans/33383b61.md`. Cross-checked against the writers/emitters the contract depends on: `src/workflow/plan_file.rs` (heading format), `src/workflow/mod.rs` (`top_plan_id`, `create_plan_with_kind`), `src/agent/turn.rs` (`emit_workflow_state`), `src-tauri/src/ipc/events.rs` (main-agent gate), `src/project/mod.rs` (`plans_dir` field).


## Findings

### LOW 1 — Doc-comment misattachment in `run_all.rs`: `stamp_backlog_in_flight` lost its doc; `plan_title_from_file` inherited the wrong one

`src-tauri/src/ipc/run_all.rs` lines 2098–2127. The new `plan_title_from_file` was inserted **between** `stamp_backlog_in_flight`'s existing doc comment and the function itself, with its own doc lines appended directly (no blank `///` separator). Consecutive `///` lines form ONE doc comment, so:

- The merged block (2098–2117) now attaches to `plan_title_from_file` (line 2118). Its rendered doc begins *"Stamp the run-all's in-flight item (or the single-dispatch in-flight item) `Pending` → `InFlight`, preserving its note…"* — that describes the **stamp**, not the title reader. Anyone reading `plan_title_from_file`'s docs (rustdoc/IDE hover) gets a function description that is factually wrong for it.
- `stamp_backlog_in_flight` (line 2127, `pub(crate)`) now has **no doc comment at all** — it had one before this change. The repo convention documents `pub(crate)` functions (see `should_stamp_in_flight` directly above, and every other `pub(crate)` item in this module).

No functional impact — documentation quality only — but it is a regression introduced by this change, and the project constitution's doc-comment discipline makes it worth fixing.

**Fix:** move `plan_title_from_file` (with only its own five doc lines, 2113–2117) **below** `stamp_backlog_in_flight`, so the stamp's doc (2098–2112) re-attaches to the stamp and the title reader keeps only its own doc. The two source-contract tests (`in_flight_stamp_only_applies_to_pending_items`, `stamp_backlog_in_flight_records_the_plan_title`) use `fn_body(...)` which extracts the body — they are unaffected by moving the helper.

### LOW 2 — README doc sync: the Backlog tab's new user-visible chip affordance is undocumented

`README.md` line 77 documents the Backlog tab at feature-list granularity (markdown rendering surfaces, image sidecars, soft delete, steer/interrupt semantics, the status lifecycle including *"an item only shows as in-flight once the agent actually starts executing — pre-planning it stays pending"*). This change adds a comparable-granularity user-visible affordance to that tab — in-flight items now identify the working plan by **title + short id**, the chip **clicks through to the working agent's chat tab**, and the pre-work checkpoint sha is a **copyable detail** — and the README doesn't mention any of it. Nothing is *stale* (the old raw-sha rendering was never documented — zero `checkpoint` matches in README), but per this project's review expectations a feature that ships without its feature-list entry is an incomplete change.

**Fix:** one phrase appended to the Backlog bullet, e.g. *"in-flight items show the working plan's title + short id as a chip that switches to the working agent's chat tab (inert once done/failed); the pre-work git checkpoint sha is a copyable detail row — the manual resume/rollback anchor."*

### Nits (not counted — optional polish, fix or waive at your discretion)

1. **Inert-chip tooltip wording** (`BacklogView.tsx` line 795): *"The plan that worked this item (no agent currently working it)"* — for a **failed** item the plan was abandoned, so "worked" reads oddly there. Defensible reading ("worked on"); a phrasing like "The plan dispatched for this item" would be unambiguous for both done and failed.
2. **`plan_title_from_file` reads the whole plan file** (`run_all.rs` line 2119, `read_to_string`) just to consume line 1 — plan bodies can be large (chunked-write protocol). A `BufReader` + `read_line` would read only the heading. It fires only on main-agent Executing entries (a handful per dispatched item), so this is not a real performance issue — polish only.


## Verification detail (all ten axes)

### 1. Title recording — CORRECT

- **Plans-dir access:** `state.project.root.lock().await.plans_dir.clone()` — the mutex guards a `Project` struct whose `plans_dir: PathBuf` field (`src/project/mod.rs:38`) is the project's `.coding/plans` dir; identical pattern to `memory_maintenance.rs:281`. The guard is dropped at the end of the clone statement (held only for a field clone).
- **Lock ordering:** the stamp holds the backlog store lock while acquiring `project.root`. I checked every `project.root.lock` site in src-tauri (`files.rs` ×5, `mcp.rs` ×2, `memory_maintenance.rs` ×2) — none acquires the backlog store lock while holding the project lock, so no reverse-order path exists; no deadlock risk.
- **File-format contract:** `plan_file.rs` writes `# Plan: {title}` as line 1 (confirmed in the writer and across the entire `.coding/plans/` corpus — every plan file starts with it). `strip_prefix("# Plan: ")` + `trim()` + non-empty filter matches exactly. The chunked-write protocol (backlog 0085ccc0) extends via `update_plan` **after** creation, so line 1 is stable.
- **Write-order (file exists at stamp time):** `create_plan_with_kind` persists the plan file and sets `WorkflowState::Executing` inside the same workflow-lock scope; the `WorkflowStateChanged` event is emitted by `emit_workflow_state` (`src/agent/turn.rs:823`) only **after** the tool call returns — so the file is on disk when the stamp runs. The tool-side comment "the plan is already persisted" confirms the ordering.
- **Refresh-on-every-Executing-entry:** `set_plan_id` runs unconditionally after the (Pending-gated) transition. Sub-plan pushes carry the same ROOT id (`top_plan_id()` = bottom of the stack, pinned by `top_plan_id_is_root_plan_constant_across_sub_plans`) → no-op refresh; a fresh plan replacing an abandoned one mid-dispatch re-links id+title together (pinned by `set_plan_id_records_the_item_plan_linkage`). `set_plan_id`'s `is_live` guard means done/failed items keep their recorded linkage — titles survive plan completion, as designed. `top_plan_id` is always `Some` while Executing (the stack is non-empty), so the None-clearing arm is unreachable in practice.
- **Main-agent gate:** unchanged at the call site (`events.rs:449-460`) — `should_stamp_in_flight` + `mgr.main_agent_id() == Some(agent_id)`; child agents entering Executing cannot stamp the main's item.

### 2. Chip conditional wiring — CORRECT

`planChipTarget(status, mainAgentId)` returns the main agent id only for `in_flight`; the button gets `onClick` only when non-null, with `cursor-pointer` vs `cursor-default` styling and distinct tooltips. The main-agent binding uses the same reactive `selectMainAgentId` selector as the phase badge — correct even while a subagent tab is active. The inert-when-not-in-flight choice over opening the plan panel is documented in the source comment (the panel only shows the active agent's live plan, so it can't display done/failed plans) — a sound, documented decision.

### 3. Note strip is display-only — CORRECT

`displayNote` is computed purely at render from `item.note` + `item.checkpoint_sha`; the stored note, `set_note`, the transition's note passthrough, and `extract_checkpoint_sha` (`run_all.rs:1083-1094`) are all untouched. The copy button copies the **full** sha (`navigator.clipboard.writeText(item.checkpoint_sha)`), so the resume/rollback anchor stays machine-readable AND copyable. Edge case noted and acceptable: a note with leading whitespace before the sha would fail `startsWith` and render raw — but notes are machine-written without leading whitespace, and the fallback (showing the raw note) is lossless.

### 4. Payload / TS type / fixture / contract alignment — CORRECT

`BacklogItem.plan_title` carries `#[serde(default, skip_serializing_if = "Option::is_none")]` — old JSONL lines deserialize (default `None`) and omit on serialize. `BacklogItemView` flattens `BacklogItem` (`backlog_cmds.rs:66-67`), so both `plan_id` and `plan_title` ride the payload; the TS type declares both optional. The fixture's in-flight item pins the PRESENT case (`plan_id: "593f4a4e"`, `plan_title: "Second task plan"`); `items[0]` pins the ABSENT case; the contract test asserts key absence + `typeof === "string"`; and `dto_fixtures_match_serde` constructs the same items Rust-side and compares serde output to the fixture — the real serialization pin, not just a JSON-to-JSON echo.

### 5. SSR-limitation adaptation — SOUND

The test file documents why the clickable markup case can't be rendered (zustand's `getServerSnapshot` reads initial store state, so `setActiveAgent`/`mainAgentId` are invisible to `renderToStaticMarkup`) and compensates without false confidence: the decision logic is pinned by `planChipTarget` unit tests, the wiring by the source-contract test (`setActiveAgent(chipTarget)`), and the INERT markup cases by actual rendered output (`cursor-default`, inert tooltip, no `cursor-pointer`). This is the right adaptation — the clickable case is pinned by two independent means.

### 6. Tests actually pin the contract — YES

- Sha reappearing as visible text → `expect(html).not.toContain(`>${sha}<`)` fails (the tooltip attribute legitimately carries the full sha; the test comment documents this).
- Chip losing click wiring → source-contract `setActiveAgent(chipTarget)` + `planChipTarget` unit tests fail.
- Title not recorded → `stamp_backlog_in_flight_records_the_plan_title` (read-before-set + plans_dir resolution), `in_flight_stamp_only_applies_to_pending_items` (3-arg `set_plan_id` call), `set_plan_id_records_the_item_plan_linkage` (both fields + re-link), `plan_title_from_file_parses_the_heading` / `handles_missing_or_malformed` (missing file, heading-not-line-1, empty title) all fail.
- No chip without a plan, title-null fallback, sha-only note → no note block, copy button carries the full sha — all pinned by rendered-markup assertions.

### 7. Documentation sync — Finding 2 (README)

`PLAN.md` is provider-strategy scoped (not affected); module doc comments are in place on all new public items (`planChipTarget`, `handleCopyCheckpoint`, `set_plan_id`, the `plan_title` field, test modules). The README gap is Finding 2 above.

### 8. Multi-platform neutrality — CLEAN

`navigator.clipboard.writeText` + `window.setTimeout`/`clearTimeout` — exactly the existing prompt-copy pattern in the same component; no Windows-only APIs, paths, or shell syntax anywhere in the change; the Rust side uses `std::path::Path::join` (cross-platform). No `cfg(windows)` additions.

### 9. Security — CLEAN

`plan_title` is local-file-derived display text rendered as a React text node (`{item.plan_title}` inside a span) — React escapes text nodes; no `dangerouslySetInnerHTML` anywhere in the component (verified by search). The tooltip `title={item.checkpoint_sha}` is attribute-escaped by React. No new IPC surface — both fields ride the existing backlog-changed payload.

### 10. Code quality — CLEAN apart from Finding 1

Doc comments present on all new public items; no dead code (`plan_title_from_file` is called from the stamp; `planChipTarget` is used in-component and exported for testability, documented as such); no unused imports (`AgentId` is used in `planChipTarget`'s signature; `AgentId` is exported from `types.ts:7`). The stated suite results (root cargo test 2018+16, `cargo test -p mnemo-app` 226+4, `npx tsc --noEmit` clean, vitest 978) are consistent with everything I read — as a read-only reviewer I could not execute the suites myself, but I found no warning triggers (`#![deny(warnings)]` hazards) in the diff. The only quality defect is the doc-comment misattachment (Finding 1).

## Verdict rationale

Both findings are LOW: Finding 1 is a documentation-quality regression with zero functional impact (but it leaves a `pub(crate)` function undocumented and another function's doc factually wrong — worth the one-line move); Finding 2 is a one-phrase README touch expected by this project's review policy. Neither blocks the change's correctness, security, or platform neutrality, and the test coverage genuinely pins the new contract.

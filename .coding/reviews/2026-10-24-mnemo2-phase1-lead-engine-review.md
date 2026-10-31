## Verdict: FINDINGS (2 high, 5 low)

The Phase 1 backbone is solid and verified: host/runtime/IPC wiring, the Tauri command/event
contract, stale-generation handling, persistence/restore, model-switch failure atomicity,
security posture, the public-API boundary, and docs sync all check out against the headless
engine's real wire shapes. But the pending-interaction path — the exact path the skipped
native-window smoke would have exercised — is broken in two independent ways: an approval
card can never be resolved (H1), and the composer/model-picker stay enabled during a pending
interaction while the runtime rejects both (H2). Both trace to a wire-shape mismatch between
the engine's `PendingInteraction` (serde tag `kind`) and the store reducer (expects
`type`/`request_id`). Neither the Vitest suite nor the runtime tests feed the real wire
shapes, so all gathered evidence stayed green (L1).

Reviewed-state: df0967896a37fac7d1f6fb748d06d0e16e1e7322
## Scope & method

The working tree is not clean (uncommitted changes + untracked files = the Phase 1 delta on
top of the Phase 0b tip df09678), so the uncommitted diff is the review scope. Read in full:

- **Rust host (all new):** `src-tauri/src/lib.rs`, `src-tauri/src/bin/mnemo2.rs`,
  `src-tauri/src/mnemo2/mod.rs` (49 l), `runtime.rs` (964 l), `ipc.rs` (268 l),
  `src-tauri/mnemo2.conf.json`.
- **Frontend (new/changed):** `mnemo2/src/lib/leadEngine.ts`, `types.ts`, `leadState.ts`,
  `store.ts` (754 l), `LeadModelPicker.svelte`, `Composer.svelte`, `LeadChat.svelte`,
  `StartScreen.svelte`, `TopBar.svelte`.
- **Tests (all six):** `leadEngine.test.ts`, `leadState.test.ts`, `store.native.test.ts`,
  `Composer.test.ts`, `LeadModelPicker.test.ts`, `LeadChat.test.ts`, plus the runtime/ipc
  Rust test modules.
- **Docs:** `README.md` (Mnemo2 block), `mnemo2/README.md`, `.coding/design/mnemo2.md`
  Phase 1 section (lines 270–406), `.coding/plans/8509d764.md` (plan of record).

To verify the integration contract against the real engine rather than assuming it, I also
read the consumer-relevant regions of `src/runtime/channels.rs` (`SerializableAgentEvent`
serde, `ApprovalRequest` fields), `src/headless/types.rs` (`SessionCommand`, `PendingInteraction`
serde), `src/headless/engine.rs` (spawn/session registry/shutdown), `src/headless/session.rs`
(send/snapshot semantics), `tests/boundary.rs` (`tauri_uses_only_public_api`), plus
`src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, root `package.json`,
`mnemo2/package.json`, and `.coding/plans/790256c6.md` (bookkeeping delta). Lockfiles and
generated noise skipped per the review contract.

## What was verified correct

- **Tauri command/event contract.** `mod.rs:36-46` registers exactly the commands the adapter
  invokes (`lead_open`, `lead_snapshot`, `lead_prompt`, `lead_interrupt`, `lead_set_model`,
  `lead_approve`, `lead_answer`, `lead_close`, `mnemo2_host_ping` — verified name-by-name
  against `leadEngine.ts`); arg naming (camelCase JS ↔ snake_case Rust) is standard Tauri v2
  and asserted in `leadEngine.test.ts:53-72`. The event envelope
  `{generation, {agent, {kind, …}, rates}}` matches `SerializableAgentEvent`'s serde
  (`tag="kind"`, `rename_all="snake_case"`, channels.rs:683-687); the channel
  `mnemo2://lead-event` matches on both sides and is asserted twice (ipc.rs:266,
  leadEngine.test.ts:115). The `mnemo2_host_ping` handshake correctly degrades to mock both in
  a plain browser and inside Mnemo1's Browser-tab WebView (unknown command → probe false).
- **Capabilities/ACL.** Neither conf sets `app.security.capabilities`, so Tauri's default
  resolution bundles `src-tauri/capabilities/default.json` for the mnemo2 binary too; its
  `windows: ["main"]` matches mnemo2's window label ("main", mnemo2.conf.json:15), so
  `core:event:default` (listen/emit) applies — without that label match the native event flow
  would silently die. The file's plugin permissions (dialog, shell:allow-open) are inert for
  mnemo2 because `run()` registers no plugins — the "minimal surface, not Mnemo1's IPC" promise
  holds. CSP is strict on both prod and dev (`script-src 'self'`, `connect-src ipc:
  http://ipc.localhost`, `object-src 'none'`).
- **Stale-generation handling.** Runtime: per-session generation counter; pump persist-tasks
  re-check `active_generation` before writing (runtime.rs:560-563); `set_model` advances the
  generation and the single forwarder is aborted/re-subscribed on every open. Frontend:
  `applyLeadEvent` drops events whose generation ≠ runtime.generation (unit-tested), and
  `openProject`'s session token guards every async apply against stale resolutions. Sound.
- **Persistence/restore + model-switch atomicity.** `lead.json` under Tauri app-data for
  `com.mnemo.mnemo2`; temp-file + remove + rename write (deliberately Windows-rename-safe);
  loaded on open and fed to `SpawnOptions.resume`; the persisted model is filtered against the
  configured registry on restore. `set_model` restores the old live session on snapshot, spawn,
  and persist failure (all three failure paths checked) and only switches while idle; restore
  is covered by both runtime and store tests.
- **Security.** `workspace_root` validated as an existing directory; state dir host-derived,
  never frontend-controlled; the model registry carries only endpoint/model ids — no API keys
  or config secrets cross the IPC boundary; no git operations, no process spawning, no shell
  anywhere in the host; `ApproveViaApi`/`ApproveEachAction` keeps every gated action in the
  UI. Note (not a finding): `lead_open`'s `config_dir` passthrough lets the trusted webview
  redirect config resolution — harmless under the current self-only CSP; tighten if remote
  content is ever loaded.
- **Multi-platform.** No `cfg(windows)`, no Windows-only APIs/paths/shell syntax in the new
  host or app code; persistence write is deliberately Windows-correct; macOS bundle
  `minimumSystemVersion` set. Only mock seed data is platform-flavored (L5).
- **Public-API boundary.** No banned tokens (`mnemo::agent`/`mnemo::runtime`/`mnemo::host`);
  the code uses crate-root re-exports (`mnemo::{Approval, PermissionPolicy,
  SerializableAgentEvent, UserAnswer}`), `mnemo::headless::*`, and `mnemo::app::policy_factory`;
  the `mnemo::config`/`mnemo::provider`/`mnemo::workspace` module paths match the established
  src-tauri import style (console.rs et al.) and the active `tests/boundary.rs` scan passes.
- **File-tools-first / warning-free.** Nothing in the diff implies shell-based file mutation;
  no `#[allow(...)]` anywhere in the new code; `#![deny(warnings)]` present at the crate root
  and all three new module roots. `mnemo::app::install_panic_hook()` used before the builder.
- **Docs sync.** `README.md:174-189` and `mnemo2/README.md` run/test commands match
  `package.json:14-19` verbatim; the design Phase 1 section (`.coding/design/mnemo2.md:310-406`)
  accurately documents the shipped commands, channel contract, state location, and restore
  semantics; doc comments present on every new public item. The one behavior claim the code
  fails to deliver ("pending approvals and questions render inline with their actions",
  mnemo2/README.md:62-63, README.md:176) is the H1 bug itself, not a doc omission — it becomes
  true once H1 is fixed. No doc changes required by this review.
- **Bookkeeping (accuracy only).** Plan 8509d764's checked steps 1-5 and open step 6 match the
  shipped state; the +7 on `.coding/plans/790256c6.md` is the harness cost-report stamp; the
  design-doc delta describes what shipped.
## High findings

### H1 — Pending approvals/questions are unresolvable in the native host (wire-shape mismatch in the store reducer)

**Where:** `mnemo2/src/lib/leadState.ts:174-190` (event case), `:82-85` (snapshot merge),
`:223-228` (refresh trigger); `mnemo2/src/components/LeadChat.svelte` (pending-notice block);
engine shapes `src/runtime/channels.rs:712-722` (`ApprovalRequest { tool_call_id, tool_name,
args, preview, core_operation }` — no `request_id`) and `src/headless/types.rs:322-346`
(`PendingInteraction`, serde `tag = "kind"` → `{"kind":"approval","request_id":…}` /
`{"kind":"question","question_id":…}`).

Two independent breaks in the same flow:

1. The engine's `approval_request` event carries `tool_call_id`, never `request_id`.
   `applyLeadEvent` stores `pending: { ...inner, type: "approval" }`, so the pending object
   has `tool_call_id` but **no `request_id`** — and LeadChat's Approve/Deny buttons are gated
   on `disabled={!pending.request_id}`. The card that appears when a gated tool call runs is
   permanently disabled, even before the second break hits.

2. `leadEventNeedsSnapshot` includes `approval_request`/`user_question`, so the store
   schedules a snapshot refresh 25 ms after each interaction event (`store.ts:72-80`). The
   snapshot's `pending` serializes with the `kind` tag and has **no `type` field**, so
   `applyLeadSnapshot`'s merge condition — `snapshot.pending && currentPending?.type ===
   snapshot.pending.type` — compares `"approval"` against `undefined` and **never merges**
   the event payload (tool name, question text, options) with the snapshot's ids. The bare
   snapshot shape replaces the event pending; `pending.type` is now `undefined`, LeadChat
   falls through both interaction branches to the generic `WAITING FOR YOU` notice — **no
   buttons at all**.

Net effect in the native host: an approval card shows (with the tool name) with disabled
buttons, then ~25-50 ms later degrades to a buttonless card; a question card shows its options
for ~25 ms and then loses the question text and options (clicking within that window is not a
realistic escape). Because the lead runs under `ApproveViaApi`/`ApproveEachAction`, the first
gated tool call of any turn dead-ends the conversation. This is precisely the acceptance
criterion ("pending-interaction forwarding") that the browser smoke cannot exercise and the
native smoke was skipped for — the review was asked to cover this path in code, and the code
does not deliver it. `mnemo2/README.md:62-63` claims it works.

**Fix direction:** in `applyLeadEvent`'s approval case derive `request_id` from
`inner.tool_call_id`; in `applyLeadSnapshot` merge on `snapshot.pending.kind` (and stamp
`type` from `kind` so even a bare snapshot pending renders the right branch); tighten the
`LeadPendingInteraction` TS type to model the actual serde shape (`kind: "approval" |
"question"`, `request_id`/`question_id`) so the compiler catches field drift instead of the
index signature papering over it. Add reducer + component tests that feed the verbatim engine
payloads (see L1).

### H2 — Enabled composer/model-picker during a pending interaction are always rejected (frontend/runtime busy-state mismatch)

**Where:** `src-tauri/src/mnemo2/runtime.rs:513-584` (`pump_events` — busy is touched only by
`Started`, `Finished`/`Exited`/`Parked`, `Error{retrying:false}`), the `busy` gates in
`send_prompt`/`set_model`; `leadState.ts:186` (`busy: false` on the interaction events);
`Composer.svelte` (`canSend`), `LeadModelPicker.svelte` (`canSwitch = !busy`),
`store.ts:407-430`.

An `ApprovalRequest`/`UserQuestion` is mid-turn: no terminal event fires while the session
waits on the user, so the **runtime**'s busy flag stays `true` and `lead_prompt`/`lead_set_model`
are rejected with "still working on the previous turn". The **frontend**, however, sets
`busy: false` on those events, so the composer and the model picker render enabled during the
pending window — and every send or switch produces an error banner instead of queuing or being
disabled. (The engine itself would queue a mid-turn prompt — its `send()` only rejects
limit-parked sessions — so the runtime's extra busy gate is the rejecting layer.)

Combined with H1 the pending window is a UI dead end: send → error; model switch → error; the
stop control is not even shown (the composer is in send mode because UI busy=false); and the
pending card's buttons are broken (H1). Only closing/reopening the project escapes.

**Fix direction:** pick one contract and test it. The minimal consistent fix is
frontend-side: gate the composer and the picker on `runtime.pending != null` as well (keep the
runtime-side rejection as the backstop). If queued prompts during a pending question are
wanted instead, clear the runtime's busy on `ApprovalRequest`/`UserQuestion` — but then
`set_model` must also be gated on the pending interaction, since switching mid-approval would
orphan the pending responder and the design says model changes are idle-only.
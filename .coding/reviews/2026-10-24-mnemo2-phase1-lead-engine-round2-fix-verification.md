## Verdict: FINDINGS (0 high, 3 low)

Both round-1 HIGH findings (H1 pending wire shapes, H2 mid-turn busy state) are fixed and verified against the engine's real wire shapes end-to-end, with genuine regression tests. Three LOW findings remain open — enumerated explicitly in §4, since round 1's report body omitted its low-finding section; the list below is re-derived from the current tree (round 1's inline references named only L1 and L5; L2–L4 were never recorded and are not recoverable).

### 1. Scope and method

- Base `df0967896a…` is still HEAD — **no intervening commits** (git log: rounds 1 and 2 reviewed the same uncommitted tree, fixes layered in place). Scope is the full uncommitted diff: 13 modified files + 16 untracked, exactly the mechanically derived set from dispatch.
- Read: the full diff stat via `git_read`; then the current content of every fix-surface file in full — `mnemo2/src/lib/types.ts` (140-325), `leadState.ts` (all 246), `LeadChat.svelte` (all 147), `Composer.svelte` (all 127), `LeadModelPicker.svelte` (all), `store.ts` (both halves, 1-753), `leadEngine.ts` (all), and the six test files; the Rust wire chain — `session.rs` (responder keying 156-163/172, snapshot pending 379-405), `types.rs` (`SessionCommand` 149-189, `PendingInteraction` 322-346), `channels.rs` (`ApprovalRequest` 712-722, `UserQuestion` 825-829), `runtime.rs` (snapshot/send/approve/answer/set_model 265-379, pump busy 478-547, scripted tests 830-963), `ipc.rs` (all 268), `mod.rs`/`lib.rs`/`bin/mnemo2.rs`/`mnemo2.conf.json`, `ask_user_args.rs`, `dispatch.rs` 1120-1164; the docs (`mnemo2/README.md` all, `README.md` 165-194); and the bookkeeping files (accuracy only). `mock.ts` seed region checked for the carried L5.

### 2. H1 — FIXED (verified against the real wire shapes)

End-to-end wire chain:

1. Engine event: `SerializableAgentEvent::ApprovalRequest { tool_call_id, tool_name, args, preview, core_operation }` (channels.rs:712-722); `UserQuestion { question_id, question, options }` (channels.rs:825-829, responder stripped).
2. `applyLeadEvent` (leadState.ts:180-208) derives `request_id` from `inner.tool_call_id` (the `inner.request_id` fallback is harmless belt-and-braces), stamps `kind: "approval"`, preserves `tool_name`; the question branch keeps `question_id`/`question`/`options` with `kind: "question"`.
3. The derivation is provably correct on the engine side: `session.rs:156-163` inserts approval responders keyed by **`tool_call_id`**, `session.rs:172` question responders keyed by **`question_id`**; `SessionCommand::Approve { request_id }` is documented as "the id from the `ApprovalRequest` event" (types.rs:177-182). `lead_approve {requestId, decision}` → `ApprovalDecision` and `lead_answer {questionId, answer}` → `AnswerInput` round-trip real wire values in ipc.rs:222-247 (`"deny_all"`, `{"kind":"choice","index":2}`, `{"kind":"freeform","text":"later"}`).
4. Snapshot merge: `PendingInteraction` serializes with serde tag `kind` (types.rs:322-346) — `Approval { request_id }` / `Question { question_id }`, built from the first responder-map key (session.rs:379-399), so snapshot ids equal the event-derived ids. `applyLeadSnapshot` (leadState.ts:80-116) merges on matching `kind` — event details + snapshot ids — and a bare snapshot pending renders gracefully (LeadChat.svelte:37-59: `tool_name ? … : "A gated action is waiting for approval."`, `question ?? "The lead needs an answer."`).
5. The 25 ms post-event snapshot refresh (`leadEventNeedsSnapshot` covers both kinds, store.ts:72-80) re-confirms the ids; `approveLead`/`answerLead` refresh again after resolving (store.ts:379-405), so the card clears as soon as the runtime resolves it.

Regression tests are genuine (pre-fix code fails them: no `kind` tag, no `request_id` from `tool_call_id`, snapshot replace dropped question/options): leadState.test.ts:60-139 feeds **verbatim** `approval_request`/`user_question` event payloads and verbatim snapshot pendings, asserting the merged pending keeps `tool_name`/`question`/`options` and `busy: true`; LeadChat.test.ts:53-82 asserts both cards and their enabled actions; leadEngine.test.ts:31-73 pins the exact invoke arg shapes.

### 3. H2 — FIXED (busy/pending semantics match the real runtime)

- `applyLeadEvent` sets `busy: true` on approval/question events (leadState.ts:204), matching the runtime: busy is cleared only by `Finished`/`Exited`/`Parked`/terminal `Error` (runtime.rs pump, ~526-541) and snapshots mirror `live.busy` (runtime.rs:280) — no terminal event fires while the session waits.
- Composer gates `canSend` on `pending === null` (Composer.svelte:12-16); busy swaps send for stop (line 46). Backstops hold: store `sendLeadMessage` returns while busy (store.ts:342), runtime `send_prompt` rejects while busy (runtime.rs:293-295), `set_model` rejects while busy (runtime.rs:~365-367), and the picker is `disabled={runtime.busy || runtime.pending !== null}` (LeadModelPicker.svelte:28). The pending card stays actionable (Approve/Deny, choice buttons).
- Tests: Composer.test.ts:60-74 (send disabled while pending — pre-fix code left it enabled and fails this), leadState.test.ts busy assertions, LeadChat pending-card tests.

### 4. Remaining LOW findings (explicit enumeration)

- **L-A — carried round-1 L5, still open: platform-flavored mock seed paths feed the real native open.** mock.ts seeds Windows drive paths (`C:/AgenticCoder/AgenticCoder` ~:79, `C:/Games/Garden` ~:91, `C:/Writing/FieldNotes` ~:103); `connectNativeLead` passes `project.path` as `workspaceRoot` to `lead_open` (store.ts), and store.native.test.ts:92 even pins the Windows path as the expected call. On macOS every seeded card hands the real engine a nonexistent `C:/…` root (surfacing as an open error). Suggested fix: neutralize seed paths or validate the directory before `lead_open` — or accept explicitly until Phase 4's project registry replaces the seeds.
- **L-B — carried round-1 L1, TS half fixed, Rust half residual: no scripted approval/question turn in the runtime tests.** The Vitest suite and ipc.rs now feed real wire shapes (L1's substance is fixed), but runtime.rs's ScriptedLlm suite still covers only text turns, restore, model switching, and interrupt (runtime.rs:830-963); `approve()`/`answer()` (runtime.rs:322-349) have no Rust test driving an approval/question turn end-to-end (event → `snapshot.pending` → resolve by `tool_call_id`).
- **L-C — new this round: the model picker's disabled-while-pending gate has no regression test.** LeadModelPicker.test.ts:59-64 asserts the busy half only; nothing asserts the `select` is disabled when `pending !== null` (LeadModelPicker.svelte:28). The dispatch's claim that the new Composer/LeadChat/LeadState tests cover the H2 regression holds for send-gating and busy, not for this gate. One added assertion closes it.

### 5. Contract checks

- **Docs sync — OK.** mnemo2/README.md:59-65 ("pending approvals and questions render inline with their actions" — now true) and :74 (picker disabled while a turn is running — holds during pending since busy stays true); README.md:174-188 matches the shipped commands; every new public Rust item has a doc comment; design doc Phase 1 section (mnemo2.md:309-340) matches ipc.rs/runtime.rs exactly (command list, channel + generation contract, state location under the Tauri app-data dir, registry from `endpoints.toml`).
- **Multi-platform neutrality** — library/app code is neutral (`PathBuf`, Tauri path resolvers, tempdirs in tests); the only platform-flavored content is the mock seed feeding the native open = finding L-A.
- **File-tools-first** — nothing to flag; all delta files are ordinary sources, no shell-mutation artifacts.
- **Warning-free build** — `#![deny(warnings)]` at src-tauri/src/lib.rs:5 and bin/mnemo2.rs:5; reported `cargo test --workspace` exit 0 ⇒ zero warnings; no `#[allow]` additions in the delta.
- **Public-API boundary — holds.** lib.rs exposes only `pub mod mnemo2`; Mnemo1's `main.rs`/config untouched; separate identifier `com.mnemo.mnemo2`, port 5180, own window + CSP (`default-src 'self'`, `object-src 'none'`); `mnemo2_host_ping` disambiguates WebViews.
- **Security — OK.** Pending-card fields render as escaped Svelte text (no `{@html}`); lead output goes through the sanitized Markdown path; no secrets or credentials in the delta.
- **Bookkeeping accuracy (one line each)** — plan 8509d764.md: 5/6 steps, both review rounds stamped at df09678 (correct — no commits between rounds); 790256c6.md's append is Phase 0b's cost report + review stamp (matches the committed state); design doc "Phase 1 (complete)" matches the shipped code; the round-1 report is present as dispatched.

### 6. Notes (not findings)

- A bare snapshot question pending (only reachable if the `user_question` event were missed, e.g. broadcast lag) would render the fallback text with no options — `PendingInteraction::Question` carries only `question_id` on the wire (engine-side shape, committed code), so the frontend cannot do better; unreachable in every realistic flow (the event always precedes the 25 ms refresh-merge, and restored sessions have empty pending maps).
- `ask_user` requires ≥ 2 options (ask_user_args.rs:49-55 — open-ended questions are instructed to post as text), so the choice-only question card is sound; the freeform `AnswerInput` arm is future-proofing, not a gap. `host_tool_call`/`policy_hook` pendings fall to the generic "WAITING FOR YOU" card — no such pending source exists under the Phase 1 config, and interrupt remains available.
- Real-provider native-window smoke remains not run, as stated in the dispatch — consistent with the no-extra-browser-instance rule and no-consent provider calls.
- Process remark: rounds 1 and 2 both reviewed the same uncommitted tree at df09678 (no intervening commits); `package-lock.json` churn is lock noise, out of scope.

Reviewed-state: df0967896a37fac7d1f6fb748d06d0e16e1e7322

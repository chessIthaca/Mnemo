# Job 3c — permissions + the public surface (G2 host tools, G6 three policy tiers)

Master-spec anchors: §5.4 :356-378 (U3/U5 — implemented 2c.5, exposed 3a),
§5.9.2 :634-652 (G2 — ToolSelection, `Engine::register_tool`, host tools),
§5.9.6 :697-716 (G6 — the three policy tiers), §5.9.5 :690 (snapshot
pending), §6 row 3c (:765), §7 rows 31-32 (:879-885), R1 (:937-938, the
cache head), R3 (:941-946, the safety review), R4 (:947-948, the hang note).
Master-plan step 9. Branch `wt/mnemo`.

## A. Job map — what lands where

- NEW `src/headless/host_tools.rs` — `HostToolSpec` (wire) + `HostTool` (the
  registry shim) + the 120 s default.
- NEW `src/headless/external_policy.rs` — `HookKind`, `TimeoutAction`,
  `ExternalPolicySpec`, `PolicyAnswer` (wire).
- `src/headless/types.rs` — `ToolGroup`, `ToolSelection`, `GateRule`,
  `QuestionPolicy`, `TurnEndPolicy`, `Guardrails` (tier-2 wire, §5.9.6),
  `SpawnOptions` gains `host_tools: Vec<HostToolSpec>`,
  `guardrails: Guardrails` (serde default), `external_policy:
  Option<ExternalPolicySpec>`; `SessionCommand` gains `HostToolResult` +
  `PolicyAnswer`; `PendingInteraction` gains `HostToolCall` + `PolicyHook`.
- `src/runtime/channels.rs` — `AgentEvent::HostToolCall` /
  `AgentEvent::PolicyHook` (with oneshot responders), the
  `SerializableAgentEvent` mirrors, `SerializedEvent` gains
  `host_tool_sender` + `policy_hook_sender` (every arm updated).
- `src/agent/dispatch.rs` — the host-tool dispatch branch, the
  `gate_call` consult (the funnel's first veto), the
  `RequireApproval` gate arm.
- `src/agent/policy.rs` — `DeclarativePolicy` + `ExternalPolicy` (the
  `AgentPolicy` impls), `GateVerdict` widened to `Allow | Deny { message } |
  RequireApproval` (+ serde), `PromptHook`/`Continuation` gain serde.
- `src/host/runtime.rs` + `src/agent/factory.rs` — `spawn_agent_shared`
  gains `host_tools: Vec<Arc<dyn Tool>>` + `policy_factory:
  Option<Arc<dyn AgentPolicyFactory>>` (all callers updated); a factory
  build variant consumes them.
- `src/headless/engine.rs` — `Engine::register_tool` (engine-level Host
  group), `Engine::register_policy` (tier-1 registry), spawn tier
  resolution (exclusive tiers), per-spawn wiring.
- `src/headless/session.rs` — pump stashes `pending_host_tools` +
  `pending_hooks`; `SessionCommand::HostToolResult` / `PolicyAnswer` arms;
  snapshot pending reports all four interaction kinds.
- `src-tauri/src/ipc/events.rs` + `src-tauri/src/console.rs` +
  `frontend/src/lib/types.ts` — the new `SerializableAgentEvent` variants'
  arms (forwarding / minimal render; the app never spawns with host tools
  today, but the match arms must exist — event completeness U1).
- Tests: `tests/integration/headless_kernel.rs` (rows 31/32),
  `tests/integration/headless.rs` (contract fixture pins), unit tests beside
  the types they pin.

## B. Current state (probe facts, verified)

1. The seam: `AgentPolicy` (policy.rs:369-412) carries `gate_call`,
   `on_prompt`, `after_tool`, `on_turn_end`, `on_session_end`;
   `AgentPolicyFactory::build_policy(&PolicyFactoryCtx) ->
   PolicyBuildResult { policy, attach_loop: FnOnce(Weak<AgentLoop>) }`
   (policy.rs:224-268); `PolicyFactoryCtx.prompt: Option<PromptSpec>`
   (:193-196). `FreePolicyFactory` (policy.rs:311-340) is the engine's
   factory (NoPolicy::ungated, coding-default fallback).
2. **`gate_call` has ZERO runtime call sites** (graph: incoming_total 0);
   `WorkflowPolicy::gate_call` returns `Allow` always (workflow_policy.rs
   :148-154) — wiring the dispatch consult is inert for the app. Renaming
   `Block { reason }` -> `Deny { message }` touches no callers.
3. The request/response machinery to mirror: `AgentEvent::ApprovalRequest {
   …, responder: oneshot::Sender<Approval> }` (channels.rs:249-256) ->
   `SerializedEvent { event, approval_sender, question_sender }` (:862-869)
   -> `into_serializable` (:878-1165) -> pump stash (session.rs:133-160) ->
   `SessionCommand::Approve`/`Answer` resolve (session.rs:~225-258) ->
   `PendingInteraction` + snapshot (session.rs:291-306).
4. The dispatch funnel (dispatch.rs:420-619): redirect gates (:426-492) ->
   mode read (:494) -> `force_prompt` (:503) -> `gated` (:518-525) ->
   OverridePermissions→`ApprovalBypassed` (:526-547) / safety-rules
   auto-approve (:548-572) / `ApprovalRequest` + `await_approval`
   (:574-619). Tool execution: `dispatch_with_interrupt` (:752). The
   `gate_call` consult plugs BEFORE :494 (a Deny short-circuits everything);
   the host-tool branch plugs inside the execution path (after the approval
   machinery — host tools are gated like any other tool).
5. `spawn_agent_shared` (host/runtime.rs:73-289): 13 params ending
   `permission` (3a.5), `initial_messages` (3a.6), `limits` (3a.7). Callers:
   app IPC (src-tauri/src/ipc/spawn.rs:35/:103/:262), engine.rs:139, the
   IpcSpawner, main-agent startup — all pass `vec![]`/`None` for the two new
   params.
6. `Tool` (tool/mod.rs:290-384): name/category/schema/safety/
   never_auto_for/approval_preview/available/deferred_group/execute/
   execute_streaming — an additive defaulted `host_tool() -> bool` marks the
   shim. `ToolCategory` (:33-42) is UI grouping, NOT the §5.9.2 group set;
   `ToolFilter` (:386+, impl app/workflow/mod.rs:198) is the state filter.
7. Engine (engine.rs): spawn rejects `opts.policy` today (:126-130 — "the
   registry lands with job 4's builder" — THIS job lands it);
   `EngineInner` fields at :387-394; the pump folds stats BEFORE
   `into_serializable` + broadcast (session.rs:124-130 — keep).
8. The pump's `SessionEvent` carries `SerializableAgentEvent`, so every new
   variant must also arm: `into_serializable`, the GUI forwarder
   (ipc/events.rs), console `render_event` (console.rs:242-399), the TS
   union (frontend/src/lib/types.ts:213). R8 applies to Savings/Spend only —
   the new variants FORWARD.

## C. Steps

### D1 — the request/response pipeline for both interaction kinds

channels.rs: `AgentEvent::HostToolCall { call_id, name, args, responder:
oneshot::Sender<HostToolOutcome> }` + `AgentEvent::PolicyHook { hook_id,
kind: HookKind, payload: Value, responder: oneshot::Sender<PolicyAnswer> }`
(`HostToolOutcome = Result<ToolResult, String>` lives beside the event);
`SerializableAgentEvent` mirrors (senders dropped); `SerializedEvent` gains
`host_tool_sender` + `policy_hook_sender`, every existing arm updated.
types.rs: `SessionCommand::HostToolResult { call_id, result: Option<
ToolResult>, error: Option<String> }` + `SessionCommand::PolicyAnswer {
hook_id, answer: PolicyAnswer }`; `PendingInteraction::HostToolCall` /
`::PolicyHook`. session.rs: pump stashes the two new sender maps
(`EngineInner` gains `pending_host_tools` + `pending_hooks`, tokio Mutex
HashMaps like `pending_approvals`); the `send` arms resolve by id
(unknown id -> `unknown_request`, mirroring Approve/Answer); snapshot
pending checks all four maps in order. App-side arms: the GUI forwarder
forwards both variants (they are UI-visible interactions), the console
renders a one-line notice; TS union gains the two members. Unit tests:
into_serializable extraction (the 3b.4 pattern, channels.rs test region),
send-arm resolution, snapshot pending ordering.

### D2 — G2 host tools end-to-end (§5.9.2, row 31)

src/headless/host_tools.rs: `HostToolSpec { name, description, parameters:
Value, safety: SafetyLevel, timeout_ms: Option<u64> }` (serde;
`effective_timeout_ms()` -> 120_000 default) + `HostTool` — implements
`Tool` (name/schema from the spec, safety from the spec, `never_auto_for`
false, `execute` returns an internal error "host tools dispatch through the
kernel's HostToolCall path" — it must never run). `Tool::host_tool() ->
bool` (defaulted false; the shim overrides). dispatch.rs: inside the
execution path, `if tool.host_tool()` -> emit `HostToolCall` (fresh
`call_id` = the parsed call's id) + await the responder under
`tokio::time::timeout(effective_timeout_ms)`; timeout -> `ToolResult::error`
("host tool 'X' timed out after Ys — the call was abandoned") and the turn
CONTINUES; an `Interrupt` during the wait resolves as an error result
(mirroring `await_approval`). Thread `fanin_tx` into the execution path as
needed. engine.rs: `Engine::register_tool(Arc<dyn Tool>)` appends to
engine-level host tools; `SpawnOptions.host_tools` shims built at spawn;
both flow through the new `spawn_agent_shared` param -> a factory build
variant registers them into the registry (advertised like any tool; output
compaction + the read cache apply because the result rides the normal
`ToolResult` path). All `spawn_agent_shared` callers updated (`vec![]`).
Row 31 test (headless_kernel.rs, scripted provider): (a) the model calls
the host tool, the host answers via `HostToolResult`, the model's next
request carries the result (the script asserts the tool-result content);
(b) an unanswered call (spec `timeout_ms` short) yields the timeout error
to the model and the turn still `Finished`.

### D3 — G6 tier 3: the external policy (§5.9.6, row 32)

src/headless/external_policy.rs: `HookKind { ToolGate, PromptIntake,
TurnEnd, AfterTool, SessionEnd }` (serde snake_case), `TimeoutAction {
Allow, Deny, Stop }`, `ExternalPolicySpec { hooks, timeout_ms (default
120_000), on_timeout (default Allow) }`, `PolicyAnswer { Gate(GateVerdict),
Prompt(PromptHook), Continuation(Continuation) }` — with the kernel
`GateVerdict` widened to `Allow | Deny { message } | RequireApproval`
(renamed from `Block { reason }`, zero callers — safe) and serde-derived;
`PromptHook`/`Continuation` gain serde (snake_case, defaults). Per-hook
payloads (documented on the variants): ToolGate `{ "tool", "args" }`,
PromptIntake `{ "text" }`, TurnEnd `{ "success", "stop_reason",
"unattended" }`, AfterTool `{ "tool", "result" }`, SessionEnd `{}`.
policy.rs: `ExternalPolicy { inner: Arc<dyn AgentPolicy>, spec,
loop_ref: Mutex<Weak<AgentLoop>> (2a.6 back-ref pattern), counter:
AtomicU64 }` — hook ids `"hook_{n}"` (monotonic, per-session unique). Every
selected hook emits `PolicyHook` + awaits `PolicyAnswer` under
`timeout_ms`; unselected hooks + head/tail/surface/route/spend/roles
delegate to `inner` (the tier-2 base — external policy is a LAYER, never a
replacement; R1: it may only gate, never touch the tool array). Timeout ->
the `on_timeout` action mapped per kind: ToolGate/AfterTool/PromptIntake ->
Allow = proceed / Deny = the veto; TurnEnd/SessionEnd -> Allow =
`Continue { note: None }` / Stop = park. A mismatched answer form (e.g. a
Gate answer to a TurnEnd) falls back to the same mapped action — a
documented protocol error. dispatch.rs: the `gate_call` consult BEFORE the
mode read (:494) — `Deny` -> error result + return; `Allow` -> proceed.
engine.rs: `SpawnOptions.external_policy` wraps the tier-2 base.
Row 32 test: hooks `[ToolGate, TurnEnd]`, short `timeout_ms`; (a) the host
answers `Gate(Deny { message })` — the call is blocked (the model sees the
deny message); (b) the turn ends, the host answers
`Continuation(Continue { note: Some("...") })` — the kernel runs another
turn (the script asserts the note arrived as the next turn's user message);
(c) `on_timeout: Allow`, nobody answers the ToolGate — the call proceeds.

### D4 — G6 tier 2 + tier 1: guardrails + the policy registry

types.rs: `ToolGroup` (the §5.9.2 list — Read/Write/Shell/Git/Web/Browser/
Codegraph/Memory/Images/Spawn/AskUser/Mcp/Host), `ToolSelection { include:
Vec<ToolGroup>, exclude: Vec<String> }` (default: all groups), `GateRule {
tool, arg_pattern: Option<String> (a regex over the JSON-serialized args;
None matches every call to the tool), verdict: GateVerdict }`,
`QuestionPolicy { WaitForHost, AnswerConstant(String), Deny }`,
`TurnEndPolicy { Stop }`, `Guardrails { prompt: Option<PromptSpec>, tools,
route: Vec<String>, limits: Option<SessionLimits>, gate_rules,
question_policy, turn_end }` — Default = today's free sessions
(coding-default head, all groups, `["free"]`, no rules, WaitForHost, Stop).
policy.rs: `DeclarativePolicy` implements `AgentPolicy` over
`Arc<Guardrails>`: stable_head = the prompt spec's head else the coding
default (FreePolicyFactory's exact behavior); tool_surface = advertise
the ToolSelection (a kernel-side name->group table; host tools map to the
Host group) + gate `ToolFilter::Free` (ungated — the engine form); route
from `guardrails.route`; gate_call = first matching rule's verdict (Deny ->
Deny); on_turn_end -> Stop. dispatch.rs: `RequireApproval` -> force the
`gated` computation (like `force_prompt`, reason "gate_rule"); under
OverridePermissions it rides the existing bypass branch (the override
stays supreme — R3). engine.rs: `Engine::register_policy(name,
Arc<dyn AgentPolicyFactory>)` (EngineInner registry, std Mutex);
`spawn` resolves `opts.policy` (unknown -> `spawn_failed`); exclusivity:
policy + external_policy, or policy + non-default guardrails -> spawn
error (`Guardrails::is_default()` helper); guardrails + external_policy
COMPOSE (declarative base, external layer). Effective limits =
`guardrails.limits.or(opts.limits)`. `question_policy` lands as wire
(binding-ready) — headless sessions register no ask_user today
(FreePolicyFactory::register_tools is a no-op), so WaitForHost is the only
behavior; AnswerConstant/Deny enforcement rides the app's ask_user wiring
at job 5 — a documented deviation, pinned by the contract fixture.
Tests: Guardrails defaults + is_default; gate-rule matching (regex +
tool); exclusivity errors; the ToolGroup table; ToolSelection advertise
filtering (a session with `include: [Read]` never advertises file_edit —
assert via the advertised schema array in the script's first request).

### D5 — contract fixture + docs

tests/integration/headless.rs `public_types_serde_roundtrip`: pin
`HostToolSpec`, `ExternalPolicySpec` (snake_case HookKind), `PolicyAnswer`
(Gate/Deny + Continuation/Continue note), `Guardrails` (defaults + a
populated GateRule + QuestionPolicy arms), `SessionCommand::HostToolResult`
+ `PolicyAnswer` wire shapes. Step-file §C landing notes (deviations,
measured facts) appended HERE when each step lands; plan context append.

### D6 — verify + boundary commit + review

Full workspace battery green (exit 0, zero warnings under
`#![deny(warnings)]`). Self-check against the review checklist (docs sync,
multi-platform neutrality — no `cfg(windows)` additions, file-tools-first,
R3 grep: no GUI/IPC path constructs OverridePermissions). Reviewer round to
PASS, findings fixed, commit `feat(3c.N)` on `wt/mnemo`.

## D. Test plan

- Rows 31/32 (headless_kernel.rs, scripted provider — see D2/D3 legs).
- Unit: channels arms (host_tool_call/policy_hook extraction), pump
  resolution + snapshot ordering, Guardrails defaults/gate rules/ToolGroup
  table, ExternalPolicy on_timeout mapping (a mock loop back-ref or the
  agent-level pattern from savings_and_spend_emitted_where_persisted),
  exclusivity errors.
- Contract fixture (D5).
- Existing goldens stay green: prompt byte-goldens, the R1 cache head, the
  3b stats/events consistency invariant (the pump folds BEFORE broadcast —
  the new variants are interaction events the stats fold ignores).

## E. Invariants

- R1: the external policy may NEVER change the tool array mid-session; the
  tool array (cache head) changes only at spawn (ToolSelection) or via
  `load_tools` (existing deferred groups).
- R3: OverridePermissions stays reachable only through the headless API;
  every would-be-gated call emits `ApprovalBypassed`; a `Deny` verdict is a
  VETO (not an approval skip) and still applies under OverridePermissions.
- R4: host tools + policy hooks always await under a timeout — no hang.
- The pump fold-before-broadcast order; live telemetry stays best-effort.
- Event completeness U1: every `AgentEvent` variant reaches `events()` and
  every mirror has its app-side arms.
- `gate_call` consult ordering: BEFORE the approval branch; a Deny never
  prompts; the consult is inert for the app (WorkflowPolicy answers Allow).

## F. Landing notes

### 3c.1 (D1) — the request/response pipeline — landed 2027-01-19

- **Payload placement deviation (from §A):** `HostToolOutcome`,
  `HookKind`, and `PolicyAnswer` live in `src/runtime/channels.rs`
  beside `Approval`/`UserAnswer` — NOT in `src/headless/external_policy.rs`
  — because runtime modules must never import from the headless module
  (row 34's kernel-only feature build would break the other direction);
  `external_policy.rs` (D3) re-exports them for the public surface.
- **The serde tag collision:** `SerializableAgentEvent`'s internal tag is
  `"kind"`, so the `PolicyHook` mirror's field is named `hook_kind` on the
  wire (a `kind` field would collide with the discriminator) — in BOTH the
  kernel event and the wire mirror, and in the TS union member.
- **GateVerdict widened (D3's rename pulled into D1, zero callers made it
  safe):** `Block { reason }` -> `Deny { message }` + `RequireApproval`,
  serde-derived snake_case; `PromptHook`/`Continuation` gained serde —
  all three are carried by `PolicyAnswer`'s arms.
- `SerializedEvent` gained `host_tool_sender` + `policy_hook_sender`;
  the None-arm tails were converted via one `replace_all` (which also
  hit the two freshly-inserted new arms — the duplicate-field compile
  errors were fixed by deleting the double-specified fields; a lesson:
  insert unique arms AFTER a bulk tail rewrite).
- The GUI forwarder destructures with `..` — the app's agents never
  spawn host tools/external policies, so a dropped responder resolves via
  the kernel-side timeout (never a hang); the console renders one-line
  notices; the TS union grew `host_tool_call` + `policy_hook`.
- Send-arm resolution + snapshot-ordering coverage rides D2's row-31
  test (D1 has no emitter yet — only the extraction/wire unit tests are
  possible); the bogus-id error paths land there too.
- Tests: `host_tool_call_into_serializable_extracts_sender`,
  `policy_hook_into_serializable_extracts_sender`,
  `policy_hook_wire_vocabulary` (channels.rs, 24/24). Battery: workspace
  green, exit 0 (3010 + 51 + integration).
### 3c.2 (D2) — G2 host tools end-to-end — landed 2027-01-19

- **The registration path (deviation from §A's build-threading sketch):**
  host tools ride the registry's shared dynamic slot (the MCP
  late-registration path — `ToolSlot` is interior-mutable and the registry
  sits behind an `Arc<ToolRegistry>` on the loop, so no `&mut` exists
  post-build). `spawn_agent_shared` gained ONE param (`host_tools:
  Vec<Arc<dyn Tool>>`), zero build-variant churn; a slot-pushed tool
  appears immediately (a shim has no deferred group — no reveal gating).
  D4 NOTE: slot-resident host tools are outside `unregister`'s per-name
  carve-out (it touches only the main map), so ToolSelection's exclude
  for host tools filters ENGINE-side at push time (D4's land).
- **The marker (deviation from the step file's `host_tool() -> bool`):**
  `Tool::host_tool_timeout_ms() -> Option<u64>` — ONE defaulted trait
  method answers both dispatch questions (is it a host tool + how long
  to wait); the dispatch branch is a single registry lookup.
- `dispatch_with_interrupt` gained `fanin_tx` + the branch at its HEAD —
  deliberately BEFORE the steering `observe_call`: an external execution
  is outside the steering-lane contract. `dispatch_host_tool` mirrors the
  interrupt semantics: ONE pinned sleep (a buffered command must never
  restart the timeout), Interrupt/Cancel → error result + stop signal +
  buffered passthrough, a closed command channel settles on the
  answer/timeout only.
- **Two G2 mechanisms, documented apart:** `Engine::register_tool(Arc<dyn
  Tool>)` = Rust in-process custom tools (Host group, `execute` runs
  normally — NOT a round-trip); `SpawnOptions.host_tools` specs = the
  round-trip shims. Duplicate names within the spawn's combined list
  refuse the spawn (engine-side check); the slot push also checks
  `contains_name`; a name colliding with a kernel built-in is the
  caller's contract (the registry's main map wins the `get` lookup, so a
  shadowing shim is dead weight + a duplicate schema).
- `SafetyLevel` gained serde (`auto_run`/`needs_approval`) — the wire form
  of `HostToolSpec.safety`.
- Row-31 test (`host_tool_round_trip_and_timeout_row_31`):
  `tokio::join!(run_to_completion, answer)` — concurrent immutable session
  borrows (AgentSession is NOT Clone; both entry points are `&self`);
  leg 1 asserts the mid-flight `snapshot().pending` reports
  `PendingInteraction::HostToolCall` (the §5.9.5 coverage D1 deferred
  here), leg 2 the 150 ms timeout error reaching the model with the turn
  still finishing. Battery: 3012 + 52 + integration, exit 0.### 3c.3 (D3) — G6 tier 3 the external policy (row 32) — landed 01f6fe7

- The landed `ExternalPolicy` (policy.rs): per-kind on_timeout mapping — gates resolve Allow/Deny; a turn end ALWAYS parks, even under `on_timeout: Allow` (an auto-continue is an explicit act, never a timeout default — a deviation from §C D3's "Allow = Continue { note: None }" wording, documented on the landed enum); a WRONG-form answer maps exactly like a timeout. Monotonic `hook_{n}` ids; unselected kinds + head/tail/surface/spend/roles delegate to the base (R1: gate-only).
- **The runtime note-honoring arm (implied by the row-32 leg-b contract, not listed in §C):** agent.rs's auto-continue now pushes the HOST's note — `Continue { note: Some(host_note) }` becomes the next turn's user message verbatim; `None` keeps the harness note byte-identical (the workflow path). `WorkflowPolicy::on_turn_end` returns `Continue { note: None }` (verified) — no app-path change.
- **`run_to_completion` re-scoped to the park (session.rs):** resolves on `Parked`/`Exited`, not the first per-turn `Finished` — a tier-3 Continue spans model turns, and the first-Finished return handed back turn one's text while turn two streamed. `final_text` resets per `Started`; the LAST Finished reason wins. Single-turn sessions are byte-identical (they park immediately).
- Tests: `external_policy_on_timeout_mapping` (src/agent/tests.rs, the savings_and_spend pattern: answered Deny/Allow, wrong-form, unanswered timeouts, the layer-B Deny message, unselected delegation — the shared `gate_args` hoisted so the `join!` futures borrow a binding that outlives them) + `external_policy_gate_and_turn_end_row_32` (three legs: the gate vetoes BEFORE the host-tool branch + the mid-flight PolicyHook snapshot; the TurnEnd Continue-with-note; the unanswered gate proceeds under `on_timeout: Allow` into R4's per-tool timeout). Battery: workspace green, exit 0.

### 3c.4 (D4) — G6 tier 2 + tier 1 guardrails + the policy registry — landed b8d6ed5

- **Placement (the D1 deviation pattern):** `ToolGroup`/`ToolSelection` are REUSED from src/agent/factory.rs (:118-230 — already the §5.9.2 shapes with ALL/free_default/includes/selects); headless re-exports them plus GateRule/QuestionPolicy/TurnEndPolicy/Guardrails from src/agent/policy.rs where `DeclarativePolicy` consumes them; types.rs carries only the `SpawnOptions.guardrails` field.
- **The tool narrowing is REGISTRATION-side (a deviation from §C D4's advertise-filter wording):** `ToolFilter` is state-derived (tool/mod.rs:403+, derives PartialEq/Eq) and cannot carry a group predicate; the engine builds a PER-SPAWN FACTORY CLONE carrying the selection so the factory's own `want(group, name)` table narrows the registry (R1 at spawn; the mcp reveals honor it — factory.rs:1404). STRICTER than an advertise filter (unselected tools cannot even dispatch) and the §C test holds verbatim (`include:[Read]` never advertises file_edit — the leg passed via absence from the registry). The engine's host tools retain by `selects(Host, name)` at push time.
- **`AgentLoopFactory` is Clone:** its six plain RwLock fields (provider, default_display_effort, context_manager, spawner, descendant_tracker, policy_factory) are Arc-wrapped — the in-file pattern; every `.read()/.write()` site unchanged through auto-deref, and the clone shares the live seams.
- **One mechanism, both tiers:** tier 1 = clone + `set_policy_factory(named)` (the 2b build machinery runs the named `build_policy`/`attach_loop`); tier 2 = clone + `with_tool_selection` + the post-build `DeclarativePolicy` swap (the D3-established pattern — nothing reads the policy between build and first prompt). guardrails + external COMPOSE — the declarative swap lands FIRST (the external layer reads `loop.policy()`). Exclusivity: policy + non-default guardrails, and policy + external_policy, refuse the spawn (`Guardrails::is_default()`); effective limits = `guardrails.limits.or(opts.limits)`.
- `GateRule.arg_pattern` compiles once at `DeclarativePolicy::new` — a bad regex fails the spawn, never a silent match-nothing. `QuestionPolicy` lands wire-only (headless sessions register no ask_user; the enforcement rides job 5 — §C's own documented deviation). Dispatch's `ApprovalBypassed` audit reason gains `"gate_rule"` (the RequireApproval→gated folding itself landed with D3's consult).
- Tests: `guardrails_is_default_and_gate_rules` (tests.rs: the is_default flips, the gate-rule matching incl. the JSON-args regex, the route, the turn-end verb, the free-agent surface, the bad-pattern error) + `guardrails_selection_gate_rules_and_policy_registry` (headless_kernel.rs: the include:[Read] narrowing, the read_files gate-rule veto, the registry resolution + the unknown-name error, both exclusivity errors, the default no-op — NB the tool is `read_files` in this codebase, not `file_read`). Battery: workspace green, exit 0.

### 3c.5 (D5) — contract fixture + docs — landed with this commit

- `public_types_serde_roundtrip` pins (§C D5): `HostToolSpec` (a populated sample rides `SpawnOptions.host_tools`), `ExternalPolicySpec` (snake_case HookKind, D3), `PolicyAnswer`'s Gate/Deny + Continuation/Continue-note + Stop arms, `Guardrails::default()` + the populated GateRule + both `QuestionPolicy` arms, and the `SessionCommand::HostToolResult` + `SessionCommand::PolicyAnswer` wire shapes. All green.
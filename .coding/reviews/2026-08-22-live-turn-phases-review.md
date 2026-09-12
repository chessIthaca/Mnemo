# Review — Live turn phases, timer, token counter + per-trace phase timings (plan f3739d61)

Reviewed the full uncommitted diff on `feat/...` (24 modified files + 2 new:
`.coding/plans/f3739d61-...md`, `frontend/src/lib/ipc-fixtures/event-phase.json`).
Read turn.rs end-to-end (all phase-emission and tool-loop exit paths), the
provider stream tasks, trace.rs, main.rs / model_resolver wiring, and the
frontend reducer / InflightBar / LlmTraceView / Message.tsx paths.

Overall: the feature is well-built. Phase emission ordering is correct on every
path I traced, the contract (serde snake_case + fixture + TS union) matches,
regression tests genuinely pin the behavior, and the new public items are
documented. Two findings need fixing (one MEDIUM correctness, one LOW data
gap), plus two LOW documentation/hardening items.

---

## MEDIUM

### M1 — `tools_ms` attribution writes to the globally-newest record and mis-attributes under concurrency

`LlmRequestLog::set_last_tools_ms` (src/provider/trace.rs:437–442) mutates
`records.back_mut()` — the newest record in the ring at call time. The
guarantee stated in the doc comments ("that response's stream just ended, so
its record is the most recent", trace.rs:432–436, openai.rs:533–535,
anthropic.rs:684–686, turn.rs:1556–1559) holds **only in a single-agent world
where nothing else creates a record during the tool batch**. In practice the
ring is shared by every agent and every LLM call in the app (one
`LlmRequestLog` instance — main.rs:832, IpcState.trace — wired into the default
provider AND every resolver-built provider: main.rs:1098–1101,
model_resolver.rs:249–253, client_factory.rs:185–199). Concrete interleavings
that create a NEWER record between the tool-calling response's stream end and
`record_tools_phase_ms` at batch end (turn.rs:1559):

1. **Concurrent agents (the app's normal mode):** a previously spawned
   reviewer/subagent or a UI-spawned parallel agent streams its own request
   while the main agent executes its tool batch. The main agent's
   `record_tools_phase_ms` then lands on the *other* agent's record, and the
   tool-calling request's own record keeps `tools_ms = None`.
2. **Same agent, deterministic:** `memory_consolidate` runs the full LLM
   extraction pipeline during `execute_tool_call` using the swappable provider
   slot (factory.rs:816–820, 391–399; tool/memory/mod.rs:272–305) — the same
   provider instance holding the shared trace. Its extraction request's record
   is created *during* the batch, so it becomes `back()` and silently receives
   the tools duration.

The record id IS known at stream time (`trace_ctx = (log, id)` in both
providers, openai.rs:700–704 / anthropic.rs:787–791) but is discarded before
the tool loop. Suggested fix: have each client remember its most recent record
id (e.g. an `Arc<Mutex<Option<u64>>>` set where `rec_id` is created), change
the trait method to consult that id, and replace `set_last_tools_ms` with an
id-scoped `set_tools_ms(id, ms)` that routes through `with_record` (which also
fixes L1). The existing test `set_last_tools_ms_attributes_to_the_newest_record`
(trace.rs:1375+) currently *pins the buggy semantics* — update it to assert
that an unrelated record created after the target request does NOT receive the
tools phase. (Verified separately: `describe_image`/vision builds a
`VisionClient` with no trace (client_factory.rs:240–243), so it cannot pollute
the ring; and the per-iteration summarization request happens before the main
request, so it does not interfere.)

---

## LOW

### L1 — `set_last_tools_ms` bypasses the file-writer mirror: traces.jsonl rows never get `tools_ms`

`set_last_tools_ms` locks `shared.records` directly (trace.rs:437–442) instead
of going through `with_record` (trace.rs:518–540), which is what sends
`Msg::Dirty(id)` to the background writer. With "Log to file" enabled, the last
mirror of a record happens at stream end (finish/usage); the later tools_ms
mutation never notifies the writer, so the on-disk row permanently disagrees
with the Trace tab (`tools_ms: null` on disk vs. a value in memory). No test
covers file mirroring of the tools phase. Fix together with M1 by routing the
id-scoped mutation through `with_record`, and extend
`file_logging_mirrors_latest_state_to_one_jsonl_row` to assert the final row
carries `tools_ms`.

### L2 — `send_ms` doc comments overstate the measurement window

The new docs (trace.rs:144–145, 198–199, 414–415; types.ts:347–349, 379–381;
openai.rs:585–587, 799–800; anthropic.rs:729–730, 884–885) describe send_ms as
"record created → POST issued — the request-build + serialization phase,
before the network wait begins". Reality: `request_start` is captured **after**
the POST response arrives (openai.rs:699, anthropic.rs:786), so send_ms
actually spans record creation → response headers received — it includes the
full network round trip and server prefill. On the OpenAI `reasoning_effort`
400-retry path (openai.rs:649–679) it additionally includes the failed first
attempt's round trip, which is nowhere documented. Also `build_request_json`
runs *before* `record_created` (openai.rs:561 vs 588), so "request-build" is
only partially covered. The measurement is symmetric across both providers and
remains a useful "time to open the stream" figure — but either reword the docs
to "record created → stream open (response headers received; includes the
reasoning_effort fallback retry when one fires)" or restructure. (The ttft_ms
anchor has the same POST-inclusive quirk, but that is pre-existing and out of
scope.)

### L3 — `reduceChildFinished` leaves the new turn fields stale (latent)

`reduceChildFinished` (agentEventReducer.ts:946–962) sets `running: false` but
does not reset `phase`/`turnStartedAt`/`liveTokens`, unlike `reduceFinished`
(862–881) and the final-error arm of `reduceError` (915–920). Today this is
harmless: the forwarder synthesizes `child_finished` only after the child's own
terminal Finished/final-Error (which resets via those reducers), and event
order is preserved — so the stale values are never visible (the bar renders a
static "idle" whenever `running` is false). Add the same three resets for
symmetry/hardening so a future reordering can't leave a subagent with a stale
phase.

---

## Verified clean (no findings)

- **Phase emission ordering (turn.rs):** `Sending` at the loop top fires
  exactly once per iteration including the has_bad_json `continue` (1132) and
  the tool-result loop-back; `Waiting` after `stream_result?` and before the
  select loop (737–747); `Streaming` fires once per request via
  `phase_streaming_sent`, including when the first event is ToolCallStart /
  ToolCallArgumentDelta (781–852); `RunningTools` only on the non-empty
  tool-calls path (1190–1198). All three tool-loop exits (safe-point break
  1250, hard_stop break 1552, normal completion) fall through to
  `record_tools_phase_ms` at 1559, which runs before BOTH the stop_reason
  early return (1566) and the MAX_RETRIES abort (1589). `tools_start` is
  re-anchored per batch (1190). Early-return paths (summarize-stop 349,
  provider-request stop 708, mid-stream stop 995) emit Finished/Err without
  phase events and the frontend resets on both terminal kinds.
- **Frontend resets:** `reduceFinished` always resets phase/turnStartedAt/
  liveTokens (862–881); `reduceError` resets them only when `retrying=false`
  (915–920); `reduceStarted` preserves `turnStartedAt` when `agent.running`
  (turn.rs Started is re-emitted per whole-turn retry attempt) and the
  regression test pins it (useAgentStore.test.ts "mid-turn provider-retry
  re-Start keeps the original timer"). liveTokens only increments via
  `Math.max(1, round(len/4))` and resets to 0 on usage/started/finished/error
  — it can never go negative.
- **InflightBar:** setInterval is created only while `running` and cleared on
  unmount and on `running` flipping false (cleanup closure); the label switch
  covers all five `TurnPhase` values exhaustively; the amber override keys off
  `pendingApproval !== null` / `pendingQuestion !== null` (fields that exist
  and are cleared on Started). Message.tsx:528–536 is a per-tool-card
  "running" indicator (thinking-dots + "running" next to an in-flight tool
  card) — a separate streaming affordance that correctly stays.
- **Contract:** `PhaseKind` serde is snake_case → `{"kind":"phase","phase":
  "running_tools"}`; the Rust serde round-trip test (channels.rs:1051+) and
  `tests/contract_fixtures.rs` match the committed fixture
  `event-phase.json` (content verified); the TS `SerializableAgentEvent`
  union + `TurnPhaseKind` mirror the wire form; `send_ms`/`tools_ms` are on
  both `LlmRequestDetail` and `LlmRequestSummary` (the wire type for
  `list_llm_requests`); the jsonl rows serialize the full record so the new
  fields flow automatically once L1 is fixed.
- **Constitution:** doc comments on all new public items (PhaseKind variants,
  AgentEvent::Phase, the trait default method, both trace methods/fields, TS
  types, fmtDuration); no new `#[allow(...)]`; no new `cfg(windows)` code and
  no Windows-only assumptions (platform-conditional tests use the existing
  `cfg!(target_os)` pattern only); README bullets (README.md:47–48) accurately
  describe the shipped behavior; PLAN.md contains no trace-field docs that
  would go stale.
- **Regression tests pin the behavior:** the backend phase-sequence test
  (agent/tests.rs:648) asserts the exact 8-element sequence and would fail if
  Sending/Waiting/Streaming/RunningTools ordering or the no-tool-calls skip
  regressed; the frontend tests pin label strings, timer wiring, token
  accumulation/reset, and turnStartedAt preservation.

## Notes (not findings)

- The untracked fixture `frontend/src/lib/ipc-fixtures/event-phase.json` must
  be included in the commit (both the Rust contract test and the vitest
  ipc-contract test fail without it).
- Cosmetic: src/agent/tests.rs:434 now reads `workflow,        sandbox,` — a
  whitespace-only join introduced in the diff; reformat.
- I could not run `cargo test` / vitest (read-only reviewer). The diff shows
  no obvious warning sources, but the closing agent should confirm a green,
  warning-free build as usual.

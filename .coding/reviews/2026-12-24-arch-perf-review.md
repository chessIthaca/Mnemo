## Verdict: FINDINGS (0 high, 3 low)

Architecture + performance review of Mnemo at HEAD bf0f71c plus the uncommitted `#[allow(dead_code)]` delta, 2026-12-24. All prior findings (2026-12-15 M1-M3/L1-L6, 2026-12-17 repetition guard / consolidation dedup / SSRF) verified as fixed and not re-reported. Every claim below was verified against the current tree.

---

### F1 — LOW (architecture/process): `#[allow(dead_code)]` on `extract_checkpoint_sha` violates the project's own warning-free rule

**Evidence:** `src-tauri/src/ipc/run_all.rs:848-863` (the uncommitted delta). The comment states: "Kept deliberately callerless (see above), so the crate-wide `deny(warnings)` in main.rs needs the exemption spelled out - the tests below are the only users and they do not count for dead-code analysis." The doc comment (lines 841-847) confirms no automatic caller since backlog 45dcf577 removed the rollback arm; only `extract_tests` (lines 865-905) calls it.

**Why it matters:** The project constitution is explicit: "Never add `#[allow(...)]` to silence a warning — fix the root cause (remove the dead code, drop the unused import, ...)". The root cause here is that the function has no production caller — the sanctioned fix is removal, not an exemption. This is the first `#[allow]` in the tree (the rule's zero-tolerance stance is what makes `#![deny(warnings)]` meaningful), and it hands every future contributor a precedent to justify their own exemptions. The sha itself stays human-readable in the item note either way; the extraction helper serves no automated path.

**Fix sketch:** Either (a) delete `extract_checkpoint_sha` and its `extract_tests` module — the checkpoint sha remains visible in the note string and recoverable from git history if a caller is ever restored; or (b) restore a real caller, e.g. expose the sha in the backlog UI payload (`backlog_list` already returns notes; a parsed `checkpoint_sha` field would make the manual resume/rollback anchor machine-readable). Either removes the need for the attribute.

---

### F2 — LOW (architecture): GLM-5.3 stream-guard boundary strings hardcoded by model-name prefix inside the generic OpenAI-compatible client

**Evidence:** `src/provider/openai.rs:947-967` — inside `complete()`, a `model.to_ascii_lowercase().starts_with("glm-5.3")` check appends GLM boundary strings to `stream_stop_boundaries` for the stream guard. In parallel, the request-side stop mechanism IS config-driven: `stop_token_ids_for` (`src/config/endpoints.rs:430-436`) resolves model-level → endpoint-level `stop_token_ids`, shipped in the `endpoints.toml` examples (endpoints.rs:1330, 1341).

**Why it matters:** Model-specific wire knowledge now lives in two places under two mechanisms: token IDs in config (data-driven, user-editable without recompile) and boundary strings in provider code (requires a code change). The next model with leaky stop tokens — GLM-5.4, a fine-tune, or a proxy alias of glm-5.3 — needs another edit to `openai.rs`, and a user running an aliased/renamed model gets no guard protection at all. It also couples the generic OpenAI-compatible client to one vendor's model naming, an accreted exception to the otherwise clean config-drives-wire-params layering (contrast the DeepSeek `off`→`none` mapping, which is correctly placed in config via `reasoning_effort_off_wire_value`, endpoints.rs:348-359).

**Fix sketch:** Add an endpoint/model-level config field `stop_boundary_strings: Vec<String>` beside `stop_token_ids`, resolve it the same way (`stop_boundary_strings_for(model_id)`), and seed the shipped GLM entries in `endpoints.toml`. The provider then reads config instead of matching model-name prefixes; new models and aliases become a config edit.

---

### F3 — LOW (performance): No transcript windowing — DOM size and per-update element-creation cost grow linearly over long sessions

**Evidence:** `frontend/src/components/chat/Conversation.tsx:98-100` — `state.transcript.map((entry, i) => <Message key={i} ... />)` mounts every entry with no virtualization or render cap. `Message.tsx:30` has a custom `arePropsEqual` for `React.memo` (finalized entries skip re-render) and streaming text renders as plain text — both good — but every batched store update during token streaming still re-creates N React elements and runs reconciliation over all of them, and the DOM retains every entry for the life of the session.

**Why it matters:** Run-All — the overnight unattended mode this release strengthens — makes multi-hundred-entry transcripts routine: each item's plan/execute/review cycle emits dozens of tool cards, diffs, and assistant turns. Element creation + reconciliation per DeltaBatcher flush and unbounded DOM growth degrade streaming smoothness and scrolling on modest hardware precisely in the scenario the feature targets. (The index keys are safe only because the transcript is append-only — worth a comment at the map site.)

**Fix sketch:** Window the list — `react-window`/`react-virtuoso`, or a lightweight "render last K entries + 'show earlier' expander" (K ≈ 200) that collapses older entries into a summary row. The existing memoized `Message` makes either adoption cheap since collapsed/unmounted entries cost nothing.

---

### Done well

- **DeltaBatcher** (`src-tauri/src/ipc/events.rs:112-262`): the streaming hot path is excellent — per-(agent, kind, index) buckets, 64KB byte-cap plus a fixed flush deadline armed on the first delta, structural events flush pending deltas first to preserve ordering, and the forwarder owns the fan-in receiver with no manager lock held across `recv()`. Token-stream → Tauri-event amplification is properly bounded.
- **Backlog status ⇄ plan-lifecycle semantics** (`src/backlog.rs` transition table + `run_all.rs:1240-1330`): the "Done means the plan loop verifiably closed; abandonment is the one true failure; everything else keeps its status and halts the run" contract is centralized in one guarded match, applied identically to Run-All and single-dispatch paths, and pinned by exhaustive tests — including source-introspection tests that assert the arms' structure. This is the strongest piece of the fresh surface.
- **Search auto-delegation** (`src/tool/agent/search.rs`): clean fast path — symbol-shaped and memory-hunt patterns delegate to codegraph/memory with a per-instance escape hatch (the second identical query runs the real search), SQLite work on `spawn_blocking`, recall stays async, and literal queries served from the FTS index. The delegation note teaches the model the re-issue protocol inline.
- **Memory recall** (`src/memory/mod.rs:1180+`): FTS5 pre-filter with a candidate pool before semantic scoring, capped full-scan fallback, batched access-bump updates in a single query, ONNX embedding on `spawn_blocking` with a hash-embedder fallback.
- **Provider layering on the other fresh changes**: the DeepSeek `off`→`none` mapping lives in config keyed by provider kind (not in the provider), and the claude-opus-5 empty-thinking-block filter (`anthropic.rs:871-886`) is applied at both capture and echo with the Rule-4 distinction documented — the fix fits the established pattern rather than accreting an exception.
- **Backlog soft delete + 30-day purge** (`src/backlog.rs:232-260`): bounded retention, purge cost paid once at startup only, soft-deleted items invisible to all live-item filters, and the JSONL union-merge design (reload-before-mutate) keeps multi-worktree merges clean.
- **Frontend streaming hygiene**: memoized `Message` with a purpose-built equality check, plain-text streaming with markdown applied only on finalize, throttled (100ms) scroll-to-bottom with stick-to-bottom detection, and per-card local `useState` for the new expanded views (no transcript-wide re-render on expand).

# Review: whole-app review findings (steps 6–11) + memory-recall decay fix

Reviewed the FULL uncommitted diff (`git diff HEAD`, 35 files) plus all new
untracked files (`error.rs`, `keys.rs`, `models.rs`, `rewire.rs`, `Markdown.tsx`,
`MarkdownImpl.tsx`). Could not run `cargo test` (read-only, no shell), but
statically traced every claim.

## Correctness

### C1 (MUST-FIX) — Stale test will fail `cargo test` after removing Debug-form kind leniency
**`src-tauri/src/ipc/settings.rs:1196-1203`** — test `accepts_debug_form_kind_case_insensitively`:

```rust
fn accepts_debug_form_kind_case_insensitively() {
    // The legacy get_config sends format!("{:?}", kind) = "OpenAI"/"Local".
    // The save path must accept that back without rejecting it.
    let ep = dto("o", "OpenAI", "https://x/v1/").into_endpoint().unwrap();
    assert_eq!(ep.kind, EndpointKind::OpenAI);
    let ep = dto("l", "LOCAL", "http://localhost/v1/").into_endpoint().unwrap();
    assert_eq!(ep.kind, EndpointKind::Local);
}
```

The diff (Q-M3) removed `self.kind.to_ascii_lowercase()` from `into_endpoint`
(`settings.rs:117`), so the match is now an **exact, case-sensitive** string
match on `"openai"`/`"local"`. `"OpenAI"` no longer matches → falls to the
`other =>` arm → returns `Err("endpoint 'o': unknown kind 'OpenAI' …")` →
`.unwrap()` **panics**. This test is now stale and will fail `cargo test`,
violating the constitution's "build must pass warning-free" gate.

The removal of the leniency is itself correct (`get_config` always emits the
serde form via `endpoint_kind_wire`, confirmed at `settings.rs:596-632` and
asserted by `endpoint_kind_wire_matches_serde` at `:1292`). The test is the
thing that's wrong. **Fix:** delete `accepts_debug_form_kind_case_insensitively`
(Debug-form acceptance is no longer needed/possible) or rewrite it to assert
that `"OpenAI"` is now *rejected* with the "unknown kind" error.

### C2 (minor) — `From<myharness::error::Error>` leaves a trailing space in `kind` for struct variants
**`src-tauri/src/ipc/error.rs:60-65`**:

```rust
let debug = format!("{:?}", e);
let kind = debug
    .split(['(', '{'])
    .next()
    .unwrap_or("error")
    .to_string();
```

For the new struct variant `WorkflowWrongState { current, expected }`, the
`Debug` output is `WorkflowWrongState { current: "...", expected: "..." }`.
`split(['(', '{']).next()` yields `"WorkflowWrongState "` — **with a trailing
space** before the `{`. So `kind` becomes `"WorkflowWrongState "` (trailing
space). Tuple variants (`Workflow("…")` → `"Workflow"`) and unit variants
(`WorkflowNoPlan` → `"WorkflowNoPlan"`) are correct; only struct variants are
affected.

The frontend does not branch on `kind` yet (it only reads `.message` via
`errMsg`), so this is latent — but the DTO's stated purpose (module doc,
`error.rs:7-9`) is to let the frontend "react differently to distinct failure
kinds," and a trailing space would break any future exact-match comparison
(`kind === "WorkflowWrongState"`). **Fix:** add `.trim()`:
`…next().unwrap_or("error").trim().to_string()`.

## Bugs

### B1 (minor / incomplete migration) — Two `#[tauri::command]` functions still return `Result<_, String>`
**`src-tauri/src/ipc/agent.rs:468` (`get_workflow_state`)** and
**`src-tauri/src/ipc/agent.rs:510` (`get_plan`)** were NOT converted to
`IpcError` — both still return `Result<T, String>`. The task summary states
"All `#[tauri::command]` functions across 8 IPC files changed from
`Result<T, String>` to `Result<T, IpcError>`," but these two registered
commands (confirmed in `main.rs:340-341`) were missed.

Functionally harmless: Tauri accepts `Result<T, String>`, and `errMsg()`
(`tauri.ts:74-90`) handles bare strings first (`if (typeof e === "string")
return e`), so no `[object Object]` rendering. But it's an incomplete
migration inconsistent with the stated goal. **Fix (optional):** convert both
to `Result<T, IpcError>` for uniformity, or leave as-is (no behavior defect).

## Security

**No findings.** The `From<myharness::error::Error>` impl uses `e.to_string()`
for `message` — this is the **same** string the pre-change code produced via
`.map_err(|e| e.to_string())`, so no *new* secret exposure is introduced. The
`kind` field derives from the variant name (Debug), not the message, so it
carries no payload. `list_models`/`list_vision_models` still route library
errors through `.map_err(IpcError::from)` with the existing "no key is leaked"
guarantee (the key is never placed in the error string by `fetch_models`).
The `describe_image`/log-permission fixes (steps 1–5) are correctly absent from
this diff (already committed).

## Constitution compliance

**No `#[allow(...)]` added.** The only `#[allow(...)]` in the tree is
`src/agent/loop_impl.rs:191,230` (`clippy::too_many_arguments`) — pre-existing,
not in this diff. No new suppressions.

**Doc comments present** on all new public items: `IpcError` + fields + `msg`,
`WorkflowNoPlan`/`WorkflowWrongState` variants, `ToolFilter::from_state`,
`rewire_vision_and_embedder`/`sync_model_resolver`, `get_api_keys`/
`list_models`/`list_vision_models`, frontend `errMsg`/`Markdown`/`MarkdownImpl`.

**Accept-only findings confirmed (no change needed):**
- Q-L2: `out.sort_by` present at `src/tool/mod.rs:364` (`out.sort_by(|a, b|
  a.name.cmp(&b.name))`). ✓
- Q-H2 (useAgentStore accepted as-is), L3 (browse native picker — by design),
  R3/R4 (browser locks — acceptable): not flagged, per instructions.

**Memory decay math verified correct** (`src/memory/mod.rs:567-592`):
`strength::decay(m.strength, now - m.last_accessed_at)` returns a new `f64`
(`strength * (-elapsed/tau).exp()`); the stored `m.strength` is never written
back — `decay` is a pure function, and `recall` only reads `m`. The new test
`recall_decays_strength_so_old_memories_sink` (`:1229-1278`) is sound: stale
memory (30-day-old `last_accessed_at`) decays to ≈`exp(-30/7)`≈0.014 while the
fresh one decays to `exp(0)`=1.0, so fresh outranks stale. (The test comment's
"≈0.5^4≈0.06" is a loose half-life approximation, not the actual exp formula —
harmless, the assertion holds either way.)

**`errMsg` completeness verified:** no missed `String(e)`/`String(err)` error
sites remain. All residual `String(...)` calls in the frontend stringify
numbers/dates/JSON/React-nodes, not thrown IPC errors.

## Summary

| Severity | Count | Action |
|---|---|---|
| Correctness (must-fix) | 1 (C1) | Delete/rewrite stale test — blocks `cargo test` |
| Correctness (minor) | 1 (C2) | Add `.trim()` to kind extraction |
| Bugs (minor) | 1 (B1) | Optional: convert 2 remaining commands |
| Security | 0 | clean |
| Constitution | 0 | clean (no `#[allow]`, docs present) |

**C1 must be fixed before commit** — it fails the warning-free build gate.

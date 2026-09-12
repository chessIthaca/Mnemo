# Code Review — Tauri IPC bridge, tests, and config/build

Scope: uncommitted changes to the Tauri bridge (`src-tauri/`), `tests/`, and
config/build files. Reviewed for correctness, security, config over-breadth,
batch-script correctness, and constitution (agent.md) safety.

## Verdict per file

| File | Verdict |
|------|---------|
| src-tauri/src/ipc/commands.rs | minor issues |
| src-tauri/src/main.rs | OK |
| src-tauri/src/ipc/state.rs (context, unchanged) | OK |
| src-tauri/src/ipc/events.rs (context, unchanged) | OK |
| src-tauri/Cargo.toml | OK |
| src-tauri/tauri.conf.json | OK |
| src-tauri/capabilities/default.json (unchanged, context) | OK (see note #11) |
| tests/ipc_bridge.rs | OK |
| tests/provider_integration.rs | OK |
| Cargo.toml | OK |
| .coding/safety.toml | OK |
| agent.md | OK |
| start.bat | minor issues |
| build.bat (new) | minor issues |
| debug.config.toml (new) | OK |

---

## Findings (ordered by severity)

### Major

*(none — no correctness or safety defects that would break behaviour or weaken
a guarantee)*

### Minor

**1. `set_model` locks `state.config` across no await but builds an `OpenAiClient`
inside the lock — fine, but the model-allowlist check can silently reject the
endpoint's *current* model after a config edit.**
`src-tauri/src/ipc/commands.rs:371` (`set_model`). The guard
`if !ep.models.iter().any(|m| m == &model)` rejects any model not listed in
`endpoints.toml`. That is correct as a validation boundary (prevents the
frontend from pointing the provider at an arbitrary model), but it means the
dropdown is the *only* source of truth and the running model set in
`build_brain` (which uses `config.general.general.default_model`, not
necessarily a member of `ep.models`) can never be re-selected via `set_model`.
Not a bug, but worth a comment or a fallback that treats `default_model` as
implicitly allowed. Low impact.

**2. `spawn_agent_shared` sends the initial prompt *after* releasing the manager
lock, so a `set_running`/`list_agents` between registration and the prompt send
sees the agent as idle — cosmetic race only.**
`src-tauri/src/ipc/commands.rs:288-294`. The agent is registered (visible in
`list_agents`) before its first `Prompt` command is queued. A UI polling
`list_agents` in that window shows `running: false` for a freshly spawned agent
that is about to start. Harmless (the `Started` event flips it), but the two
steps aren't atomic. Acceptable; note only.

**3. `set_model` error path returns a plain `Err` string but the factory is
touched before validation of factory presence.**
`src-tauri/src/ipc/commands.rs:409-421`. The `provider` is fully built (including
an env-var read for the API key) *before* checking `state.factory.is_some()`. On a
failed-brain startup this wastes a provider build and reads env vars needlessly,
then returns "agent factory unavailable". Reorder to check `state.factory` first.
Cosmetic/efficiency, not a correctness bug.

**4. `start.bat` `dev` path ignores the "CLI not found" guard.**
`start.bat:8-13`. The `if not exist "frontend\node_modules\.bin\tauri.cmd"` check
sits *after* the `dev` branch and uses `goto :end` to skip it — but the `dev`
branch runs *before* the check, so `start.bat dev` with no npm deps installed
fails with a raw "'..tauri.cmd' is not recognized" instead of the friendly
message. Move the existence check above the `dev` branch (or duplicate it there).
Minor UX.

**5. `start.bat`/`build.bat` use `set TAURI_EXIT=%ERRORLEVEL%` immediately after
`call`, which is correct, but `build.bat` never `popd`s back before the final
success `echo` path differs — consistent, fine.** Verified both capture
`%ERRORLEVEL%` *before* `popd` (which can reset it), so exit-code propagation is
correct. No action; noting that the pattern is right and should be kept if these
scripts are edited.

### Nit

**6. `commands.rs` — new `get_session_list` is missing a blank line / doc spacing
before the next `#[tauri::command]` (`get_config`).**
`src-tauri/src/ipc/commands.rs:524-525`. The `}` of `get_session_list` is
immediately followed by `#[tauri::command]` with no blank line. Cosmetic; add a
blank line for consistency with the rest of the file.

**7. `set_model` logs via `eprintln!` while the rest of the crate mixes `log`.**
`src-tauri/src/ipc/commands.rs:423` and `main.rs` (pre-existing). The new
`eprintln!("switched model…")` matches the surrounding style (main.rs uses
`eprintln!` for startup notes), so this is consistent — but the crate also
depends on `log`. Not worth changing; noting for awareness.

**8. `tauri.conf.json` `csp: null` (unchanged) leaves the webview without a CSP.**
`src-tauri/tauri.conf.json:26`. Not part of this diff, but since the app renders
agent output and lists files, a CSP would harden the webview against injected
markup. Out of scope; flagging as a standing hardening opportunity.

**9. `debug.config.toml` ships a sample pointing at `ollama-local` with
`auto-read-approve-writes` — appropriate for a debug sample and clearly
commented.** No issue. It does not weaken safety (writes still require approval
in that mode) and is opt-in (must be copied into `~/.myharness/`).

**10. `agent.md` edits are a path rename (`C:\myHarness` → `C:\AgenticCoder\…`),
a reworded pipe-exit-code rule (same semantics), and a new "Communication"
section.** None of these weaken safety guarantees — the exit-code rule is
preserved (arguably clearer), and the communication rules are additive. OK.

### Security — verified clean

**11. Command exposure:** All new commands (`set_model`, `get_session_stats`,
`get_project_stats`, `get_session_list`) are read/state operations or validated
model switches. None take a filesystem path or shell string. `read_file`,
`list_files`, `save_conversation`, `load_conversation` all route through
`state.sandbox.validate(...)` — confinement preserved. `set_model` validates
endpoint + model against `endpoints.toml`, so the frontend cannot point the
provider at an arbitrary host. OK.

**12. `shell:allow-open` in capabilities/default.json is the scoped open**
(allows `http(s)://`, `tel:`, `mailto:` only per the acl-manifest) — NOT
`allow-open` with a broad path scope, and `execute`/`spawn`/`kill`/`stdin_write`
are all absent from the capability. This is the recommended minimal shell
permission. No `allow-all` anywhere. OK.

**13. `spawn_agent` tool → `IpcSpawner`:** the tool's `safety()` is
`SafetyLevel::NeedsApproval` (src/tool/agent/spawn_agent.rs:81), so the new
tool-driven spawn path still requires user approval and goes through the normal
safety gate. The `IpcSpawner` shares the same manager/loops as UI spawns, so a
tool-spawned agent is governed identically. OK.

**14. `.coding/safety.toml` new rules** are additive auto-approve patterns for
specific, read-only/diagnostic shell commands (`cargo test`, `cargo build`,
`rustc --version`, `Get-Item …LastWriteTime` touch + build) and `complete_step`.
None auto-approve a destructive or write operation. Semantics preserved. OK.

---

## Signature / contract verification (all matched)

- `AgentSpawner::spawn(&self, name: &str, task: &str) -> Result<AgentId, String>`
  (src/runtime/mod.rs:136) — matches `IpcSpawner::spawn`. ✔
- `AgentLoopFactory::{set_spawner, set_provider, context_manager_for,
  memory_handle}` — all exist with matching signatures (src/agent/factory.rs). ✔
- `AgentLoop::{set_provider, session_id, workflow_handle}` — match. ✔
- `MemoryStoreTrait::{session_stats, project_stats, session_list}` — match
  (src/memory/mod.rs). ✔
- `Config::{endpoint, key_for}` + `Endpoint::effective_reasoning_effort` — match. ✔
- `AgentEvent::Usage` / `SerializableAgentEvent::Usage` now carry `cached_tokens`,
  `ttft_ms`, `generation_ms` (src/runtime/channels.rs:79-92, 160-167) — matches the
  extended test. ✔
- `OpenAiClientConfig.reasoning_effort: Option<String>` (src/provider/openai.rs:50)
  — matches both main.rs and commands.rs construction sites. ✔
- Frontend `AppConfig.pricing` / `EndpointInfo.reasoning_effort` /
  `PricingEntry.cached_per_1m` (frontend/src/lib/tauri.ts:110-124) align with the
  new `get_config` JSON shape. ✔
- `AgentId = u64` — serializes cleanly over IPC for the `agent_id` args. ✔

## Tests — validity

- `tests/ipc_bridge.rs::usage_serializes` asserts the three new fields serialize,
  appear in JSON, and round-trip with exact values. Correct and meaningful. ✔
- `tests/provider_integration.rs` adds `reasoning_effort: None` to the test client
  config — a mechanical field addition to satisfy the struct; integration tests
  remain `#[ignore]`d (require a live endpoint). ✔
- Note: `tests/ipc_bridge.rs` exercises the *core* serializable contract, not the
  Tauri layer directly (src-tauri is a separate crate). That's an accepted
  limitation documented in the test header; the new `set_model`/stats commands
  have no automated coverage at the IPC boundary.

---

## Could NOT verify

- **Runtime behaviour of `set_model` against a live endpoint** (provider swap,
  context-manager rebuild, per-loop `set_provider`) — verified by signature and
  logic read, not executed; no live endpoint available in review.
- **The Tauri event payload the frontend actually receives for the new stats
  commands** — `serde_json::to_value` of `SessionStats`/`ProjectStats`/
  `SessionSummary` assumed to match the TS interfaces; field names were compared
  by eye (they match), but not round-tripped through a running app.
- **`tauri::async_runtime::block_on` inside the sync `setup` closure** (main.rs)
  — assumed to be the supported pattern for briefly awaiting locks in a sync
  context; not run. It is the documented Tauri approach and the locks are
  uncontended at startup, so deadlock risk is negligible.
- **Whether `npm run dev` actually serves on port 5179** (matches the changed
  `devUrl`) — depends on the frontend Vite config, which is outside this review's
  file scope.

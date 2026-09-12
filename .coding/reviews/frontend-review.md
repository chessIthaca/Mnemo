# Frontend Code Review

Scope: uncommitted frontend changes (React/TypeScript) under `frontend/`, plus the new
untracked `frontend/src/components/views/StatsView.tsx`. Backend files
(`src-tauri/src/ipc/commands.rs`, `src/runtime/channels.rs`, `src/memory/types.rs`,
`src/config/endpoints.rs`) were read only to verify event/command contracts.

Verification performed: full diffs of all 13 modified files, full read of `StatsView.tsx`,
cross-checks of every new/changed IPC command signature + event payload against the Rust
sources, and `npx tsc --noEmit` in `frontend/` (exit 0, no type errors).

## Verdict per file

| File | Verdict |
|---|---|
| `frontend/src/hooks/useAgentEvents.ts` | OK |
| `frontend/src/lib/tauri.ts` | OK |
| `frontend/src/lib/types.ts` | OK |
| `frontend/src/hooks/useAgentStore.ts` | OK (minor nits) |
| `frontend/src/App.tsx` | OK |
| `frontend/src/components/views/StatsView.tsx` (new) | minor issues |
| `frontend/src/components/layout/StatusBar.tsx` | minor issues |
| `frontend/src/components/chat/InflightBar.tsx` | OK |
| `frontend/src/components/chat/Message.tsx` | OK |
| `frontend/src/components/layout/RightPanel.tsx` | OK |
| `frontend/src/components/layout/ConfigDialog.tsx` | OK |
| `frontend/src/styles/globals.css` | OK |
| `frontend/vite.config.ts` | OK |
| `frontend/package.json` | OK |

No critical or major findings. `tsc --noEmit` is clean.

## Findings (ordered by severity)

### Minor

1. **`StatsView.tsx` — `refresh()` (≈ lines 311–333): dropped-error handling is inconsistent.**
   The `getSessionList()` failure is swallowed to `[]` via `.catch(() => [])`, and a
   `getSessionStats()` failure is caught and assumed to mean "no session yet"
   (`setSessionStats(null)`). But a failure in `getProjectStats()` calls
   `setError(String(e))`. So if the memory store is unavailable, the project card shows an
   error while the session list silently goes empty and the session card silently shows
   "No active session". The result is a partially-degraded view with no single clear signal.
   Fix: surface a single error state when any of the three fetches fails (or mark each card
   independently), instead of mixing "silent empty" and "error banner" semantics.

2. **`StatsView.tsx` — real-time refresh effect (≈ lines 343–346): over-fetches the project
   stats + full session list on every token bump.**
   `tokenUsage` increments on *every* `Usage` event (once per LLM request, and more often if
   streaming usage deltas are emitted), and the effect re-runs the whole `refresh()` —
   including `getProjectStats()` (an aggregate query) and `getSessionList()` (a full table
   scan) — each time. Only the session card actually needs per-request freshness; the project
   card and session list change far less frequently. With several agents this can mean
   frequent redundant SQLite aggregate queries.
   Fix: split the refresh — re-fetch only `getSessionStats(activeAgent)` on `tokenUsage`
   bumps, and fetch project stats + session list on mount / agent change (or on an explicit
   refresh / interval). Also note the effect intentionally omits `refresh` from its deps
   (with an eslint-disable); that's fine, but the comment should say the deps are the
   *trigger*, not the full closure set.

3. **`StatsView.tsx` — `refresh` closure over `activeAgent` + two effects both calling it
   (≈ lines 333–346) can double-fetch on agent switch.**
   Changing `activeAgent` recreates `refresh` (dep of the first effect) *and* changes
   `tokenUsage` (dep of the second effect), so both effects fire and `refresh()` runs twice
   back-to-back. Harmless but wasteful (duplicate IPC round-trips on every agent switch).
   Fix: consolidate to a single effect keyed on `[activeAgent, tokenUsage?.prompt,
   tokenUsage?.completion, tokenUsage?.cached]`, or guard the token-usage effect to skip
   when only the agent changed.

4. **`StatusBar.tsx` — `selectReasoningEffort` / `selectModel` (≈ lines 160–185): label
   desyncs from the backend on failure.**
   On `setModel` failure the code only `console.error`s — the toolbar label still shows the
   old model/effort while the user thinks nothing happened. Worse, the store's `model` /
   `provider` / `reasoningEffort` are the *source* of the next `setModel(provider, model,
   effort)` call, so if a user manually edits `endpoints.toml` (or the backend's resolved
   endpoint differs), a subsequent effort change can call `set_model` with a stale
   endpoint/model and fail silently.
   Fix: surface a transient error (toast / inline message) on failure, and consider reading
   the authoritative current endpoint from the backend rather than trusting the local store.

5. **`StatusBar.tsx` — model/effort dropdowns are mouse-only (a11y).**
   The dropdowns open on `onClick` and close only on an outside `mousedown`. There's no
   `Escape`-to-close, no `aria-expanded`/`aria-haspopup`, and no keyboard navigation of the
   option list — inconsistent with good dropdown practice and with a keyboard-driven app.
   Fix: add an `Escape` keydown handler while open, `aria-expanded` on the trigger buttons,
   and (ideally) roving focus/`role="menu"`/`role="menuitem"` on the lists. (The pre-existing
   safety + workflow pickers in the same file have the same gap, so this is at least
   consistent.)

### Nit

6. **`useAgentStore.ts` — seven near-identical `setCode*Color` setters (≈ lines 497–610).**
   Each re-issues `applyCodeColors(...)` with all seven values via six `get()` calls. Verbose
   and easy to break when an eighth color is added. A single `setCodeColor(key, value)`
   (or building the color object once from `get()`) would collapse ~110 lines to a handful.
   Not a correctness issue — the current code is correct and consistent with the existing
   `setAccentColor`/`setBorderColor` pattern.

7. **`Message.tsx` — `CodeBlock`: `navigator.clipboard.writeText(String(children))`.**
   `children` for a highlighted block is a React node tree; `String(children)` yields
   `[object Object]` for anything but a plain string, so the copied text can be wrong for
   nested/highlighted content. This line predates the diff (only the class handling above it
   changed), so it's out of scope — flagging since it's in a touched file. Fix: extract text
   via a ref on the `<code>` element (`codeRef.current?.textContent`) for the copy payload.

## Cross-checks that PASSED (no action needed)

- **`Usage` event payload** — frontend `types.ts` `usage` variant
  (`prompt_tokens`, `completion_tokens`, `reasoning_tokens`, `cached_tokens`,
  `ttft_ms: number|null`, `generation_ms: number|null`) exactly matches the Rust
  `SerializableAgentEvent::Usage` (`u32` + `Option<u32>` → JSON number/null), serialized with
  `#[serde(tag="kind", rename_all="snake_case")]`. ✔
- **Stats IPC contracts** — `get_session_stats` / `get_project_stats` / `get_session_list`
  return `serde_json::Value` serialized from `SessionStats` / `ProjectStats` /
  `Vec<SessionSummary>`; the TS interfaces in `tauri.ts` match `src/memory/types.rs`
  field-for-field (`u64`/`i64` → JSON number, `Option<i64>` → `| null`). ✔
- **`get_config` shape** — backend emits `{general:{default_provider,default_model,safety},
  endpoints:[{name,kind,base_url,models,reasoning_effort}], pricing:[{model,input_per_1m,
  output_per_1m,cached_per_1m}]}`; matches the new `AppConfig` TS interface. `reasoning_effort`
  is `Option<String>` → `string | null`, matching `reasoning_effort?: string | null`. ✔
- **`set_model` args** — Rust `set_model(endpoint_name, model, reasoning_effort)`; Tauri
  converts JS camelCase → Rust snake_case (consistent with the existing
  `send_prompt(agent_id,…)` ← `{agentId,…}` precedent), so `{endpointName, model,
  reasoningEffort}` is correct. ✔ The `"off"` → omit-field and `None` → endpoint-default
  mapping in the backend matches the frontend's doc comments.
- **Event listener singleton (`useAgentEvents.ts`)** — the module-level
  `ensureListenerStarted()` correctly fixes the StrictMode double-subscribe leak described in
  the header comment. The hook is mounted exactly once (App.tsx:21), so the single
  `activeDispatch` swap is safe, and the rAF buffers are flushed in the cleanup before detach.
  No leak, no double-listener. ✔
- **Port alignment** — `vite.config.ts` `port: 5179` matches `src-tauri/tauri.conf.json`
  `devUrl: http://localhost:5179` (both changed together). ✔
- **globals.css** — the `pre.hljs` scoping fix (box styles off the inline `<code>`) is
  consistent with the `Message.tsx` change that strips `hljs` from the `<code>` class list;
  the `--code-*` custom properties are all defined in `:root` and consumed by the `.hljs-*`
  rules. No broken selectors. ✔
- **package.json** — no new runtime deps added (only reordering + `vite ^5.4.11 → ^5.4.21`
  patch bump); `lucide-react@^0.460.0` already provides the icons used (`Gauge`, `BarChart3`,
  `Coins`, `Cpu`, `Hash`). ✔

## Could NOT verify

- **Runtime behaviour** (actual tok/sec values, cost estimates, dropdown open/close, real-time
  Stats refresh): not run — this was a static review only. The timing/cost math is sound on
  paper but not exercised.
- **`eslint`**: the repo's `package.json` has no lint script and no eslint config was found in
  the diff, so the `eslint-disable-next-line react-hooks/exhaustive-deps` in StatsView was
  reviewed by hand rather than by running the linter.
- **Backend Rust logic** (cache heuristic, TTFT capture, per-day bucketing): out of scope; only
  the *shapes* the frontend depends on were verified, not their numeric correctness.
- **`key_for` / config lock behaviour in `set_model`**: read but not traced end-to-end; assumed
  correct per its doc comment.

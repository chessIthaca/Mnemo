# Code Review — Endpoints editing tab in Settings (ConfigDialog)

**Reviewer:** read-only reviewer
**Date:** 2026-04-04
**Scope:** all uncommitted changes (`git diff HEAD`) implementing the Endpoints tab:
backend config `save()` methods + `Config::save_all`, Tauri `get_api_keys` /
`save_endpoints` commands, `EndpointDto` + validation, frontend
`EndpointsTab`/`EndpointCard`, `configVersion` store + StatusBar re-sync.
**Test status:** `cargo test` green — 39 config tests pass, 6 `endpoint_dto_tests`
pass (`myharness-app` package). Diff compiles clean (only pre-existing
`unused_mut` warnings in `src/runtime/agent.rs`, unrelated to this diff).

Findings are grouped by severity. File:line references use the post-diff
working tree.

---

## SECURITY — API-key handling

**No findings.** The secrets path is correctly isolated:

- `get_config` (`src-tauri/src/ipc/commands.rs:711-742`) builds a clean JSON
  object and never serializes `keys` / `KeyStore`. The new `max_context` /
  `max_output_tokens` / `multimodal` fields added to the `endpoints` payload
  are non-secret. Confirmed.
- `get_api_keys` (`commands.rs:806-818`) is the **only** secrets path: it
  returns `HashMap<String,String>` of endpoint→key, fetched on demand when
  the Settings dialog opens. No `eprintln!`/`dbg!`/`tracing` touches the map.
- `save_endpoints` (`commands.rs:830-1010`) never logs keys. The only
  `eprintln!` (line ~997) prints `kind_label` only. `EndpointDto` carries no
  `api_key` field, so even the derived `Debug` cannot leak a key. Validation
  errors format `ep.name` / `ep.models` / `self.base_url` — never a key.
- `KeyStore` has a manual redacting `Debug` impl (`keys.rs:97-104`) and
  `save()` (`keys.rs:82-93`) writes via `toml::to_string_pretty` + `fs::write`
  with no logging.
- Frontend never `console.log`s keys: `getApiKeys()` (`tauri.ts:164`) result
  is stored in `apiKeys` state and bound to a password `<input>` with
  `autoComplete="off"`. `loadError`/`saveError` use `String(e)` on Tauri
  error messages, which never include key payloads.

One note (not a finding): `KeyStore::save`'s doc comment
(`keys.rs:78-81`) claims "The map is sorted by key so the output is stable
across writes (independent of HashMap iteration order)." This is **false** —
`KeysFile.keys` is a `HashMap` (`keys.rs:25`), whose iteration order is
non-deterministic. See CORRECTNESS finding C5. (keys.toml lives in the global
config dir, not the repo, so this causes local file churn, not git churn.)

---

## CORRECTNESS

### C1 — [HIGH] Live provider is NOT re-synced when an endpoint's *contents*
change but its name (and the default model) stay the same.

`save_endpoints` decides whether to rebuild the live provider with
(`commands.rs:981-982`):

```rust
let provider_changed = old_default_provider != default_provider
    || old_default_model != default_model;
```

This only compares the **names** of the default endpoint + default model, not
their **contents**. If the user edits the *default* endpoint's `base_url`,
`kind`, `multimodal`, `reasoning_effort`, or its **API key** — while leaving
`default_provider` and `default_model` unchanged — `provider_changed` is
`false`, so block 4 is skipped. The TOML files are updated, but the running
`OpenAiClient` keeps the old base URL / kind / key / reasoning effort until a
restart or an explicit model switch via the status bar.

Concrete cases that silently leave a stale provider:
- Change the default endpoint's `base_url` (e.g. point OpenAI at a proxy) →
  live client still hits the old URL.
- Change the default endpoint's API key → live client still authenticates
  with the old key (until restart).
- Toggle `multimodal` on the default endpoint → live provider still strips
  image blocks.
- Change `reasoning_effort` on the default endpoint → live provider still
  sends the old effort.

The doc comment (`commands.rs:745-770`) even promises "if the default
endpoint **or** model changed relative to the currently running provider,
rebuilds the provider" — but the implementation only catches a *name* change,
not a content change.

**Suggested fix:** rebuild + swap whenever the default endpoint's identity
*or* any of its mutable fields (base_url, kind, multimodal,
reasoning_effort, api_key) differ from the values the live provider was built
from, OR (simpler and safe) always rebuild+swap when the default endpoint is
present in the saved set. The current "changed?" gate is an optimization that
is incorrect for in-place edits.

### C2 — [MEDIUM] `default_model` validation is stricter than `set_model`,
can block innocent saves.

`into_endpoint`/`save_endpoints` validation (`commands.rs:875-892`) rejects
a `default_model` that is not in the host endpoint's `models` list, with no
exception. The doc comment (`commands.rs:880-885`) claims leniency: "a model
that is the live running model but not in the endpoint's list is allowed
(mirrors set_model's implicit allow)". The code does **not** implement that
leniency — the `match` arm `Some(ep) if ep.models.iter().any(|m| m == dm) => {}`
is the only accept path; everything else errors.

`set_model` (`commands.rs:470-476`) explicitly allows
`config.general.general.default_model.as_deref() == Some(model)` even when
the model is absent from `ep.models`. So a config can legitimately have
`default_model = "foo"` where "foo" is the running model but not in the
endpoint's `models` array. If the user then opens the Endpoints tab, makes an
**unrelated** edit (e.g. toggles `multimodal`), and clicks Save —
`default_model` is still "foo", host.models lacks "foo" → **save is
rejected**, even though the user never touched the model. This is a
regression / footgun.

**Suggested fix:** match `set_model`'s leniency — also accept `dm` when it
equals the (pre-save) `old_default_model`, or drop the model-membership check
entirely and rely on the endpoint's `models` allowlist at `set_model` time.
At minimum, fix the doc comment to match the (strict) implementation.

### C3 — [LOW] Saving an endpoint with no models, set as default, silently
points the live provider at the hardcoded fallback model `"gpt-4o"`.

`save_endpoints` block 4 (`commands.rs:1003-1006`) resolves the model as
`default_model.clone().or_else(|| ep.models.first().cloned())
.unwrap_or_else(|| "gpt-4o".to_string())`. If the default endpoint has zero
models and `default_model` is None (the UI sets `defaultModel = ep.models[0]
?? null` → null on `onSetDefault`), the provider builds with model
`"gpt-4o"` — wrong for any non-OpenAI default endpoint (e.g. a local Ollama
endpoint). No error is surfaced. Edge case, but worth either rejecting a
default endpoint with no models, or falling back to the endpoint *kind*'s
default model rather than a hardcoded string.

### C4 — [LOW] Empty/blank model strings are written to `endpoints.toml`.

`addModel` (`ConfigDialog.tsx`) pushes `""` into `endpoint.models`.
`handleSave` sends the array verbatim — no trimming/filtering of empty
entries. `into_endpoint` (`commands.rs`) doesn't filter empties either. So a
user who clicks "Add model" then leaves it blank saves `models = ["",
"gpt-4o"]` to `endpoints.toml`. The datalist suggestion set (`allModelNames`)
correctly filters `m.trim()`, but the persisted array does not. Minor data
hygiene; duplicate model names within an endpoint are also unvalidated.

### C5 — [LOW] `KeyStore::save` doc comment is wrong (claims sorted output).

`keys.rs:78-81` states the output "is sorted by key so the output is stable
across writes (independent of HashMap iteration order)." `KeysFile.keys` is a
`HashMap` (`keys.rs:25`), so `toml::to_string_pretty` emits entries in
non-deterministic order. The file still parses correctly (round-trips), so
this is not a data bug — but the doc claim is false and the on-disk
`keys.toml` reorders on every save. Using a `BTreeMap` (or sorting before
serialization) would make the claim true. Low severity (keys.toml is in the
global config dir, not the repo).

### C6 — [LOW] `KeyStore::from_map` is dead code.

`keys.rs:59-61` adds `pub fn from_map` but the save path uses
`KeyStore::default()` + `insert` (`commands.rs:916-925`). `from_map` is never
called anywhere. Harmless public API, but unused. The plan listed it as
added; consider dropping it or wiring it in.

### C7 — Round-trip / non-general-section preservation — VERIFIED CORRECT.

- `Config::save_all` (`mod.rs:94-103`) writes `config.toml`
  (`GeneralConfig::save`), `endpoints.toml` (endpoints **+ pricing**), and
  `keys.toml`. `projects.toml` is correctly untouched.
- `GeneralConfig::save` (`general.rs:173-179`) serializes the whole struct
  (`general`/`context`/`memory`/`ui` + nested `vision_model`), so editing only
  `default_provider`/`default_model` preserves context/memory/ui/vision.
  Verified by `save_all_preserves_unrelated_general_sections`.
- Pricing is carried over verbatim in `save_endpoints`
  (`commands.rs:928`: `pricing: current.pricing.clone()`), so saving endpoints
  does not clobber `[[pricing]]`. Verified by
  `save_round_trips_endpoints_and_pricing`.
- `EndpointKind` casing: `get_config` sends `format!("{:?}", e.kind)` =
  "OpenAI"/"Local"; the UI normalizes to lowercase via `kindFromConfig`
  (`ConfigDialog.tsx`); `into_endpoint` (`commands.rs:789-797`) lowercases
  and accepts both forms. Round-trips correctly; tested by
  `accepts_debug_form_kind_case_insensitively`.
- Deleting an endpoint drops its key: `deleteEndpoint` removes from
  `apiKeys` (`ConfigDialog.tsx`), and `save_endpoints` only keeps keys for
  endpoints in `built` (`commands.rs:919-925`). Verified.
- Empty API-key strings are dropped: `save_endpoints` skips `k.is_empty()`
  (`commands.rs:922-924`). Verified.
- `max_context`/`max_output_tokens`/`multimodal` round-trip: `get_config`
  sends them; `load()` maps them; `EndpointDto` carries them; `Endpoint`
  persists them. Verified (struct fields + test coverage).

### C8 — Lock handling — VERIFIED CORRECT (no deadlock, no I/O under lock).

`save_endpoints` matches `set_model`'s lock discipline:
- Block 2 (`commands.rs:915-945`): locks `state.config`, builds `new_config`
  + captures old defaults, **drops** the lock (scoped block returns tuple).
  No `await` while holding the config lock.
- `save_all` + `Config::load` (fs I/O) run **outside** any lock.
- Block 3 (`commands.rs:953-958`): re-locks `state.config` only for the
  pointer swap, then drops.
- Block 4 (`commands.rs:965-998`): re-locks `state.config` for the *sync*
  `build_openai_client` (no await inside), drops before
  `factory.set_provider` and before locking `state.agent_loops`.

Lock order is `config → (drop) → agent_loops` — identical to `set_model`
(`commands.rs:460-500`), so there is no lock-order inversion / deadlock with
concurrent `set_model`/`get_config`/`get_api_keys`. The `context_manager` is
rebuilt via `factory.context_manager_for` and swapped into each live loop
(`commands.rs:990-993`), exactly mirroring `set_model` — no missing
context-manager swap. `context_manager_for` (`factory.rs:186-188`) is sync and
holds no `state.config` lock. ✓

---

## BUGS (frontend logic)

### B1 — [MEDIUM] Duplicate `id="all-models-datalist"` across endpoint cards.

`EndpointCard` renders `<datalist id="all-models-datalist">` inside its JSX,
and `EndpointsTab` renders one `EndpointCard` per endpoint, so N cards → N
`<datalist id="all-models-datalist">` elements in the DOM. HTML `id` must be
unique; browsers resolve `list="all-models-datalist"` to the *first* match.
Functionally this is harmless because every card's datalist is built from the
same shared `allModelNames` (passed from the parent), so suggestions are
identical regardless of which datalist wins — but it is invalid HTML and a
latent footgun (if per-card suggestion sets ever diverge, only the first
card's set would be used). **Suggested fix:** render the `<datalist>` once at
the `EndpointsTab` level (outside the `.map`), or make the id unique per card
(e.g. `all-models-datalist-${endpoint.name}`).

### B2 — [LOW] Unused `index` prop on `EndpointCard`.

`ConfigDialog.tsx:632` passes `index={i}` and the prop type declares
`index: number;` (line 730), but `index` is never destructured or referenced
in the component body (confirmed: `index` appears only at the call site and
the type). Dead prop. Not a compile error (TypeScript doesn't flag
undeclared-but-typed props), but it's noise and `react/no-unused-prop-types`
may flag it under a strict ESLint config. Remove the prop or use it.

### B3 — [LOW] `key={i}` (array index) on `EndpointCard` can misplace local
`showKey` state on delete.

`EndpointsTab` keys `EndpointCard` by array index `i`. Each card holds a
local `showKey` boolean (password visibility). Deleting a card above the
default-visible one causes React to reuse component instances by position,
so a different card inherits the previous `showKey=true` — the eye-toggle
state "jumps" to the wrong endpoint. Data is unaffected (the password values
live in parent state keyed by name), but the visible/hidden state can briefly
show the wrong card's key as revealed. Using a stable key (e.g. a generated
id, or the endpoint name once non-empty) would prevent this. Low severity.

### B4 — `onNameChange` re-keying + `defaultProvider` — VERIFIED CORRECT.

`onNameChange` (`ConfigDialog.tsx`) updates `apiKeys` (delete old name, set
new), then `defaultProvider` if it matched the old name, then
`updateEndpoint(i, { name: newName })`. React batches these setters, so on
the next render `endpoints[i].name === newName` and `apiKeys[newName]`
exists, and the card reads `apiKey={apiKeys[ep.name]}` consistently. No race
with `defaultProvider`: the rename updates `defaultProvider` synchronously
before `updateEndpoint` flushes, and the dirty-check `serialize` re-runs on
the new state. The `index`-based key (B3) does not affect this since renaming
doesn't change array position. ✓

### B5 — `deleteEndpoint` default fallback — VERIFIED CORRECT.

`deleteEndpoint` (`ConfigDialog.tsx`) on deleting the default endpoint sets
`setDefaultProvider(endpoints.find((_, idx) => idx !== i)?.name ?? null)`.
`endpoints` here is the closure value *including* the to-be-deleted entry, and
`.find((_, idx) => idx !== i)` correctly skips the deleted index, returning
the next remaining endpoint's name (or null if it was the only one).
`setDefaultModel(null)` is correct (the new default endpoint's model is
re-chosen on next user action). No bug. ✓

### B6 — `radio name="default-endpoint"` shared across cards — VERIFIED
CORRECT.

All default-endpoint radios share `name="default-endpoint"`, forming a single
radio group — exactly the desired "only one default" behavior. `checked` is
controlled by `isDefault`. No bug (a shared `name` is the *correct* way to
group radios). ✓

### B7 — [LOW] "Saved — the live provider was re-synced" success message is
shown even when no swap occurred.

`handleSave` sets `savedOk=true` unconditionally on a successful
`saveEndpoints` call, and the banner says "Saved — the live provider was
re-synced." But the backend returns `provider_swapped` and the swap is
skipped when (a) `provider_changed` is false (see C1 — this is the common
case of an in-place edit, where nothing is re-synced at all), or (b)
`state.factory` is None, or (c) no default endpoint exists. The message is
misleading in those cases. Either gate the message on `provider_swapped`, or
make the copy accurate ("Endpoints saved." + a conditional "provider
re-synced" line).

---

## CONSTITUTION COMPLIANCE

**No blocking findings.**

- **Windows/PowerShell:** No new shell commands are introduced by this diff
  (all changes are Rust + TS). The reviewer's own `cargo test` invocations
  used PowerShell syntax and unpiped `$LASTEXITCODE` checks per the
  constitution. ✓
- **Doc comments on public functions:** All new public Rust items have doc
  comments — `EndpointDto` + `into_endpoint` (`commands.rs:744-786`),
  `get_api_keys` (`commands.rs:796-804`), `save_endpoints`
  (`commands.rs:768-795`), `Config::save_all` (`mod.rs:86-94`),
  `endpoints::save` (`endpoints.rs:115-117`), `GeneralConfig::save`
  (`general.rs:165-172`), `KeyStore::save`/`from_map`/`insert`
  (`keys.rs:56-93`). Frontend wrappers (`getApiKeys`/`saveEndpoints`,
  `tauri.ts:157-210`) and store fields/setters have JSDoc. ✓
- **`cargo test` before step complete:** Run by the reviewer — 39 config
  tests + 6 `endpoint_dto_tests` pass; diff compiles. (The parent agent
  must still run the full `cargo test` itself per the closing sequence.) ✓
- **No commits to main:** This is a feature branch; the diff is uncommitted
  working-tree changes only. No merge/push performed. ✓
- **Line-ending style:** New files use the project's style; the `git diff`
  warns about LF→CRLF on `.coding/backlog.json` and a plan file, but those
  are pre-existing bookkeeping files touched outside this plan's scope (see
  "Out-of-scope" below), not source files introduced by this diff. The new
  Rust/TS edits follow existing style. ✓

### Out-of-scope (pre-existing, not introduced by this diff)
- `.coding/backlog.json` (status flip on item id 6) and
  `.coding/plans/d10066a6-….md` (checkbox flips) + `.coding/plans/stack.json`
  (stack pointer) are bookkeeping churn from a *different*, earlier plan
  (the "safety checks removal" plan). Not part of the Endpoints-tab work;
  flagged only for completeness — the parent agent's commit should decide
  whether to include or exclude them.
- Pre-existing `unused_mut` warnings in `src/runtime/agent.rs:448,511`
  (`let (fanin_tx, mut fanin_rx)`) — unrelated to this diff.

---

## Summary

The diff is well-structured, correctly isolates secrets, and the config
round-trip + lock discipline are sound (verified by tests + code reading).
The notable correctness gap is **C1**: in-place edits to the *default*
endpoint (base_url, kind, multimodal, reasoning_effort, or API key) do **not**
re-sync the live provider, because the swap gate compares only endpoint +
model *names*. **C2** makes `default_model` validation stricter than
`set_model`, which can reject innocent saves. Both should be addressed before
merge. The remaining findings (C3-C6, B1-B3, B7) are low-severity hygiene /
UX. Security and constitution compliance are clean.
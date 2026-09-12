# Code Review — Endpoints: fetch model picker + base_url in collapsed card

**Reviewer:** read-only reviewer
**Date:** 2026-04-04
**Scope:** all uncommitted changes (`git diff HEAD`) for plan
"Endpoints: fetch model picker + base_url in collapsed card".

Files reviewed:
- `src-tauri/src/ipc/commands.rs` — new `list_models` Tauri command
- `src-tauri/src/main.rs` — registered `list_models` in `invoke_handler`
- `src/provider/openai.rs` — `fetch_models`, `parse_model_ids`,
  `truncate_string` + unit tests
- `frontend/src/lib/tauri.ts` — `listModels` typed wrapper
- `frontend/src/components/layout/ConfigDialog.tsx` — base_url moved into the
  always-visible row; live model picker (`ModelPickerDropdown`), `addModelWithValue`,
  `onPickModel`, fetch/cache/outside-click state

Overall: the backend is clean and well-tested; the security posture (no key
leak, no arbitrary-URL abuse beyond configured/edited base_url, `get_config`
still secret-free) holds. The frontend has two real bugs in the focus-mode
editing path and one cache-guard edge case worth fixing. Details below.

---

## CORRECTNESS

### C1 — Focus-mode typing can clobber the saved model with `""` (real bug)
`frontend/src/components/layout/ConfigDialog.tsx:1098-1102`

In focus mode the input is controlled by `pickerQuery`, not by the model `m`:

```tsx
value={pickerOpen === mi ? pickerQuery : m}
onChange={(e) => {
  setPickerQuery(e.target.value);
  if (e.target.value !== m) onModelChange(mi, e.target.value);
}}
```

`onModelChange` → `setModelAt` writes the typed value straight into
`endpoint.models[mi]` on **every keystroke** (as long as it differs from the
original `m`). Consequence: if the user focuses a row holding `gpt-4o`, clears
the field to type a filter, and then clicks outside (or hits Escape — there is
no Escape handler, see C2) without picking, `endpoint.models[mi]` is now `""`.
The committed model list silently gains a blank entry; on save the blank is
persisted. The display still reads `pickerQuery` so the user does not see the
damage until the picker closes and the row reverts to showing `m` — which is now
`""`.

This is the core correctness issue the plan flagged. The intent of the
`!== m` guard was "only commit when actually changed," but combined with the
controlled-by-`pickerQuery` display it commits *intermediate* filter text as
the real model value.

Recommended fix: in focus mode, do **not** call `onModelChange` on each
keystroke. Only mutate the model on a definitive action:
- when the user picks an item from the dropdown (`onPick` already does
  `onModelChange(mi, id)`), and
- when the picker closes (commit `pickerQuery` once, if it differs from `m`),
  e.g. in the outside-click effect / an onBlur that fires on close.

i.e. remove the `if (e.target.value !== m) onModelChange(...)` from `onChange`
and let the close path commit. Keep `setPickerQuery` in `onChange` so the
filter still works. This also removes the "dropped edits" concern the plan
raised: with per-keystroke `onModelChange` gone, there is no edit to drop.

### C2 — No Escape / blur-commit path; the only close is outside-click
`frontend/src/components/layout/ConfigDialog.tsx:892-900` (outside-click effect),
`:1097-1108` (input)

The dropdown closes only via the `mousedown` outside-click listener. There is
no `onKeyDown` Escape handler and no commit-on-blur. So:
- Pressing Escape does nothing (commonly expected to dismiss a popover).
- Tabbing away from the input fires a real blur; because the dropdown's
  `onMouseDown` only `preventDefault`s clicks **on buttons/`[role=option]`**,
  the filter `<input autoFocus>` inside the dropdown can steal focus on mount
  (see C3), and tabbing out of *that* input blurs without closing the picker
  (the outside-click listener only fires on `mousedown`, not on focus moves),
  leaving an orphaned dropdown.

Recommended fix: add `onKeyDown` Escape → `setPickerOpen(null)` on the row
input, and commit `pickerQuery` to the model when the picker closes (see C1).

### C3 — `autoFocus` on the dropdown's filter input steals focus from the row input
`frontend/src/components/layout/ConfigDialog.tsx:1220-1228` (filter input in
`ModelPickerDropdown`)

`ModelPickerDropdown` renders an `<input autoFocus ...>` filter box. In
**focus mode** this dropdown mounts inside the row right under the model input
the user just focused. `autoFocus` moves focus to the dropdown's filter input,
so the row input immediately loses focus. Two symptoms:
- The row input's `onFocus`-fetched list is correct, but the user's caret is
  no longer in the row input — typing goes to the dropdown filter, not the row.
  The row input's `onChange` (which drives `pickerQuery` in focus mode) never
  fires from the keyboard; only the dropdown filter updates `pickerQuery`.
  Since both call `setPickerQuery` the *filtering* still works, but the
  row-input-as-filter UX implied by `value={pickerOpen === mi ? pickerQuery : m}`
  is broken — the row input is effectively read-only once the dropdown opens.
- Combined with C1, the row input's `onChange` rarely fires (only for the very
  first keystroke before focus moves), so the model-clobber bug is mostly
  masked in practice — but if `autoFocus` is ever removed or delayed, C1 bites
  immediately.

Recommended fix: do **not** render the `autoFocus` filter input in focus mode
(the row input *is* the filter in that mode). Only auto-focus the filter in
add-mode (`pickerOpen === "__add__"`), where there is no row input to type in.
Pass an `autoFocus` prop into `ModelPickerDropdown` and set it based on mode.

### C4 — `pickerRef` wraps the whole Models section, so the "Pick from server" button and every row share one outside-click region
`frontend/src/components/layout/ConfigDialog.tsx:1047` (`<div ... ref={pickerRef}>`)

`pickerRef` is on the container that holds the Models header, the add-mode
dropdown, the empty hint, and **all** model rows + their focus-mode dropdowns.
The outside-click effect (`:892-900`) closes the picker only when the click is
**outside** `pickerRef`. Since every row and both dropdowns are inside it,
clicks on any row input (to switch which row's picker is open) are "inside," so
the picker never closes before `openRowPicker` reopens it for the new row.
That is actually fine for row-to-row switching, but it means: clicking the
"Pick from server" button while a row picker is open does **not** close the row
picker first — `openAddPicker` just overwrites `pickerOpen` to `"__add__"`,
which works. No correctness bug, but the wrapping is wider than the comment
implies ("anchored to the row"). Acceptable; noting for clarity.

### C5 — Moving base_url/kind out of the expanded grid loses no behavior
`frontend/src/components/layout/ConfigDialog.tsx:945-993` (new always-visible
row) vs. the removed `grid grid-cols-[7rem_1fr]` block.

The old collapsed section was a `<button>` showing a `kind` badge + base_url +
model count, and clicking it expanded the card. The new always-visible row
replaces the badge with a real `<select>` and adds a base_url `<input>`, both
editable while collapsed. The model-count `<button>` still toggles expand.
Reviewing for lost behavior:
- The `kind` badge was display-only in collapsed mode; the `<select>` is now
  editable in collapsed mode. This is a behavior *gain*, not a loss. The
  expanded grid's `<select>` was removed, so there is no duplicate select —
  good (the comment explicitly calls this out).
- The `aria-expanded` on the toggle button now reflects `open` (was hardcoded
  `false`) — improvement.
- No `onEndpointChange` wiring was lost: both `kind` and `base_url` still call
  `onEndpointChange({ ... })`.

No correctness regression from the move.

---

## BUGS

### B1 — Cache guard blocks refetch after an empty list AND after an error
`frontend/src/components/layout/ConfigDialog.tsx:850`

```ts
if (key === modelsCacheKey && modelsCache.length > 0 && fetchState.status === "ready") {
  return; // Already cached for this url+key.
}
```

Two cases where this guard misbehaves:

1. **Empty list (server returned 0 models).** `fetchServerModels` sets
   `fetchState` to `{ status: "error", message: "No models returned..." }` and
   `modelsCache` stays `[]` (line 863 sets error status; the success branch
   only sets `ready` when `list.length > 0`). So `modelsCache.length > 0` is
   false → the guard never short-circuits → **every re-focus/refetch retries
   the server** even though we already know it returns nothing. Not a bug per
   se (retries are harmless-ish), but the cache is effectively disabled for
   empty endpoints, which contradicts the "cached per url+key" design. More
   importantly, an endpoint that legitimately returns an empty list is
   permanently in `error` status (see B2).

2. **After an error.** Same as above: `modelsCache` is `[]`, `fetchState` is
   `error` → guard never short-circuits → every focus re-hits the server. This
   is actually *desirable* for errors (you want a retry), so the behavior is
   fine, but the guard's *comment* ("Already cached") is misleading because it
   never fires for these states. Minor.

The real issue is B2 (below). The guard itself is safe — it only ever
short-circuits when there is a real, non-empty, ready cache. No incorrect
*stale* data is shown. Low severity.

### B2 — An endpoint returning an empty model list is shown as an error, not a (valid) empty state
`frontend/src/components/layout/ConfigDialog.tsx:863`

```ts
setFetchState(list.length > 0 ? { status: "ready" } : { status: "error", message: "No models returned by this endpoint." });
```

A server that returns `{ "data": [] }` (valid OpenAI shape, just empty) is
treated as an error. `parse_model_ids` returns `None` for an empty `data`
array **only when there is no `models` fallback** — wait: `parse_model_ids`
returns `None` when both arrays are empty/absent, so `fetch_models` returns
`Err(...)` ("could not find a model list") for `{ "data": [] }` with no
`models` key. So the empty-list path here is actually only reached when the
server returns e.g. `{ "data": [], "models": [] }` (both present but empty) or
when `parse_model_ids` returns `Some(vec![])` — which it never does, because
both branches require `!ids.is_empty()` to return `Some`. So `list` from
`fetch_models` is **always non-empty** on success (empty → `Err`).

That means line 863's `list.length > 0` branch is the only reachable one on
success; the `error` branch is dead code given the current `fetch_models`
semantics. Not harmful, but the "No models returned" error message can never
appear; the user instead sees "could not find a model list in the response…".
Minor inconsistency between the two layers. Consider returning `Ok(vec![])`
from `fetch_models` for a present-but-empty `data`/`models` array so the UI's
empty-state ("No models available." at `:1255`) is reachable, or drop the dead
branch. Low severity.

### B3 — `modelsCacheKey` is set BEFORE the await, but a failed/replaced fetch can leave a stale key
`frontend/src/components/layout/ConfigDialog.tsx:858-867`

`setModelsCacheKey(key)` runs before `await listModels(...)`. If the user
edits the base_url while a fetch is in flight, a second `fetchServerModels`
starts: it computes a *new* `key`, sees `key !== modelsCacheKey` (old key),
proceeds, and sets `modelsCacheKey` to the new key. Now two fetches are in
flight. Whichever resolves **last** wins (`setModelsCache` / `setFetchState`),
which may be the one for the *old* URL. The cache key then reflects the
last-set key (the new one, set synchronously up front), but `modelsCache`
holds the stale-URL result. Result: cache shows models from URL A while the
cache key says URL B → a subsequent focus sees `key === modelsCacheKey` and
**short-circuits**, showing the wrong list.

This is a classic stale-closure / race on a fetch-with-no-cancellation bug.
Realistic trigger: user types a base_url, focus fires a fetch, user keeps
typing the URL (each keystroke doesn't refetch — refetch only happens on
focus/refresh — so the race window is narrow, mainly "focus, then immediately
edit URL and re-focus"). Lower likelihood but real.

Recommended fix: capture an `AbortController` per fetch or a request-id token;
in the `.then`/`catch`, ignore the result if the current `modelsCacheKey` no
longer matches the captured key (or if `pickerOpen` has closed). At minimum,
set `modelsCacheKey` only **after** a successful fetch, and on the success path
verify the URL still matches what's in `endpoint.base_url` before committing.

### B4 — Refresh button clears cache but the in-flight `fetchState` is overwritten to `loading` only if not already cached
`frontend/src/components/layout/ConfigDialog.tsx:1078-1082`, `1139-1143`

`onRefresh` does `setModelsCache([]); setModelsCacheKey(""); void fetchServerModels();`.
Clearing `modelsCacheKey` to `""` guarantees the guard at `:850` won't
short-circuit, so a refetch always happens — correct. But `modelsCache` is
emptied immediately, so the dropdown body flips to the `loading`/`error` state
and the previously-shown list disappears during the refetch. Minor UX flicker;
not a bug. Acceptable.

---

## SECURITY

### S1 — No API key is leaked in error messages ✓
`src/provider/openai.rs:130-167`, `src-tauri/src/ipc/commands.rs:901-903`

Verified each error path in `fetch_models`:
- `failed to build HTTP client: {e}` — reqwest builder error; no key. ✓
- `failed to reach {url}: {e}` — reqwest send error; the reqwest error type
  does **not** echo request headers (Authorization) in its `Display`, so the
  Bearer token is not included. The URL is included, which is fine (it's the
  configured base_url, not a secret). ✓
- `HTTP {status} from {url} — {truncate_string(&text, 500)}` — the **response
  body** is truncated to 500 chars. A misconfigured/malicious server *could*
  echo the Authorization header back in its response body, which would then
  appear (truncated) in the error message shown in the UI. This is a
  low-probability but real leak vector: the error string flows to the frontend
  via `e.to_string()` and is rendered at `ConfigDialog.tsx:1244`
  (`{error || "Failed to fetch models."}`) and `:866`.
  Mitigations already in place: only 500 chars, and the user controls the
  base_url (it's their own endpoint). But a server returning
  `{"error":"unauthorized; got bearer sk-..."}` would surface the key.
  Recommended hardening: never include the response body for auth-failure
  statuses (401/403), or scrub the body for the key substring before
  formatting. Low severity given the threat model (user's own endpoint), but
  worth noting.
- `failed to parse /models response as JSON: {e}` — serde_json error; no
  key. ✓
- `could not find a model list...` — static string. ✓

`list_models` maps via `.map_err(|e| e.to_string())`; `Error::Provider`
`Display` is `"provider error: {0}"` (`src/error.rs:17-18`), so the UI sees
`"provider error: failed to reach ..."`. No key in any of these. ✓ (The one
  caveat is S1's response-body echo, above.)

### S2 — No arbitrary-URL abuse beyond configured/edited base_url ✓ (acceptable)
`src-tauri/src/ipc/commands.rs:883-895`

`base_url` resolves to: the passed `base_url` (if non-empty) → the saved
endpoint's `base_url` → error. The passed value comes from the dialog's
`endpoint.base_url` field, which the user edits. So a user can point the
picker at any URL they type — but that's the same trust level as the existing
endpoint base_url field (which is used verbatim for chat completions). There
is no SSRF escalation: the command doesn't accept an arbitrary URL from an
untrusted source; it's driven by the same config the user already controls.
The `/models` path is appended server-side (`format!("{}/models", ...)`), so
the user can't redirect to an arbitrary path on the host beyond `/models`.
Acceptable. ✓

### S3 — `get_config` no-secrets guarantee intact ✓
`src-tauri/src/ipc/commands.rs:711-742`

`get_config` was not modified. It still builds its JSON by hand without ever
reading `keys`/`key_for`. The new `list_models` command reads the key
in-process and sends it over HTTPS to the endpoint; it does **not** return the
key to the frontend (returns `Vec<String>`). The `listModels` TS wrapper
returns `string[]`. No key crosses the IPC boundary. ✓

### S4 — Key resolution fallback chain matches the provider ✓
`src-tauri/src/ipc/commands.rs:889-894` vs `src/provider/client_factory.rs:20-27`

`list_models` resolves: passed key → `config.key_for(name)` →
`OPENAI_API_KEY` env → `ANTHROPIC_AUTH_TOKEN` env → `"dummy"`. This matches
`resolve_api_key` exactly (stored → OPENAI_API_KEY → ANTHROPIC_AUTH_TOKEN →
dummy). The command's doc comment claims it "mirrors the provider's key
fallback chain in `build_openai_client`" — verified accurate. The one
difference: `list_models` lets a *passed* `api_key` override the stored key
(which the provider path doesn't need, since it always uses the saved
endpoint). This is the intended "reflect unsaved edits" behavior. ✓

---

## CONSTITUTION COMPLIANCE

### K1 — Public Rust functions have doc comments ✓
- `fetch_models` (`src/provider/openai.rs:118-122`) — has `///` doc. ✓
- `parse_model_ids` (`:184-190`) — private (`fn`, not `pub`), has doc. ✓
- `truncate_string` (`:209-211`) — private, has doc. ✓
- `list_models` (`src-tauri/src/ipc/commands.rs:845-870`) — `#[tauri::command]
  pub async fn`, has a multi-line doc comment. ✓
All new public items are documented.

### K2 — TS wrapper has a doc comment ✓
`frontend/src/lib/tauri.ts:169-185` — `listModels` has a JSDoc block
describing resolution + the no-leak guarantee. ✓

### K3 — Tests added for new logic; `fetch_models` testability ✓
`src/provider/openai.rs:1551-1616` — six new tests:
- `parse_model_ids_openai_shape`, `_ollama_shape`,
  `_openai_data_takes_precedence_when_present`, `_empty_data_falls_back_to_models`,
  `_unknown_shape_returns_none` — good coverage of the parser, including the
  OpenAI-takes-precedence and empty-`data`-falls-back edge cases.
- `truncate_string_short_kept_intact`, `_long_cut_with_ellipsis`,
  `_respects_char_boundaries` — covers the helper including the char-vs-byte
  truncation (the `é` test is a nice catch).

`fetch_models` itself is not unit-tested. There is no HTTP mock dependency in
the crate (no `mockito`/`wiremock`/`httpmock` found in `Cargo.toml` or source),
so testing it would require adding a dev-dependency. Given the function is a
thin reqwest wrapper around the already-tested `parse_model_ids`, leaving it
untested is a reasonable scope decision — the constitution says "tests added
for new logic," and the *logic* (parsing, dedupe, truncation) is tested. Not a
finding. ✓

### K4 — Line-ending style ✓
The diff shows no mixed endings introduced. The git warning about LF→CRLF on
the `.coding/plans/*.md` file is git's autocrlf normalization on an existing
tracked file, not something this change introduced, and `.md` plan files are
not source. The Rust and TSX files use the repo's existing style. ✓

### K5 — Windows paths / PowerShell ✓
N/A for code (no shell commands in the diff). ✓

### K6 — `cargo test` gating ✓
The plan's step 8 marks `cargo test` as done. The new tests are standard
`#[test]` fns and should compile/run. (This review is read-only and did not
re-run them.)

---

## SUMMARY OF ACTIONABLE FINDINGS (by priority)

1. **C1 (bug, high):** Focus-mode `onChange` commits every keystroke to
   `endpoint.models[mi]` via `onModelChange`, so clearing the field then
   closing without picking clobbers the saved model with `""`. Remove the
   per-keystroke `onModelChange`; commit only on pick and on picker-close.
   `ConfigDialog.tsx:1099-1102`.
2. **C3 (bug, medium):** `autoFocus` on the dropdown's filter input steals
   focus from the row input in focus mode, making the row input read-only
   once the dropdown opens. Only auto-focus the filter in add-mode.
   `ConfigDialog.tsx:1220-1228`.
3. **B3 (bug, medium):** In-flight fetch + URL edit can race; the last
   resolver wins and the cache key no longer matches `modelsCache`, so a
   later focus short-circuits and shows the wrong list. Guard the
   resolve path with a request-id/AbortController or commit the cache key
   only on success. `ConfigDialog.tsx:858-867`.
4. **C2 (bug, low):** No Escape-to-close or blur-commit; add an Escape
   handler. `ConfigDialog.tsx:1097-1108`.
5. **S1 (security, low):** The HTTP-error path includes 500 chars of the
   response body; a server echoing the Bearer token back would leak it to
   the UI. Suppress the body for 401/403 or scrub the key substring.
   `src/provider/openai.rs:147-154`.
6. **B2 (bug, low):** Empty-but-valid model list is unreachable as a UI
   "empty" state (it becomes a parse error); the `list.length > 0` branch
   at `ConfigDialog.tsx:863` is dead. Align the two layers or drop the dead
   branch. `ConfigDialog.tsx:863`.

No findings on: backend key resolution (S2/S4), `get_config` secrecy (S3),
the base_url/kind move (C5), doc comments (K1/K2), or test coverage (K3).
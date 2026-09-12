# Review: `supports_reasoning_effort` endpoint capability flag

**Date:** 2026-08-11  
**Reviewer:** read-only reviewer subagent  
**Scope:** Feature files only (working tree has many unrelated dirty files — ignored).

## Intent

Some models reject the OpenAI-compatible `reasoning_effort` request parameter.
Add endpoint-level `supports_reasoning_effort` (default **true** for back-compat).
When false:

1. Never send `reasoning_effort` in the request body
2. Ignore toolbar effort dropdown value in `set_model`
3. Settings UI: checkbox + disable effort select
4. StatusBar: hide effort dropdown (show n/a)

## Files reviewed

| File | Role |
|------|------|
| `src/config/endpoints.rs` | Field + `effective_reasoning_effort()` |
| `src/provider/client_factory.rs` | Test `Endpoint` constructors |
| `src-tauri/src/ipc/commands.rs` | DTO, get_config/get_settings, set_model, tests |
| `frontend/src/lib/tauri.ts` | `EndpointInfo` / `EndpointEditable` |
| `frontend/src/components/settings/types.ts` | `blankEndpoint` |
| `frontend/src/components/settings/sections/EndpointCard.tsx` | Checkbox + disabled select |
| `frontend/src/components/settings/sections/ProvidersSection.tsx` | Load mapping |
| `frontend/src/components/settings/sections/AdvancedSection.tsx` | Import mapping |
| `frontend/src/components/layout/StatusBar.tsx` | Hide effort UI; force off on model switch |
| `frontend/src/App.tsx` | Seed effort from capability flag |

Related (not in intentional list; verified call sites still correct):

- `src-tauri/src/main.rs` — startup provider uses `ep.effective_reasoning_effort()`
- `src/provider/openai.rs` — omits body field when `reasoning_effort` is `None`

---

## Findings

### Correctness

**None blocking.** Core request-path gating is correct and layered:

1. **Config / parse** (`src/config/endpoints.rs:72–110`)  
   - `#[serde(default = "default_true")]` — missing key stays `true` (not plain `#[serde(default)]`, which would default bool to `false` and break back-compat).  
   - `effective_reasoning_effort()` returns `None` when `supports_reasoning_effort` is false **before** applying `"off"` / default-`max` logic. Explicit stored effort (e.g. `"high"`) cannot leak into the request.

2. **Startup** (`src-tauri/src/main.rs:291–297`)  
   Builds the live client with `ep.effective_reasoning_effort()`.

3. **`set_model`** (`src-tauri/src/ipc/commands.rs:471–485`)  
   Capability flag wins: `if !ep.supports_reasoning_effort { None }` — toolbar value is ignored. `"off"` / explicit value / endpoint default handled only when supported.

4. **`save_endpoints` rewire** (`src-tauri/src/ipc/commands.rs:1099–1105`)  
   Rebuilds provider with `ep.effective_reasoning_effort()` so toggling the flag and saving takes effect live.

5. **Wire / DTO**  
   - `get_config` and `get_settings` both emit `supports_reasoning_effort`.  
   - `EndpointDto` uses `default = "default_true_dto"`; tests cover absent-key → true and false round-trip + `effective_reasoning_effort() == None`.

6. **OpenAI client** (`src/provider/openai.rs:594–596`)  
   Adds `reasoning_effort` to the body only when `Some` — matches `None` from the gates above.

7. **Frontend**  
   - Settings: checkbox + disabled effort select (`EndpointCard.tsx:305–362`).  
   - Load/import default true when key absent (`ProvidersSection.tsx:55–56`, `AdvancedSection.tsx:193–197`).  
   - StatusBar hides dropdown / shows n/a; `selectModel` forces `"off"` when unsupported.  
   - App seed matches backend (`App.tsx:188–190`).

### Bugs

#### M1 (minor) — `effortOpen` not cleared when effort UI is unmounted

**File:** `frontend/src/components/layout/StatusBar.tsx`  
**Lines:** ~279–296 (`selectModel`), ~316–318 (`activeSupportsEffort`), ~455–517 (conditional render), ~209–219 (outside-click)

When the effort menu is open (`effortOpen === true`) and the user switches to an endpoint with `supports_reasoning_effort === false`:

1. The effort control unmounts (`activeSupportsEffort ? … : n/a`).
2. `selectModel` does **not** call `setEffortOpen(false)`.
3. The outside-click effect still depends on `effortOpen`, but `effortRef.current` is null while unmounted, so  
   `if (effortRef.current && !effortRef.current.contains(...))` never runs `setEffortOpen(false)`.

Result: `effortOpen` stays `true`. Switching back to a supported endpoint remounts the control with the menu **already open** without a click.

**Suggested fix (any one):**

- In `selectModel`, when forcing unsupported → `setEffortOpen(false)`.
- Or `useEffect(() => { if (!activeSupportsEffort) setEffortOpen(false); }, [activeSupportsEffort])`.

Does not affect request correctness (backend still omits the field).

#### N1 (nit) — Advanced import coercion vs Providers load

**Files:**  
- `AdvancedSection.tsx:194–197` — `undefined → true`, else `!!value`  
- `ProvidersSection.tsx:56` — `e.supports_reasoning_effort ?? true`

| Import value | AdvancedSection | ProvidersSection (`??`) |
|--------------|-----------------|-------------------------|
| missing / `undefined` | `true` | `true` |
| `null` | `false` (`!!null`) | `true` |
| string `"false"` | `true` (`!!"false"`) | N/A from backend |

`get_config` always sends a real bool, so the Settings load path is fine. Only malformed/hand-written import JSON is affected. Low practical risk; note for consistency with other boolean import fields.

#### N2 (nit) — unsupported → supported keeps toolbar `"off"`

**File:** `StatusBar.tsx:281–282`

```ts
const effort =
  ep?.supports_reasoning_effort === false ? "off" : reasoningEffort;
```

After using an unsupported endpoint (store forced to `"off"`), switching to a supported endpoint keeps `"off"` instead of adopting the target endpoint’s configured default (`reasoning_effort ?? "max"`). Spec only required forcing off for unsupported endpoints; reverse direction is intentional-looking but may surprise users. Optional polish: `effortForEndpoint(ep)` when the target supports effort.

### Security

**No findings.** Capability flag is config/UI only; no change to keys, approval gates, sandbox, or secret serialization. `get_config` / `get_settings` still omit API keys; `supports_reasoning_effort` is non-sensitive.

### Constitution compliance

**No findings** for the reviewed feature surface.

- Public API: `Endpoint::effective_reasoning_effort` retains a full doc comment; new field is documented; `EndpointDto` field documented.
- Private serde helpers (`default_true` / `default_true_dto`) need no public docs.
- No agent-driven merge/commit to `main` in this review scope.
- Tests added for parse default, parse false, effective omit when unsupported, DTO absent-key default, DTO false round-trip.

---

## Path coverage checklist (intent)

| Requirement | Status |
|-------------|--------|
| Default true for back-compat | ✅ serde `default_true` + DTO + FE `?? true` / `=== false` checks |
| Never send field when false | ✅ `effective_reasoning_effort` + `set_model` force `None` + client omits `None` |
| Ignore toolbar in `set_model` when false | ✅ `commands.rs:473–475` |
| Settings checkbox + disable select | ✅ `EndpointCard.tsx` |
| StatusBar hide / n/a | ✅ `activeSupportsEffort` branch |
| Force off on model switch (unsupported) | ✅ `selectModel` + App seed |

---

## Verdict

**Approve with one minor UI-state fix (M1).**

The capability flag is wired correctly end-to-end (config → effective effort → client factory / set_model / save rewire → request JSON). Back-compat default is correct (explicit `default_true`, not bare `#[serde(default)]` on bool).  

Only actionable issue: clear `effortOpen` when the effort control is hidden for unsupported endpoints (M1). N1/N2 are optional polish.

**No security or constitution findings.**

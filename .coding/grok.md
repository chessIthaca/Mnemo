# grok.md — Config Dialog Redesign Spec + Full Review Findings

**Date:** 2026-08-10  
**Branch reviewed:** `feat/bookkeeping-tools-autorun`  
**Scope:** Full product review with deep focus on Settings / config surface  
**Primary artifact reviewed:** `frontend/src/components/layout/ConfigDialog.tsx` (~1330 lines) plus config IPC (`src-tauri/src/ipc/commands.rs`), config schema (`src/config/*`), appearance store (`useAgentStore.ts`), and StatusBar model/safety controls  

This document is the implementation brief for a **much better config dialog**, followed by a **prioritized list of every finding** that should be addressed (config-specific first, then broader product debt still open).

---

## 1. Executive summary

The current Settings dialog is a **partial, two-tab modal** (Appearance + Endpoints) bolted onto a much richer config system that already exists on disk and in Rust:

| Config domain | On disk | Editable in UI today? |
|---|---|---|
| Theme / font / colors | `localStorage` only | Yes — Appearance tab (live, no Save) |
| Endpoints + API keys + default provider/model | `~/.myharness/endpoints.toml` + `keys.toml` + `config.toml` | Yes — Endpoints tab (explicit Save) |
| Safety mode | runtime + `config.toml` | **No** — StatusBar dropdown only |
| Safety rules (`safety.toml`) | project / global | **No** — Right-panel Safety editor only |
| Vision model | `config.toml` `[general.vision_model]` | **No** |
| Context fill rate | `config.toml` `[context]` | **No** |
| Memory embedding model/provider | `config.toml` `[memory]` | **No** |
| UI `theme` / `show_token_usage` | `config.toml` `[ui]` | **No** — dead schema; frontend ignores it |
| Pricing (`[[pricing]]`) | `endpoints.toml` | **No** — carried over on save only |
| `max_context` / `max_output_tokens` | `endpoints.toml` | **Round-trip in data, no UI fields** |
| Projects registry | `projects.toml` | **No** |

**Verdict:** the Endpoints tab is functional and relatively well-engineered (dirty tracking, validation, live model picker, provider re-sync). The **dialog shell and information architecture are not** — too narrow, incomplete, inconsistent save semantics, and split across three surfaces (Settings modal / StatusBar / Right panel). Appearance is a localStorage island disconnected from the backend `[ui]` section.

A redesign should treat Settings as the **single place to configure the product**, with a larger chrome, a left nav of sections, unified dirty/save behavior, and full coverage of the existing config schema.

---

## 2. Current architecture (what exists)

### 2.1 Frontend

```
Sidebar gear → ConfigDialog (Radix Dialog)
  ├── Tab: Appearance  (Zustand + localStorage; live apply)
  └── Tab: Endpoints   (load via getConfig + getApiKeys; save via saveEndpoints)
StatusBar
  ├── Safety mode dropdown + SafetyToggleDialog (Autonomous confirm)
  ├── Model picker (set_model IPC)
  └── Reasoning effort picker
Right panel → SafetyRules (raw safety.toml editor)
```

**Key files**

- `frontend/src/components/layout/ConfigDialog.tsx` — monolith: dialog shell + Appearance + EndpointsTab + EndpointCard + ModelPickerDropdown
- `frontend/src/hooks/useAgentStore.ts` — appearance prefs (`mh.*` localStorage keys), `configVersion` bump
- `frontend/src/lib/tauri.ts` — `getConfig`, `getApiKeys`, `listModels`, `saveEndpoints`, `setModel`, …
- `frontend/src/components/ui/dialog.tsx` / `tabs.tsx` — Radix wrappers (focus trap / ARIA present for dialog)

### 2.2 Backend config model

```
~/.myharness/
├── config.toml      # [general] [context] [memory] [ui]  (+ optional [general.vision_model])
├── endpoints.toml   # [[endpoint]] + [[pricing]]
├── keys.toml        # [endpoint-name] api_key = "…"
└── projects.toml    # [[project]] name/path
```

**IPC today**

| Command | Purpose | Secrets? |
|---|---|---|
| `get_config` | general defaults + endpoints + pricing | No |
| `get_api_keys` | endpoint → key map | **Yes — full plaintext keys** |
| `list_models` | live `/models` fetch for picker | Uses key server-side |
| `save_endpoints` | full replace of endpoints + keys + default provider/model; reloads config; rebuilds live provider | Writes keys |
| `set_model` / `set_safety_mode` | runtime switches | No |
| `get_safety_rules` / `save_safety_rules` | raw TOML | No keys |

There is **no** generic `save_general_config` / `save_ui` / `save_pricing` / `save_vision` command.

### 2.3 What already works well (do not regress)

1. **Secrets isolation:** `get_config` never includes keys; keys only via `get_api_keys`.
2. **Batch validation:** `save_endpoints` rejects the whole batch on any error (no partial endpoint list).
3. **Non-general sections preserved** on endpoint save (context/memory/ui/vision/pricing).
4. **Live provider re-sync** on save is now unconditional when a default endpoint exists (earlier name-only gate was fixed).
5. **Empty model ids stripped** on save; empty API keys treated as “no key”.
6. **Default-model leniency** for the pre-save running model (mirrors `set_model`).
7. **Collapsible endpoint cards**, stable UIDs, live model picker with race guards, dirty/Reset/Save bar.
8. **Radix Dialog** focus trap + Escape + `DialogTitle`.
9. **StatusBar `configVersion` re-sync** after save.

---

## 3. Redesign goal

Build a **Settings experience** that a power user can rely on as the single source of truth for:

1. How the app looks (appearance).
2. Which providers/models/keys it talks to (endpoints).
3. How the agent behaves (safety, context, memory, vision).
4. Cost tracking (pricing).
5. Which projects are known (optional advanced).

Match the quality bar of modern agent IDEs (Cursor / VS Code / Claude Desktop settings): **wide panel, left section nav, search, dirty indicator, explicit Save/Cancel or clear live-apply rules, inline validation, connection test.**

---

## 4. Detailed redesign instructions

### 4.1 Information architecture

Replace the two top tabs with a **settings shell**:

```
┌─────────────────────────────────────────────────────────────────┐
│  Settings                                          [Search]  ✕  │
├──────────────┬──────────────────────────────────────────────────┤
│ General      │  <section content scrolls here>                  │
│ Providers    │                                                  │
│ Appearance   │                                                  │
│ Safety       │                                                  │
│ Memory       │                                                  │
│ Vision       │                                                  │
│ Pricing      │                                                  │
│ Advanced     │                                                  │
├──────────────┴──────────────────────────────────────────────────┤
│  Unsaved changes ·  Reset section    [Cancel]  [Save changes]   │
└─────────────────────────────────────────────────────────────────┘
```

**Section map (required content)**

| Nav id | Title | Source of truth | Notes |
|---|---|---|---|
| `general` | General | `config.toml` `[general]` | Default provider, default model, links into Providers |
| `providers` | Providers | `endpoints.toml` + `keys.toml` | Current Endpoints tab, expanded |
| `appearance` | Appearance | Prefer `config.toml` `[ui]` + optional local overrides; today localStorage | Theme, font, colors, token-usage toggle |
| `safety` | Safety | runtime safety mode + `safety.toml` | Mode picker + rules editor (or deep-link to panel) |
| `memory` | Memory | `config.toml` `[memory]` | Embedding model + provider endpoint name |
| `vision` | Vision | `config.toml` `[general.vision_model]` | Optional endpoint + model for image-to-text |
| `pricing` | Pricing | `endpoints.toml` `[[pricing]]` | Per-model $/1M rates used by Stats |
| `advanced` | Advanced | `config.toml` `[context]`, paths | Summarize fill rate, open config dir, export/import |

**Do not** leave safety mode only on the StatusBar forever — StatusBar can keep a **quick switcher**, but Settings must own the full explanation + defaults.

### 4.2 Layout & chrome

| Rule | Detail |
|---|---|
| **Size** | Default `max-w-3xl` or `min(960px, 92vw)` × `min(720px, 90vh)`. Current `max-w-md` is too narrow for endpoint cards. |
| **Structure** | Left nav (~180px) + content. Use Radix Tabs oriented vertically, or a custom nav with `role="tablist"` `aria-orientation="vertical"`. |
| **Scroll** | Only the **content pane** scrolls. Header + footer + left nav stay fixed. (Current Appearance/Endpoints scroll fix must be preserved.) |
| **Search** | Filter left-nav items + in-section field labels (client-side). Optional v1. |
| **Footer** | Global Save / Cancel when any section is dirty. Or per-section Save if live-apply is kept for Appearance only — see §4.4. |
| **Close** | If dirty → confirm “Discard unsaved changes?”. Escape and overlay click respect the same rule. |
| **Theme tokens** | Dialog chrome must use CSS vars (`text-primary`, `border`, accent) — stop hardcoding `text-slate-*` / `bg-cyan-*` so light theme doesn’t look broken inside Settings. |

### 4.3 File / module structure (break the monolith)

Split `ConfigDialog.tsx` into:

```
frontend/src/components/settings/
  SettingsDialog.tsx          # shell: open/close, nav, dirty footer, load orchestration
  sections/
    GeneralSection.tsx
    ProvidersSection.tsx      # today’s EndpointsTab
    EndpointCard.tsx
    ModelPickerDropdown.tsx
    AppearanceSection.tsx
    SafetySection.tsx
    MemorySection.tsx
    VisionSection.tsx
    PricingSection.tsx
    AdvancedSection.tsx
  hooks/
    useSettingsDraft.ts       # single draft state + dirty + validate + save
    useEndpointModels.ts      # listModels cache / race guard
  types.ts                    # draft shapes mirroring backend DTOs
  validation.ts               # pure validators shared with tests
```

Keep `ConfigDialog` as a thin re-export or rename call sites in `Sidebar.tsx`.

**Target size:** no single settings file > ~400 lines.

### 4.4 Save / dirty model (critical product decision)

**Problem today:** Appearance applies instantly to localStorage; Endpoints require Save; closing the dialog with dirty endpoints **silently discards** in-memory edits; Done always closes.

**Recommended model (hybrid, explicit):**

1. **Draft on open.** When the dialog opens (`open` goes true), load a full settings draft from the backend (+ appearance from store).
2. **Edit the draft.** UI never writes disk or localStorage until Save (except optional live *preview* for colors — see below).
3. **Save writes everything dirty** in one IPC batch (or ordered multi-command with a single success path).
4. **Cancel / Reset** reloads draft from last-saved snapshot.
5. **Close with dirty** → confirm discard.

**Appearance live preview exception (optional but nice):**

- Color/font/theme changes can apply to CSS vars immediately for preview.
- On Cancel/discard, restore previous CSS vars + localStorage values.
- On Save, commit to localStorage **and** backend `[ui]` if wired.

**Do not** mix “live permanent” Appearance with “explicit Save” Endpoints without a discard warning — that is the current footgun.

### 4.5 Backend IPC to add / extend

Implement a coherent settings API. Suggested shape:

```rust
// Read (no secrets)
get_settings() -> SettingsDto {
  general: { default_provider, default_model, safety, vision_model },
  context: { summarize_at_fill_rate },
  memory: { embedding_model, embedding_provider },
  ui: { theme, show_token_usage },
  endpoints: Vec<EndpointDto>,   // no api_key
  pricing: Vec<PricingEntry>,
  projects: Vec<{ name, path }>, // optional
  config_dir: String,            // display path for Advanced
}

// Secrets still separate
get_api_keys() -> HashMap<String, String>

// Write (transactional as possible)
save_settings(SettingsSaveDto) -> SaveResult {
  // Validates all sections
  // Writes config.toml + endpoints.toml + keys.toml (+ pricing)
  // Reloads state.config
  // Re-syncs provider if needed
  // Optionally updates safety mode runtime
  // Returns { provider_swapped, warnings[] }
}
```

**Migration path:** keep `save_endpoints` working during the transition; have the new UI call `save_settings`. Deprecate the narrow command once the UI is cut over.

**Must-fix backend gaps**

1. `get_config` / `get_settings` must return **serde kind** (`"openai"` / `"local"`), not `format!("{:?}", kind)` (`"OpenAI"` / `"Local"`). Drop the frontend `kindFromConfig` hack.
2. Include `vision_model`, `context`, `memory`, `ui` in the read DTO.
3. Accept `pricing` edits (today silently preserved only).
4. Accept `max_context` / `max_output_tokens` from UI (fields already on `EndpointDto`).
5. Prefer **atomic multi-file write** (write temp files + rename) — `Config::save_all` is currently non-transactional across three files.
6. Consider **not** returning full API keys by default: return `has_key: bool` + allow “reveal/replace” to reduce shoulder-surfing risk. If full keys remain (for edit convenience), clear them from React state when the dialog closes.

### 4.6 Providers section (evolved Endpoints tab)

Keep the card model; improve it:

#### Always-visible summary row (collapsed)

- Name, kind badge, base URL (truncated), model count, Default badge, connection status dot (optional after test).

#### Expanded fields (required)

| Field | Control | Validation |
|---|---|---|
| Name | text | non-empty, unique, stable id caution on rename |
| Kind | select `openai` \| `local` | required |
| Base URL | text | non-empty, URL-ish; missing trailing `/` is auto-appended |
| API key | password + show/hide | optional; empty = env/dummy fallback |
| Reasoning effort | select: `default (max)` \| max/high/medium/low/minimal/off | **fix null/default vs off** (see finding F-CFG-03) |
| Multimodal | checkbox | |
| Max context | number optional | positive int |
| Max output tokens | number optional | positive int ≤ max context if both set |
| Models | list + picker | no blanks; unique within endpoint |
| Default endpoint | radio | one of |
| Default model | star / select when this endpoint is default | must resolve |

#### Actions

- **Add endpoint** (auto-expand, focus name).
- **Delete** (always confirm, even if unnamed).
- **Test connection** — call `list_models` (or a dedicated `ping_endpoint`) and show success/error inline.
- **Pick from server** — keep current live model picker; add arrow-key navigation like StatusBar.
- **Duplicate endpoint** (nice-to-have).

#### Defaults UX

- Default provider + default model should also appear at the top of Providers (or in General) as a summary, not only buried inside a card.
- When user sets default endpoint with no models → block Save with a clear message (already partially enforced server-side).

### 4.7 Appearance section

| Control | Behavior |
|---|---|
| Theme | Dark / Light / **System** (honor `prefers-color-scheme`) |
| Font family | curated select + optional custom text |
| Font size | **range slider 10–24** + numeric readout (number-only input is clunky) |
| Show token usage | toggle → wire to `ui.show_token_usage` and StatusBar/InflightBar |
| Colors | grouped: UI chrome vs code tokens; each row: swatch + validated hex |
| Presets | “Default dark”, “Default light”, “High contrast” one-click |
| Preview | keep live text + code sample; ensure light-theme code tokens work |
| Reset | section reset + global appearance reset |

**Persistence unification**

- Write theme to **both** localStorage (fast paint) and `config.toml` `[ui].theme` on Save so CLI/other surfaces can share it.
- On app mount: prefer localStorage if present, else backend `ui.theme`, else `"dark"`.
- Document the precedence in code comments.

**Hex validation:** reject non-`#RRGGBB` on commit; don’t apply invalid CSS.

### 4.8 Safety section

| Control | Notes |
|---|---|
| Safety mode radio/cards | Four modes with plain-language descriptions (not just kebab-case labels) |
| Autonomous warning | Reuse `SafetyToggleDialog` confirm before enabling |
| Safety rules | Embed `SafetyRules` editor **or** “Open Safety panel” button |
| Link to constitution | Read-only note that core ops always gate |

Saving mode should call `set_safety_mode` and persist `config.toml` `general.safety`.

### 4.9 Memory / Vision / Context / Pricing / Advanced

**Memory**

- `embedding_provider` — select from configured endpoint names.
- `embedding_model` — text or model picker against that endpoint.
- Help text: used for recall/consolidation; requires reachable embedding endpoint.

**Vision**

- Enable toggle → when on, require `endpoint` + `model`.
- Endpoint select from configured endpoints (prefer multimodal).
- Explain: used when main endpoint is not multimodal (`describe_image` path).

**Context (Advanced or own subsection)**

- `summarize_at_fill_rate` slider 0.2–0.9 (default 0.5).
- Explain: fraction of context window that triggers summarization.

**Pricing**

- Table: model, input $/1M, output $/1M, cached $/1M.
- Add/remove rows; validate ≥ 0.
- Suggest auto-fill when adding models from known providers (optional).

**Advanced**

- Show resolved config directory path (`global_config_dir()`).
- Buttons: “Reveal in file manager”, “Copy path”, “Reload from disk”.
- Export/import JSON or TOML bundle (optional phase 2).
- Projects list (phase 2): name + path registry.

### 4.10 Accessibility & keyboard

Settings is a power-user surface — keyboard must work:

1. Radix Dialog already traps focus — keep it; ensure custom left nav is in the tab order.
2. Section nav: arrow keys move between sections.
3. Model picker: ArrowUp/Down + Enter + Escape (StatusBar already has a pattern — reuse).
4. Every icon button needs `aria-label` (delete, show key, refresh, collapse).
5. Dirty state announced: `aria-live="polite"` on “Unsaved changes”.
6. Color inputs need associated labels (not only adjacent text).
7. Respect `prefers-reduced-motion` for expand/collapse animations.

### 4.11 Validation UX

- Inline field errors under the control (red text + `aria-invalid`).
- Section badge with error count on left nav.
- Save disabled while invalid **or** Save enabled but focuses first error (prefer disable + summary).
- Server errors from `save_settings` shown in a sticky error banner at top of content (keep current red box pattern).

### 4.12 Loading / empty / error states

| State | UI |
|---|---|
| Loading settings | Skeleton in content pane, nav disabled |
| Load failure | Error + Retry (already exists for endpoints) |
| No endpoints | Empty card + primary “Add your first provider” CTA + short help linking to docs/sample |
| Save success | Toast or green banner; auto-clear; optional auto-close only if user preference |
| Provider swapped | Explicit message (already present) |

### 4.13 Reload on open

`EndpointsTab` currently loads once on mount (`useEffect []`). Because `ConfigDialog` stays mounted in `Sidebar`, **reopening Settings may show a stale draft** if something else changed config (StatusBar model switch, external file edit).

**Required:** when `open` becomes `true`, re-fetch settings (or key off `open` in the draft hook). Reset dirty state from the fresh snapshot.

### 4.14 StatusBar co-existence

After redesign:

- StatusBar model/effort pickers remain for **session quick-switch** (`set_model`).
- StatusBar safety quick-switch remains, but Settings is canonical for explanation + defaults.
- Saving default provider/model in Settings must bump `configVersion` so StatusBar re-syncs (already wired).
- Fix StatusBar resync bug: `if (ep?.reasoning_effort)` skips updating when effort is null/`off` (finding F-CFG-08).

### 4.15 Tests required for the redesign

**Rust**

- `save_settings` round-trip for general/context/memory/ui/vision/pricing.
- Reject invalid base_url, duplicate names, bad default model.
- Provider rebuild on in-place default endpoint edit (regression for old C1).
- Atomicity / no partial write on mid-failure if implemented.

**Frontend (Vitest / component)**

- Draft dirty detection.
- Discard confirm on close.
- kind serde form (no Debug casing).
- reasoning_effort null vs off mapping.
- max_context fields serialize.
- Appearance discard restores previous CSS vars.

### 4.16 Implementation phases (recommended order)

| Phase | Deliverable | Exit criteria |
|---|---|---|
| **P0** | Shell resize + left nav + split files + dirty-close confirm + reload-on-open | No data loss on close; dialog usable at 960px |
| **P1** | Providers completeness: max_context/max_output UI, effort default fix, Test connection, delete always confirms | All endpoint fields editable |
| **P2** | `get_settings` / `save_settings` IPC; General + Memory + Vision + Context | Full schema coverage except pricing/projects |
| **P3** | Appearance unified with `[ui]`; show_token_usage; system theme; hex validation; light-theme code presets | No localStorage/backend drift |
| **P4** | Safety section; Pricing editor; Advanced path reveal | Settings is single control plane |
| **P5** | Search, presets, export/import, a11y pass, tests | Polish |

Do not mark the redesign “done” until P0–P2 land; P3–P5 are still in-scope for “much better.”

### 4.17 Explicit non-goals (for this redesign)

- Per-agent providers (product is intentionally global — PLAN.md).
- Cloud account login / OAuth providers.
- Rewriting the StatusBar model picker into Settings (keep both).
- Moving project-local `.coding/config.toml` overrides into v1 (note as future).

---

## 5. Findings to address

Severity key:

- **P0** — user data loss, security, or silent wrong runtime behavior  
- **P1** — correctness / major UX brokenness  
- **P2** — incomplete product surface / quality  
- **P3** — polish, nits, cleanup  

Status:

- **open** — still present in tree as of this review  
- **fixed-verify** — prior review claimed fixed; re-verify in redesign  
- **accepted-debt** — known, track but may defer  

---

### 5.1 Config dialog & settings (primary)

#### F-CFG-01 — P0 — Unsaved endpoint edits discarded without warning
- **Where:** `ConfigDialog.tsx` footer Done / Escape / overlay; `EndpointsTab` dirty state local only  
- **Issue:** User can edit endpoints, click Done or outside, and lose all in-memory changes. No `beforeunload`-style confirm inside the dialog.  
- **Fix:** Dirty gate on close (§4.4). Disable Done or rename to Cancel when dirty; prefer explicit Save/Cancel footer.

#### F-CFG-02 — P1 — Settings draft not reloaded when dialog reopens
- **Where:** `EndpointsTab` `useEffect(() => load(), [])`; `ConfigDialog` always mounted from `Sidebar`  
- **Issue:** Stale endpoints/keys after StatusBar model changes or external config edits.  
- **Fix:** Load when `open === true` (depend on `open` or lift load to shell).

#### F-CFG-03 — P1 — `reasoning_effort` null displayed as `"off"` (wrong semantics)
- **Where:** `EndpointCard` select `value={endpoint.reasoning_effort ?? "off"}`; backend `effective_reasoning_effort`: `None` → **`"max"`**, only explicit `"off"` omits field  
- **Issue:** UI teaches users that “unset” means off; saving after a no-op open can persist `null` visually as off while runtime still sends max — or writing off accidentally. Mapping is inverted vs backend default.  
- **Fix:** Select options: `Default (max)`, `max`, `high`, …, `off`. Map default ↔ `null`. Never coerce `null` → `"off"`.

#### F-CFG-04 — P1 — `max_context` / `max_output_tokens` not editable in UI
- **Where:** fields exist on `EndpointEditable` / `EndpointDto` / `get_config`; no inputs in `EndpointCard`  
- **Issue:** Users must hand-edit `endpoints.toml` for context caps that drive summarization and request budgets.  
- **Fix:** Optional number inputs in Providers expanded card (§4.6).

#### F-CFG-05 — P1 — Large config surface missing from Settings
- **Where:** `src/config/general.rs` vision/context/memory/ui; pricing in endpoints  
- **Issue:** Only Appearance + Endpoints exposed. Vision, memory embeddings, summarize rate, pricing, projects, backend UI theme all require hand-editing TOML.  
- **Fix:** Sections in §4.1 / phases P2–P4.

#### F-CFG-06 — P1 — Dual source of truth for theme/UI
- **Where:** frontend `mh.theme` localStorage vs `config.toml` `[ui].theme` / `show_token_usage`  
- **Issue:** Backend UI section is effectively dead. `show_token_usage` never read by frontend. Cross-device/config-file theme can’t work.  
- **Fix:** Unify on Save (§4.7); wire `show_token_usage` to InflightBar/StatusBar.

#### F-CFG-07 — P1 — Dialog too narrow / cramped for provider editor
- **Where:** `DialogContent` `max-w-md`  
- **Issue:** Endpoint cards with URL + models + keys need ~800–960px. Horizontal overflow / cramped controls.  
- **Fix:** §4.2 size rules.

#### F-CFG-08 — P1 — StatusBar resync ignores null/`off` reasoning effort
- **Where:** `StatusBar.tsx` `resyncFromBackend`: `if (ep?.reasoning_effort) setReasoningEffortStore(...)`  
- **Issue:** Falsy check skips update when effort is missing or when user wanted off after save. Store can desync.  
- **Fix:** Always set from endpoint effective value (including default max / off).

#### F-CFG-09 — P1 — Monolith component (~1330 lines)
- **Where:** `ConfigDialog.tsx`  
- **Issue:** Hard to test, review, and extend; Appearance + Providers + picker coupled.  
- **Fix:** Split per §4.3.

#### F-CFG-10 — P2 — Inconsistent save semantics (live Appearance vs Save Endpoints)
- **Where:** Appearance setters write localStorage immediately; Endpoints need Save  
- **Issue:** Users don’t know which tabs need Save; “Done” feels like commit for both.  
- **Fix:** Unified draft model §4.4; footer always honest about dirty state.

#### F-CFG-11 — P2 — Color hex free-text not validated
- **Where:** `ColorRow` text `onChange={onChange}` → `setAccentColor` etc.  
- **Issue:** Invalid strings applied to CSS vars → broken UI until reset.  
- **Fix:** Validate `#RRGGBB`; only commit valid values; show inline error.

#### F-CFG-12 — P2 — Empty-name endpoint delete skips confirm
- **Where:** `deleteEndpoint`: confirm only if `ep.name.trim()`  
- **Issue:** Accidental delete of a filled-but-unnamed new card.  
- **Fix:** Always confirm.

#### F-CFG-13 — P2 — `get_config` emits Debug kind strings
- **Where:** `commands.rs` `format!("{:?}", e.kind)` → `"OpenAI"`/`"Local"`  
- **Issue:** Requires frontend normalization; fragile if enum Debug changes.  
- **Fix:** Serialize with serde rename (`openai`/`local`).

#### F-CFG-14 — P2 — API keys held in React state for dialog lifetime
- **Where:** `getApiKeys` → `apiKeys` state; dialog may stay mounted  
- **Issue:** Full secrets in JS heap even when Settings closed (if tab state retained). Shoulder surfing if show-key left on.  
- **Fix:** Clear keys on close; consider `has_key` + replace-only; reset `showKey` on collapse/close.

#### F-CFG-15 — P2 — `Config::save_all` non-transactional across files
- **Where:** `src/config/mod.rs` sequential `fs::write`  
- **Issue:** Crash mid-save can desync endpoints vs keys vs general.  
- **Fix:** Write `*.tmp` + rename; or single backup directory snapshot before write.

#### F-CFG-16 — P2 — Pricing not editable; can drift from models
- **Where:** `save_endpoints` clones `current.pricing`  
- **Issue:** Stats cost estimates go stale when models added/removed.  
- **Fix:** Pricing section §4.9.

#### F-CFG-17 — P2 — No connection test affordance
- **Where:** model picker fetch is the only probe  
- **Issue:** Users can’t verify base_url+key without hunting the picker.  
- **Fix:** Explicit “Test connection” button.

#### F-CFG-18 — P2 — Model picker keyboard support weaker than StatusBar
- **Where:** `ModelPickerDropdown` click-only list  
- **Issue:** No ArrowUp/Down/Enter roving focus (StatusBar has `onDropdownKeyDown`).  
- **Fix:** Reuse StatusBar pattern.

#### F-CFG-19 — P2 — Font size only via number input
- **Where:** Appearance font size `<input type="number">`  
- **Issue:** Poor UX vs slider; easy to clear field mid-edit.  
- **Fix:** Range input + value label.

#### F-CFG-20 — P2 — No System theme / `prefers-color-scheme`
- **Where:** theme dark|light only  
- **Issue:** Ignores OS preference (also noted in UI usability review).  
- **Fix:** Add `system` theme.

#### F-CFG-21 — P2 — Light theme + Settings chrome mismatch
- **Where:** hardcoded `text-slate-200`, `cyan-*` in dialog  
- **Issue:** Appearance can switch to light while dialog internals stay dark-tuned.  
- **Fix:** Token-based classes throughout Settings.

#### F-CFG-22 — P2 — No frontend tests for Settings
- **Where:** only Rust `endpoint_dto` / config tests  
- **Issue:** regressions in dirty/close/effort mapping go unnoticed.  
- **Fix:** §4.15.

#### F-CFG-23 — P3 — Double `bumpConfigVersion` on save
- **Where:** `handleSave` calls `bumpConfigVersion()` and `onSaved()` which also bumps  
- **Issue:** Harmless double StatusBar resync.  
- **Fix:** Single bump path.

#### F-CFG-24 — P3 — `makeUid` uses `Math.random`
- **Where:** endpoint row keys  
- **Issue:** Fine for React keys; prefer `crypto.randomUUID` when available.  
- **Fix:** trivial.

#### F-CFG-25 — P3 — Appearance “Reset all” irreversible
- **Where:** `resetAppearance` writes localStorage immediately  
- **Issue:** No undo.  
- **Fix:** Under draft model, reset only draft until Save; or confirm.

#### F-CFG-26 — P3 — No deep-link to open a specific Settings section
- **Where:** only `open` boolean  
- **Issue:** Can’t “Open Settings → Providers” from empty-state CTAs.  
- **Fix:** `settingsSection: string | null` in store.

#### F-CFG-27 — fixed-verify — Live provider rebuild on in-place endpoint edit
- **Prior:** review C1 (name-only gate)  
- **Now:** `save_endpoints` docs + code claim unconditional rebuild  
- **Action:** keep a regression test; don’t reintroduce the gate.

#### F-CFG-28 — fixed-verify — default_model membership leniency / no-models default
- **Prior:** review C2/C3  
- **Now:** old_default_model allow + resolvable-model check present  
- **Action:** preserve in `save_settings`.

---

### 5.2 Broader product findings still worth addressing

These are **outside** the Settings redesign proper but should stay on the fix list. Sourced from this pass + existing `.coding/reviews/*` and `reviews/00-consolidated-report.md`, re-validated where possible.

#### Runtime / agent core

| ID | Sev | Finding | Notes |
|---|---|---|---|
| F-CORE-01 | P1 | Memory recall still O(N) full scan; FTS5 underused | Scale cliff as memory grows |
| F-CORE-02 | P1 | Working-tier memory not pruned after consolidation | Bloats recall |
| F-CORE-03 | P1 | Tool results may still lack hard truncation in some paths | Token blowups |
| F-CORE-04 | P2 | Constitution re-read-each-turn — verify still true after refactors | PLAN hard rule |
| F-CORE-05 | P2 | Interrupt during summarization may not be select!-aware | Cancel stuck |
| F-CORE-06 | P2 | Multi-agent: provider/safety/config global by design — document in UI | Avoid user confusion |
| F-CORE-07 | P3 | Dead / vestigial paths in provider or app stubs | Cleanup when touched |

#### Frontend / UX (non-settings)

| ID | Sev | Finding | Notes |
|---|---|---|---|
| F-UI-01 | P1 | First-run empty conversation has no onboarding | Discoverability |
| F-UI-02 | P1 | Slash commands lack autocomplete menu | Only `/help` |
| F-UI-03 | P1 | Plan panel historically hard to discover (verify autoRevealPlan coverage) | Plan-first workflow |
| F-UI-04 | P2 | Light theme code highlighting incomplete without proper `--code-*` defaults | Related to Appearance |
| F-UI-05 | P2 | Accessibility still uneven outside Radix dialog/tabs | Agent tabs, many icon buttons |
| F-UI-06 | P2 | No `prefers-reduced-motion` | Animations always on |
| F-UI-07 | P2 | Approval button order escalates left→right (Allow-for-project first) | Easy mis-click |
| F-UI-08 | P2 | Retry-in-progress under-communicated in transcript | User sees endless “thinking” |
| F-UI-09 | P3 | `/save` `/load` path-only, no file picker | Friction |
| F-UI-10 | P3 | DenyAll exists in Rust approval enum but not in ApprovalPrompt UI | Feature gap |

#### IPC / Tauri

| ID | Sev | Finding | Notes |
|---|---|---|---|
| F-IPC-01 | P2 | No generic settings save — only `save_endpoints` | Blocks full UI |
| F-IPC-02 | P2 | Startup still falls back to dummy provider when no endpoint | OK, but Settings empty-state must teach setup |
| F-IPC-03 | P3 | `get_config` hand-built JSON vs typed DTO | Drift risk |

#### Safety / constitution

| ID | Sev | Finding | Notes |
|---|---|---|---|
| F-SAFE-01 | P1 | Core ops (`merge`/`push`) must remain `never_auto_for` regardless of mode | Do not regress |
| F-SAFE-02 | P2 | Bookkeeping tools AutoRun — document in Safety section help | User education |
| F-SAFE-03 | P3 | `AutoReadApproveWrites` distinction weak if few tools use it | Product clarity |

#### Testing / quality

| ID | Sev | Finding | Notes |
|---|---|---|---|
| F-TEST-01 | P1 | No component tests for ConfigDialog/Settings | Add with redesign |
| F-TEST-02 | P2 | Critical paths (summarize, SSE parse, consolidate) historically under-tested | Keep investing |
| F-TEST-03 | P3 | Frontend test suite thin vs Rust | Balance |

---

## 6. Suggested acceptance checklist (Settings redesign)

- [ ] Settings opens at ≥900px usable width; left nav + scrollable content + fixed footer  
- [ ] Closing with dirty Providers (or any dirty section) prompts confirm  
- [ ] Reopening Settings reloads from backend  
- [ ] All `Endpoint` fields editable including max_context / max_output_tokens  
- [ ] reasoning_effort default vs off correct end-to-end  
- [ ] Vision / memory / context / ui.show_token_usage configurable  
- [ ] Save writes disk, reloads `state.config`, re-syncs provider, bumps StatusBar  
- [ ] API keys never logged; cleared from UI state on close  
- [ ] Light + dark Settings chrome readable  
- [ ] Keyboard: section nav, model picker, Escape-with-dirty  
- [ ] `cargo test` green; new settings unit tests green  
- [ ] Reviewer pass on the redesign PR with this file as the spec  

---

## 7. Reference map (quick links)

| Concern | Path |
|---|---|
| Settings UI (current) | `frontend/src/components/layout/ConfigDialog.tsx` |
| Appearance store | `frontend/src/hooks/useAgentStore.ts` |
| IPC wrappers | `frontend/src/lib/tauri.ts` |
| IPC commands | `src-tauri/src/ipc/commands.rs` (`get_config`, `get_api_keys`, `list_models`, `save_endpoints`, `set_model`) |
| Config load/save | `src/config/mod.rs`, `general.rs`, `endpoints.rs`, `keys.rs`, `projects.rs` |
| Sample config | `debug.config.toml` |
| Prior endpoints review | `.coding/reviews/2026-04-04-config-endpoints-tab-review.md` |
| Prior model picker review | `.coding/reviews/2026-04-04-endpoints-model-picker-review.md` |
| UI usability review | `.coding/reviews/2026-ui-usability-review.md` |
| Consolidated older findings | `reviews/00-consolidated-report.md` |
| Product architecture | `PLAN.md` |

---

## 8. One-line charter

> **Replace the narrow two-tab Settings modal with a full settings shell that edits every field the backend already understands, never discards dirty state silently, and becomes the single control plane for providers, appearance, safety, memory, vision, and pricing.**

---

## 9. Implementation status (in progress)

Started on branch `feat/bookkeeping-tools-autorun` after this spec was written.

### Landed (P0–P5 core)

| Item | Status | Location |
|---|---|---|
| Wide shell + left nav + search | Done | `SettingsDialog.tsx` |
| Split monolith | Done | `frontend/src/components/settings/**` |
| Dirty-close + reload-on-open + clear keys | Done | Providers + shell |
| Providers: effort / max_context / test / confirm delete | Done | `EndpointCard` / `ProvidersSection` |
| `get_settings` + `save_settings` IPC | Done | `commands.rs` + `tauri.ts` |
| Memory / Vision / Safety / Pricing / Advanced sections | Done | `sections/*Section.tsx` |
| System theme + show_token_usage + light preset | Done | store + Appearance + InflightBar |
| Theme/token usage persist to config.toml `[ui]` | Done | Appearance → `saveSettings` |
| Safety rules editor in Settings | Done | `SafetySection` |
| Context fill-rate + config path + projects list | Done | `AdvancedSection` |
| Settings DTO unit tests | Done | `settings_dto_tests` in commands.rs |

### Still open / follow-ups

- Broader product debt in §5.2 (memory O(N), onboarding, slash autocomplete, …)

### P2/P3 + transactional save + live rewire

| Item | Status |
|---|---|
| Appearance draft + discard | Done |
| Atomic temp+rename + `.bak` rollback on mid-rename failure | Done |
| Vitest unit tests (`types.test.ts`) | Done |
| `get_config` serde kind | Done |
| `openSettings` / `settingsSection` deep-link | Done |
| Export/import JSON bundle | Done |
| StatusBar safety persists to config.toml | Done |
| `SwappableVision` live rewire (Settings + endpoints save) | Done |
| `MemoryStore::set_embedder` + config-driven Ollama/hash | Done |

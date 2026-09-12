# Review: Live vision-capable model picker + remove datalist white bubble

**Scope:** All uncommitted changes (`git diff HEAD`) across 8 files.
**Verdict:** Clean. No correctness, bug, security, or constitution findings that
require a fix. A few minor observations (non-blocking) are noted at the end.

---

## Correctness

### `fetch_models` delegation is byte-identical ✅
`fetch_models` (src/provider/openai.rs:147) now delegates to
`fetch_models_with_vision` and discards the `vision_capable` flag via
`.map(|v| v.into_iter().map(|m| m.id).collect())`. The de-dup/sort path in
`fetch_models_with_vision` (lines 220-236) uses a `BTreeMap<String, bool>`,
which sorts by id exactly as the historical `BTreeSet<String>` did, and drops
blank/whitespace-only ids after trimming (lines 223-226) — identical to the old
loop. The 6 existing `parse_model_ids` tests (lines 1680-1736) exercise the
delegated `parse_model_ids` → `parse_models_with_vision` path and assert the
same `Some`/`None`/empty-list contract. Confirmed equivalent.

### Vision filtering falls back to all models ✅
`parse_models_with_vision` sets `vision_capable: false` for every entry when the
provider exposes no modality (vanilla OpenAI/Ollama) — verified by
`parse_models_with_vision_vanilla_openai_has_no_modality` and
`parse_models_with_vision_ollama_shape` tests. In VisionSection.tsx:134-136,
`modalityKnown = capable.length > 0` and `pickerModels` falls back to the full
`visionModels` list when no model is flagged capable. So an unknown-modality
provider shows all models (with the "Modality unknown" note at line 292), never
an empty list. Correct.

### `list_vision_models` key resolution matches `list_models` exactly ✅
The resolution block (src-tauri/src/ipc/settings.rs:254-270) is character-for-
character identical to `list_models` (lines 204-220): override `base_url`/`api_key`
→ saved endpoint → `OPENAI_API_KEY` → `ANTHROPIC_AUTH_TOKEN` → `"dummy"`, with the
same `endpoint_name` error string. Registered in main.rs:231. Correct.

### Datalist removal doesn't break free-text entry ✅
EndpointCard.tsx:506 — only the `list="all-models-datalist"` attribute was removed;
the `<input>` remains a plain text input with its `onChange`/`onKeyDown` handlers
intact, and the custom `ModelPickerDropdown` (opened on focus) is additive.
ProvidersSection.tsx removed the now-unused `allModelNames` useMemo and the
`<datalist>` block — no dangling references (the `list` attribute that referenced
it is gone). Free-text entry still works; the custom dropdown still provides
suggestions. Correct.

---

## Security

### No API key leak in error messages ✅
`fetch_models_with_vision` (src/provider/openai.rs:186-203) preserves the 401/403
body-suppression path verbatim: for auth-failure statuses it returns
`"HTTP {status} from {url} — unauthorized (check the API key)"` without reading
the response body, so a malicious server echoing the `Authorization: Bearer {key}`
header cannot surface the key in the UI. Non-auth errors truncate the body to 500
chars via `truncate_for_display`. The IPC layer maps errors via `.map_err(|e|
e.to_string())` (settings.rs:277) — no key material is added. Correct.

---

## Constitution compliance

### Doc comments on all new public functions/structs ✅
- `ModelWithVision` struct + both fields documented (openai.rs:115-130).
- `fetch_models_with_vision` documented (openai.rs:153-162).
- `list_vision_models` command documented (settings.rs:230-243).
- `VisionModelInfo` interface + `listVisionModels` documented (tauri.ts:198-208,
  361-373).
- Private helpers `model_supports_vision` and `parse_models_with_vision` also
  carry doc comments (good practice, though not constitutionally required for
  private items).

### No commits to main ✅
No `git commit`/`git merge`/`git push` in this diff — it's a working-tree change
set only. The `.coding/plans/stack.json` change is bookkeeping (plan id +
`reviewed` flag), not a commit to main.

### Line-ending style preserved ✅
The diff shows no mixed `\r\n`/`\n` introductions; the file tools normalize to
the detected style. No CRLF artifacts in the diff.

### Tests ✅
4 new unit tests added (openai.rs:1756-1830) covering the OpenRouter shape,
vanilla-OpenAI no-modality, Ollama shape, and `model_supports_vision` edge cases.
The existing 6 `parse_model_ids` tests + settings DTO tests are preserved and
exercise the delegation path.

---

## Minor observations (non-blocking, no fix required)

1. **`useImperativeHandle` has no deps array** (VisionSection.tsx:142). This means
   the `save` closure is recreated every render — but since it reads `enabled`,
   `endpoint`, `model` from the latest render scope, this is actually *correct*
   behavior (it always saves the current values). This is the pre-existing pattern
   (unchanged by this diff), and adding a deps array would risk a stale closure.
   No action needed.

2. **Outside-click effect deps** (VisionSection.tsx:132) include `[pickerOpen,
   pickerQuery, model]`. The handler calls `closePicker(true)`, which reads
   `pickerQuery` and `model` — both are in the deps, so the effect re-subscribes
   with fresh values whenever they change. No stale-closure risk. This mirrors
   EndpointCard's `[pickerOpen, pickerQuery]` pattern (EndpointCard.tsx:165). The
   extra `model` dep is correct here because `closePicker` compares against `model`
   (VisionSection.tsx:115). Correct.

3. **Race guard** (VisionSection.tsx:85, 89, 94): `fetchReqId` ref increments per
   fetch and is checked after `await` — stale responses are discarded. Matches
   EndpointCard's pattern. Correct.

4. **`host` const removal**: The old `const host = endpoints.find(...)` (used only
   for the static `<select>`) was removed along with the select. Nothing dangles —
   `fetchVisionModels` does its own `endpoints.find` (line 80). Clean.

---

**Conclusion: No findings requiring fixes.** The delegation preserves byte-
identical output, vision filtering falls back correctly, no key leaks, key
resolution matches `list_models`, the datalist removal is safe, and all new
public items carry doc comments. Recommend proceeding to commit.

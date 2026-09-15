## Verdict: FINDINGS (4 high, 5 low)

Scope: all uncommitted changes on `wt/mnemo` (`git diff HEAD`, 20 paths / 1116 insertions). Severity convention: the verdict format only carries high/low; the 5 non-high findings are split below into **medium** (correctness/doc accuracy) and **low**, each labeled.

Design invariants 1, 2, 5 and 6 hold (see "Verified correct" at the end): the filter is display-layer only, the default render is byte-identical to before, the 11 keys are consistent across the whole stack, and the legacy `show_delegation_notes` still parses/serializes/seeds.

The feature's own acceptance criterion — *"each marker kind has a Chat-section checkbox; unchecked → that line is stripped from the ToolCard render only"* — fails for **6 of the 11 kinds** (H1–H4, M1). The registry's matchers were written against invented line shapes rather than the real emission sites, and the test file's fixtures (which claim to be "the real emission shapes") encode the same invention, so the suite pins the fiction instead of the contract.

---

### H1 (high) — `edit_stale_read`'s prefix never matches the real line: the toggle silently does nothing

- Registry: `frontend/src/lib/delegationNotes.ts`, the `edit_stale_read` entry — `linePrefixes: ["Re-read the file"]`, matched at line start after an optional `note: `/`NOTE: ` unwrap (`unwrapNote` / `matchesDef`).
- Real emission: `src/tool/agent/file_edit.rs:169-175` (`with_fresh_read_nudge`) formats
  `"{msg} — the content has likely drifted from your last read. Re-read the file (read_files, this exact path) and retry with the exact current text; do not edit without a fresh read."`
  → the line **starts** with the drift message (`old_string not found …`, line-range past EOF, empty file); `Re-read the file` is mid-line. The backend documents it as a *substring* marker, not a prefix: `src/tool/agent/file_edit.rs:164` and `src/agent/steering_stats.rs:264-269` (`first_line().contains(self.marker())`).
- The line is user-visible: a failed `file_edit` renders through the filtered fallback `<pre>` (`frontend/src/components/chat/Message.tsx:809-812`) because `fileEditDiff` returns `null` for `!success` (`frontend/src/lib/toolCardPaths.ts:741`) and there are no read sections — so the note displays and the checkbox cannot hide it.
- **Fix:** add the real shape, e.g. `linePattern: /^(?:.*— the content has likely drifted from your last read\.\s*)?Re-read the file/` is too loose; better: a `lineIncludes` field scoped to *first-line* lines that must contain the marker alongside the drift phrase, or mirror the backend exactly — `linePattern: /— the content has likely drifted from your last read\. Re-read the file/`. Add a regression test using the verbatim `file_edit` error string.

### H2 (high) — `graph_miss`'s prefix never matches: the hint is a JSON field value

- Registry: `linePrefixes: ["No symbols matched"]`.
- Real emission: `src/tool/agent/codegraph.rs:215-226` (`miss_hint`) → `"No symbols matched '{query}'. The graph indexes symbol definitions only — …"`, placed in the payload's `hint` field and serialized with `serde_json::to_string_pretty` (`codegraph.rs:381-383`). The rendered line is `  "hint": "No symbols matched 'zzz'. …"` → it does not start with the prefix. The backend says so explicitly: `src/agent/steering_stats.rs:216-218` ("these outputs are structured pretty-JSON") and detects it with a full-output `contains` (`:242-247`), *because* it is not line-leading.
- Also uncovered: the other `miss_hint` arm emits a different string entirely — `"no exact symbol; try one of the candidates' ids"` (`codegraph.rs:224`) — so the kind is only partially modeled even in shape terms.
- **Fix:** match the JSON field form (`linePattern: /^\s*"hint":\s*"No symbols matched/`) — or, since graph output is a single blob the model reads, drop this kind from the registry and document that graph misses are not chat-filterable.

### H3 (high) — `search_nudge` hides the *other* kind's line and never hides its own

- Registry: `key: "search_nudge"`, `linePrefixes: ["AUTO-DELEGATED"]`, label "Search symbol nudge", description "The 'is an indexed symbol' header that steers a symbol-shaped search to the graph tools."
- Real advisory nudge: `src/tool/agent/search.rs:675-686` emits ``'{pattern}' is an indexed symbol — graph_context(id="…") gives its definition + callers in one call; …`` or `the symbol '{name}' (from pattern '{pattern}') is an indexed symbol — …`, wrapped by `with_note` as `note: …` (`search.rs:998-1001`). After the `note: ` unwrap the line begins with `'hello' is an indexed symbol` → the `AUTO-DELEGATED` prefix does not match → unchecking "Search symbol nudge" **does not hide the nudge**.
- What it does hide is the AUTO-DELEGATED delegation header — a line `auto_delegated` already covers. The header happens to contain `is an indexed symbol` (`AUTO-DELEGATED to the code graph — 'X' is an indexed symbol …`), which is why the two were conflated; but the backend treats them as two emissions of one kind: `src/agent/steering_stats.rs:202-210` ("the auto-delegated answer block … carries the marker on its first line too … The escape repeat's advisory note counts as before") with `detect` = `first_line().contains(SEARCH_NUDGE_MARK)` where `SEARCH_NUDGE_MARK = "is an indexed symbol"` (`:231-234`, `:279`).
- Net effect: the checkbox is a **no-op for its own note** and a **mislabeled duplicate of `auto_delegated`**; the description names a line the toggle cannot control.
- **Fix:** cover both shapes — keep `["AUTO-DELEGATED"]` (documented union) **and** add a pattern for the advisory form, e.g. `linePattern: /^'[^']*' is an indexed symbol/` plus `/^the symbol '[^']*' \(from pattern '[^']*'\) is an indexed symbol/`; if the union with `auto_delegated` is still wanted, keep it documented as such.

### H4 (high) — `shell_tip` and `shell_redirect` toggles are inert: the shell card bypasses the filter

- Emission: `src/tool/agent/shell.rs:359-373` prepends `GREP_NUDGE` (`:41`) / `REDIRECT_NOTE` (`:50`) into `combined`, which becomes the result `output`.
- Render: `frontend/src/components/chat/Message.tsx:712` calls `parseShellOutput(call.result.output)`; `toolCardPaths.ts:699-723` keeps those lines in `stdout` (only the `[exit code: N]` trailer and the `[stderr]` split are handled); `Message.tsx:793-808` then renders `shellOut.stdout` **raw** — `stripSteeringNotes` is only applied in the generic fallback `<pre>` (`:809-812`), which is unreachable for `shell` calls (`shellArgs`/`shellOut` are non-null).
- So unchecking either shell checkbox cannot hide its line; both lines remain displayed exactly as before the feature. This is the largest acceptance-criterion gap: these are the two kinds that were *always* visible and are now advertised as controllable.
- **Fix:** route the shell stdout/stderr render through `stripSteeringNotes(shellOut.stdout, hiddenSteeringNotes)` (and stderr likewise), with a source-contract pin in `delegationNotes.test.ts` mirroring the existing ones.

---

### M1 (medium) — co-occurring notes share ONE line: `literal_tip` over-hides `known_memory_hit`, whose own toggle is a no-op

- `merged_note` joins components with `"; "` (`src/tool/agent/search.rs:1026-1048`) and `with_note` prepends the result once as `note: …` (`:998-1001`) → one line.
- The backend pins the exact co-occurrence (`src/agent/steering_stats.rs:1068-1084`):
  `"note: TIP: pattern has no regex metacharacters — … (one indexed lookup instead of a tree walk); known memory hit: 'PLAN: backlog 70f5b248' — this id is a known backlog item; memory_search it for detail"`.
- Consequences: unchecking `literal_tip` drops the whole line, taking the known-memory-hit text with it (violates "individually controllable"); unchecking `known_memory_hit` alone does nothing (the line starts with `TIP: pattern…`). The registry's overlap note (`delegationNotes.ts` module header) documents only the `auto_delegated`/`search_nudge` pair — this second, real sharing is undocumented, and the two toggles are asymmetric.
- **Fix:** decide per component: after unwrapping, split the line on `"; "` and drop only the hidden components (re-join the visible remainder as the rendered line), or at minimum document the pairing and make `known_memory_hit`'s matcher component-aware. Add a test using the verbatim co-occurring string above.

### M2 (medium) — the per-kind tests are tautologies for exactly the kinds that are broken

- `frontend/src/lib/delegationNotes.test.ts` declares its `LINES` map "One representative output LINE per kind — the real emission shapes", but three entries match no backend emission:
  - `search_nudge: "AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol (…)"` — the delegation header, not the advisory nudge (`search.rs:675-686`).
  - `graph_miss: "No symbols matched for 'zzz' — try a text search instead"` — real text is `No symbols matched 'zzz'. The graph indexes symbol definitions only — …` inside the JSON `"hint"` field (`codegraph.rs:215-226`).
  - `edit_stale_read: "Re-read the file before editing it again — the content changed since your last read"` — no such string exists; the real one is the mid-line tail of `file_edit.rs:169-175`.
- Because the `it.each` cases feed the fabricated lines, they pass while production shapes fail — the suite cannot detect H1–H3, i.e. it pins the fixture, not the contract. The Rust side already treats these strings as a contract (`file_edit.rs:164-168`, `steering_stats.rs:110-112`, `:274-286`).
- **Fix:** copy the verbatim literals from the emission sites into `LINES` (cross-root prose mirror, same discipline as `windowRestore`), and add one assertion per kind that the *real* shape is dropped by its own toggle. For `graph_miss`, land H2's decision first.

### M3 (medium) — `docs/CONFIGURATION.md` and `docs/FEATURES.md` overstate what the toggles do

- `docs/CONFIGURATION.md` (the new `[ui.steering_notes]` paragraph) says the notes "are individually controllable" and `docs/FEATURES.md:42` says "every steering note is individually showable/hideable from **Settings → Chat**", while H1–H4 + M1 show 6 of 11 kinds are not controllable in the shipped code. The other claims in that paragraph are accurate and well written (display-layer only, first-line scoped, `auto_delegated` inheriting `show_delegation_notes`, the `search_nudge` union).
- **Fix:** after H1–H4 land, the text is accurate as written — so treat this as a *gate*: either fix the matchers, or list the kinds that are display-filterable. The reviewer's documentation-sync duty (agent.md) makes an inaccurate user-facing claim a finding on its own.

### L1 (low) — `read_nudge` is inert in the display path

read_files results render as the parsed compact section list whenever headers match (`Message.tsx:718-721`, `:768-792`), and the `SYMBOL NUDGE:` line rides *outside* those headers (`toolCardPaths.ts:762-765`) → it is never rendered in the card, so the checkbox can change nothing. Pre-existing display behavior, not a regression, but the Chat label/description promise a visible line. Consider dropping the kind (its prefix *is* correct, so it would work if the line were ever rendered) or documenting it.

### L2 (low) — the legacy wrappers are now test-only, but their docs claim live callers

`isDelegationNote` / `stripDelegationNotes` (`frontend/src/lib/delegationNotes.ts:280-296`) say "Kept for the original single-toggle callers/tests" — after this change no production caller remains (`Message.tsx` uses `isSteeringNoteHidden`/`stripSteeringNotes`; `ChatSection.tsx:88-95` uses the store setter). Verified by repo search: only `delegationNotes.test.ts` references them. TS has no unused-export warning, so neither `tsc` nor the Rust `#![deny(warnings)]` gate catches it. Either delete both or reword the comment to "test-only compatibility shims".

---

## Verified correct (no findings)

- **Invariant 1 — display-layer only.** No backend suppression anywhere in the diff; `stripSteeringNotes` runs only at the two render sites (`Message.tsx:510`, `:811`), and the IPC carries display flags only. The tool-result text the model reads is untouched — the prior DECISION 950fdba5 contract holds.
- **Invariant 2 — no default regression.** `DEFAULT_HIDDEN_STEERING_NOTES = ["auto_delegated"]` (registry `defaultVisible`), the store seed is a copy of it (`useAgentStore.ts` `hiddenSteeringNotes`), `App.tsx:338-345` hydrates via `hiddenKeysFromConfig` (absent/null config → the same default), and `stripDelegationNotes` now delegates to `stripSteeringNotes(text, ["auto_delegated"])` — so the default render is equivalent to the old prefix list `["note: AUTO-DELEGATED", "AUTO-DELEGATED"]`. `effective_steering_note_visible` (`general.rs:578-581+`) resolves unknown keys to *visible* (fail-safe) and seeds `auto_delegated` from `show_delegation_notes`.
- **Invariant 3 — match precision (for the kinds that match at all).** The `note: `/`NOTE: ` unwrap + line-start anchoring is the right discipline; the quoted-note guard test (`src/docs.md: the TIP: …`) and the mid-line-mention tests are real pins, and they mirror the backend's first-line scoping. No bare-substring matching was introduced.
- **Invariant 5 — contract consistency.** `UiConfig::STEERING_NOTE_KEYS` (`general.rs:552-564`) ≡ the 11 registry keys (`delegationNotes.ts`) ≡ `SteeringNotesCfg` fields ≡ the fixture keys (`ipc-fixtures/dto-get-settings.json:60-72`, asserted exactly by `ipc-contract.test.ts:438-450`) ≡ `tauri.ts` (`SteeringNotes` resolved `:471`, the save patch `:540`) ≡ `ChatSection` draft/save (all 11 sent). Serialization hygiene is right: per-field `skip_serializing_if = "Option::is_none"` (`general.rs:375` etc.), the table-level `skip_serializing_if = "SteeringNotesCfg::is_empty"` (`:493`) with `is_empty()` = "all `None`" (`:419-425`), and `general.rs` pins both the round-trip and the key/field correspondence (`:1311-1346`).
- **Invariant 6 — legacy key.** `show_delegation_notes` still parses, round-trips (`general.rs:1246-1258`) and appears in the GET DTO/fixture (`:59`); the Chat section no longer writes it, and nothing depends on it being written (the per-kind key wins, `:581`).
- **Invariant 7 — test-file registration.** No new test file was added (only `delegationNotes.test.ts`, `ChatSection.test.ts`, `ipc-contract.test.ts` modified), so no `frontend/vitest.config.ts` allowlist entry was needed; the config is untouched in the diff. Correct.
- **Rust-side regression pins are real, not tautological.** The default/explicit/legacy-seed tests (`general.rs:1261-1295`), the "every key addresses a real field + every field round-trips" pin (`:1311-1346`), and the co-occurrence test (`steering_stats.rs:1068-1084`) would fail on a behavior regression. The `contract_fixtures.rs` literal is asserted against the shipped fixture, so the Rust↔TS contract is doubly pinned.
- **Constitution.** No Windows-only API/path/shell in library or app code; no shell-based file mutation in the diff; no `#[allow]`; new public Rust items carry doc comments (`SteeringNotesCfg` + every field, `STEERING_NOTE_KEYS`, `effective_steering_note_visible`, `is_empty`); the diff is hunk-based with no whole-file rewrites, so line-ending style is preserved.

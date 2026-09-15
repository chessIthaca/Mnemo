## Verdict: FINDINGS (0 high, 1 low)

Round-2 closing review of plan `3ae367bd` ("Per-note toggles for steering notes in the Chat config") — the FULL uncommitted diff (`git_read(op="diff")`: 20 paths / +1397 -180), not just the round-1 fixes. Round 1: `.coding/reviews/2026-09-15-steering-note-toggles-review.md` (0 high / 4 low); all four are addressed.

The single low finding is a residual qualification gap in the reworded "matching" sentence (round-1 LOW 4): ten of the eleven matchers keep the documented promise, the eleventh (`edit_stale_read`) does not, and the docs still say "never". Everything else verified sound: the other three round-1 fixes are correct, all four render paths are filtered, the splitter still re-joins byte-identically, the new alternation prefix causes no over-match or mis-split, and the display-only contract holds (no emission code is touched anywhere in the diff).

Method: read-only inspection; the suites were not re-run (I cannot). Every claim below is hand-derived from source, and the alternation shape was additionally checked against a LIVE emission produced by the tool during this review.


### 1. Round-1 LOW 1 (alternation-nudge shape) — FIXED, correct

- `frontend/src/lib/delegationNotes.ts:139` — `linePrefixes: ["AUTO-DELEGATED", "from the alternation pattern"]`; the comment at :128-138 now says "Four real shapes" and names the UNQUOTED `name == branch` entry.
- Ground truth verified twice. (a) Source: `src/tool/agent/search.rs:597-627` — `alternation_nudge` emits `from the alternation pattern '{pattern}': {entries joined with "; "}; prefer the graph tools for symbol hunts (search is for text)`, the unquoted entry exactly `{name} is an indexed symbol — graph_context(id="{id}")` (:608-609) and the quoted form `'{branch}' resolves to '{name}', an indexed symbol — …` (:610-614). (b) Live: a real `search` call during this review (pattern `steering_notes|steering`) returned a note whose leading phrase and per-entry shape match the tested strings structurally byte-for-byte; only the ids differ.
- New tests are right: the verbatim quoted + unquoted cases (`delegationNotes.test.ts:155-165`) and the no-mis-split pin (:167). I re-derived the splitter by hand against the real string: the per-entry `; <entry>` tails are NOT recognized component starts (`matchesDef` needs the leading quoted name or the `the symbol '…'` shape, and an unquoted entry reads `Strip is an indexed symbol — …`), so no mis-split occurs; hiding only `search_nudge` returns the line unchanged, re-joined at the recorded offsets with `"; "` so the remainder is byte-identical.
- No over-match: the new prefix is matched against the note-unwrapped text at position 0, so a `path:line:`-prefixed result line cannot hit it (the whole-line pattern pass is unaffected — the alternation shape carries no `linePatterns` entry).

### 2. Round-1 LOW 2 (unfiltered error summary) — FIXED, correct

- `frontend/src/components/chat/Message.tsx:456-462` computes `errorSummary` as `failed && lastCall.result ? (filterSteeringNote(toolErrorSummary(lastCall.result.output), hiddenSteeringNotes) ?? "") : ""`; the JSX at :662-664 renders on `errorSummary !== ""`.
- `null` (a hidden kind consumed the line) and `""` (empty/whitespace-only output) both collapse to `""`, so nothing renders — the old `toolErrorSummary(...) !== ""` render condition is subsumed exactly. A genuine non-note error has no matching matcher (whole-line pass at `delegationNotes.ts:321`, component split at :324-337) and passes through verbatim; a non-note remainder of a merged line also survives, since components are only dropped when they themselves match.
- Surrounding JSX/conditional structure is undisturbed: the previous expression was evaluated twice per render, the new form precomputes it once — same branches, same classes.
- Truncation cannot defeat the filter: `toolErrorSummary` caps at `ERROR_SUMMARY_MAX = 120` (`toolCardPaths.ts:591, 613-614`), and the drift line's marker `Re-read the file` completes at character 99 (I counted the emitted string), so the pattern still sees it.
- Behavior note (intended, not a defect): with `edit_stale_read` hidden the whole error line is gone — the note IS the result's first line — which is precisely the consistency round 1 asked for with the expanded `<pre>`.
- The added source-contract assertions (`delegationNotes.test.ts:376-385`) mirror the shipped strings, including `{errorSummary !== "" && (`.

### 3. Round-1 LOW 3 (legacy toggle dropped on an absent table) — FIXED, agrees with the backend

- `frontend/src/lib/delegationNotes.ts:398-406`: an absent/null `cfg` returns `legacyShowDelegationNotes ? [] : [...DEFAULT_HIDDEN_STEERING_NOTES]`; a present table returns only the kinds explicitly `false`, in registry order. `frontend/src/App.tsx:344-352` passes BOTH `settings.ui.steering_notes` and `settings.ui.show_delegation_notes`.
- Backend cross-check: `UiConfig::effective_steering_note_visible` (`src/config/general.rs:578-594`) — an explicit override wins; `None` follows `show_delegation_notes` for `auto_delegated` (default false) and defaults the other ten to visible. The frontend semantics match for every payload the shipped DTO can carry: `GetSettingsSteeringNotes` (`src-tauri/src/ipc/settings.rs:632-655`) is 11 non-optional bools with no skip attribute, so the table is either fully present or absent.
- No hydration path hides everything or prevents hiding: absent + legacy true gives `[]` (nothing hidden, the user's old preference survives); absent + legacy false gives `["auto_delegated"]` (the pre-existing default); a present table wins in both directions, including all-visible (`[]`) and all-hidden.
- `ChatSection` reads the store (already hydrated from the same resolved config) rather than re-reading settings: `ChatSection.tsx:42-44` builds the positive draft form, `:91-93` writes the store, `:103` sends `steering_notes: draft.steeringNotes` in the save patch. Equivalent source and consistent with every other toggle in that section.

### 4. Round-1 LOW 4 (doc overclaim) — mostly fixed, ONE residual (the finding below)

- Both docs and the module doc now state the two match modes (line-start anchoring OR the note's own marker shape), the `path:line:`-prefix consequence, and the raw-stream-at-column-0 caveat; the wording is consistent between `docs/CONFIGURATION.md`, `docs/FEATURES.md` and `delegationNotes.ts:5-40`, and the test title matches.
- Residual: the `edit_stale_read` matcher (`delegationNotes.ts:257`) is marker-scoped and NOT line-anchored, and `filterLine`'s whole-line pass (`:321`, via `matchesDef`, `:290`) tests patterns against the RAW line — so a location prefix does not protect a hit line. See Finding 1.

### Whole-diff re-check — nothing else broken

- Render paths. Exactly two `hiddenSteeringNotes` store reads (`Message.tsx:449` ToolCard, `:715` CallDetail; the test pins the count at 2). The collapsed chip (`:520-523`) goes through `filterSteeringNote` and keeps a merged note's visible remainder, and the `noteText !== ""` guard skips an emptied chip. The shell branch (`:725-731`) filters stdout and stderr before the empty-checks, so a fully hidden stream renders nothing rather than an empty `<pre>`. The generic `<pre>` (`:832`) always filters. I verified the shell notes are covered by the stdout filter: `src/tool/agent/shell.rs:354-370` prepends GREP_NUDGE / REDIRECT_NOTE to the combined output BEFORE the `[stderr]` marker, and `parseShellOutput` (`toolCardPaths.ts:717-722`) splits on the first `[stderr]` marker, so the notes land in `stdout`.
- Splitter integrity. `filterLine` re-joins the surviving components at the recorded offsets with `"; "` (`:331-337`), returns the ORIGINAL line when nothing was dropped (`:336`) and `null` when everything was — byte-identical remainders confirmed by hand for the merged literal-TIP + known-memory case, which carries two internal `"; "` runs and splits only at the one recognized start.
- Display-only contract. The diff changes no emission code at all — nothing under `src/tool/**` or `src/agent/**` is touched (only `src/config/**` and `src-tauri/src/ipc/**`), and `effective_steering_note_visible` has a single production consumer, the IPC DTO (`src-tauri/src/ipc/settings.rs:661`). No Rust consumer of the hidden set can reach the tool-result text, so the model's context — including a note's re-issue escape hatch — is provably unchanged.
- Lockstep. `src-tauri/src/ipc/contract_fixtures.rs:255-267` and `frontend/src/lib/ipc-fixtures/dto-get-settings.json:60-72` carry the same 11 keys with the same values in the same order; `ipc-contract.test.ts:438-452` asserts that shape; `tauri.ts:471` (resolved) and `:540` (save patch) are consistent with it.
- Type swap complete. Repo-wide, no production reference to `showDelegationNotes` remains (non-`.coding/` hits are only the new negative source pin and the documented legacy shims); `setHiddenSteeringNotes` is the single writer, and the registry default preserves the old render (`DEFAULT_HIDDEN_STEERING_NOTES` is `["auto_delegated"]`).

## Finding

### LOW 1 — the reworded matching promise still overstates for the `edit_stale_read` matcher

**Where.** `docs/CONFIGURATION.md` (the new `[ui.steering_notes]` paragraph: "…never hide content that merely quotes a note; only a raw stream that reproduces a note verbatim at column 0 … can match"), `docs/FEATURES.md` ("…so content that merely quotes a note is not hidden there…"), and `frontend/src/lib/delegationNotes.ts:36-38` (module doc: "a note QUOTED inside a result line is not hidden where that line carries a location prefix").

**Why it is wrong.** Ten of the eleven matchers are line-start anchored (prefixes matched at position 0 of the unwrapped text, or `^`-anchored patterns), so the `path:line:` prefix of a search/read hit does protect them. The eleventh — `edit_stale_read`, `delegationNotes.ts:257` — is marker-scoped and UNANCHORED, and `filterLine`'s whole-line pass (`:321`, via `matchesDef` at `:290`) tests patterns against the RAW line, prefix and all. So a hit line that reproduces the drift sentence is dropped with the toggle off, despite its location prefix. Concrete in-repo lines that match (each is a single line containing the drift clause followed by the marker):
- `.coding/reviews/2026-09-15-steering-note-toggles-review.md:20` (quoting the note inside review prose)
- `.coding/reviews/2026-09-14-steering-note-per-kind-toggles-review.md:15`
- `frontend/src/lib/delegationNotes.test.ts:55` (the test fixture constant)

**Impact.** Display-only, and opt-in: `edit_stale_read` is default-visible, so this only bites a user who unchecks "Stale-read note" and then searches/reads text reproducing the full two-clause sentence. It is a wording defect, not a behavior defect — but "never" is exactly the class of statement round-1 LOW 4 was raised about, so the reworded text does not yet fully meet the "docs match the code" bar. The registry comment at `delegationNotes.ts:255-256` ("a file's content quoting 'Re-read the file' is never hidden") is accurate as written and needs no change.

**Concrete fix (docs-only, one clause in each of the three spots).** Name the exception, e.g. append: "— with one deliberate exception: the stale-read matcher keys on the note's own drift sentence, so a line reproducing that sentence in full is hidden wherever it sits, location prefix included." Tightening the matcher instead is not practical: the text preceding the drift clause varies with the failure mode (`with_fresh_read_nudge(msg)`, `src/tool/agent/file_edit.rs:171-173`), so there is no stable line-start shape to anchor on.

## Observations (explicitly NOT findings — no action required)

1. `hiddenKeysFromConfig({})` (an empty but present object) returns `[]`, where the backend would hide `auto_delegated` in that state. Unreachable from the shipped DTO (11 non-optional booleans, no skip attribute) — only a hand-crafted payload could produce it.
2. Unchecking "Stale-read note" removes a failed `file_edit`'s entire error line from both the collapsed summary and the expanded output, because that line IS the note. Intended: round 1 asked for summary/detail consistency, and the plan records line-based stripping as the design. Flagging the user-visible consequence only.
3. The test fixtures for `shell_tip` and `shell_redirect` are shortened variants of the real `GREP_NUDGE` / `REDIRECT_NOTE` strings (the real ones carry a second `; ` clause). I hand-verified the real strings against the prefix + splitter logic: the trailing clause (`; the tool caps output itself, prefer running the command unredirected.`) is not a recognized component start, so no mis-split occurs. A verbatim-string test would close a gap that inspection already closes.
4. Out-of-task `.coding/` churn rides the same diff (the backlog item set `in_flight`, the `2027-01-11-startup-window-geometry-clamp…` knowledge amendment, and `plans/9ff59133.md`'s "Regression test" section reduced to `clampRestoredGeometry`). All app-managed bookkeeping, unrelated to this plan; no impact on the feature.

## Constitution checks

- **Documentation sync.** `docs/CONFIGURATION.md` and `docs/FEATURES.md` document the table, the 11 keys, the defaults, the legacy key and the display-only contract (modulo Finding 1). No other doc claims the old single-toggle behavior as the whole story.
- **Multi-platform neutrality.** No platform-specific API, path or shell syntax anywhere in the diff — TypeScript helpers plus serde/TOML only.
- **File-tools-first.** The diff's non-source changes are `.coding/` app bookkeeping; no shell-based file mutation.
- **Warning-free Rust, no `#[allow(...)]`.** Repo-wide search for `#[allow(` returns zero matches. New public Rust items all carry doc comments: `SteeringNotesCfg` + its fields, `UiConfig::steering_notes`, `STEERING_NOTE_KEYS`, `effective_steering_note_visible`, `GetSettingsSteeringNotes` + fields, `SteeringNotesDto` + patch application.
- **Defaults preserve old behavior.** `DEFAULT_HIDDEN_STEERING_NOTES` derives from `defaultVisible: false`, i.e. `["auto_delegated"]` only — an untouched config renders exactly what it rendered before, and the legacy `[ui].show_delegation_notes` still seeds it.

## Test status

Not re-run (read-only). Every new or changed assertion I could check by hand lines up with the shipped source: I enumerated the source pins in `delegationNotes.test.ts` (`filterSteeringNote(info.note, hiddenSteeringNotes)`, `filterSteeringNote(toolErrorSummary(lastCall.result.output), hiddenSteeringNotes)`, `{errorSummary !== "" && (`, `stripSteeringNotes(call.result.output, hiddenSteeringNotes)`, the shell stdout/stderr strips, the store-read count of 2, `not.toContain("showDelegationNotes")`), `ChatSection.test.ts` (`Steering notes in tool results`, `{STEERING_NOTES.map((def) => (`, `checked={draft.steeringNotes[def.key]}`, `s.setHiddenSteeringNotes(`, `steering_notes: draft.steeringNotes`) and `ipc-contract.test.ts:438-452` against the fixture — all present and matching. The parent's reported results (vitest 1174 passed / 83 files, `npm run build` clean, `cargo test` 2366 + 16 passed / 0 failed) are consistent with what I verified by inspection; after Finding 1's docs-only fix, the doc text change cannot affect any test, but the suites should be re-run once anyway before commit.

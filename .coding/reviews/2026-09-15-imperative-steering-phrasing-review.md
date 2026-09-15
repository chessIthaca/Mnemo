## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes for plan 987fef4c (backlog 56168c38, "Imperative phrasing for steering notes and tool-strategy guidance") on branch `wt/mnemo` — 17 paths, +53/−48. The conversion itself is sound: every listed production site now reads as an imperative, the steering marker/label contract is byte-intact, the escape hatch is untouched, and both the search.rs full-sentence pin and the steering_stats fixtures were moved onto the NEW sentences rather than weakened. The three low findings are documentation-accuracy items: a doubled amendment prefix in two knowledge records, a still-non-verbatim glob quote in `docs/FEATURES.md`, and a stale spec pointer in the `symbol_nudge` doc comment.

Method: read the plan, the full `git_read(op="diff")`, the marker table + `detect()` + label wiring in `src/agent/steering_stats.rs`, every production emission site listed in the task, the frontend registry and its fixtures, and the touched docs/knowledge records; then repo-wide searches for each removed phrasing (`prefer the graph tools`, `prefer running the command`, `prefer smaller fragments`, `Prefer update_plan`, `the preferred way`, `are safest`, `your call`, `consider memory_consolidate`) to find un-updated consumers in `src/**`, `src-tauri/**`, `frontend/**`, `README.md`, `PLAN.md`, `docs/**`, `.coding/**`. Tests were NOT re-run (read-only reviewer): the reported green suites were cross-checked by inspection — every literal changed in production is either asserted by an updated pin or asserted nowhere, so the diff introduces no un-updated assertion.

## Verified ground truth (PASS items)

### 1. Contract strings byte-intact
The marker table (`MarkerKind::marker()`, `src/agent/steering_stats.rs:113-126`), `label()` (`:166-179`) and `detect()` (`:228-271`) are untouched by the diff — that file's only changes are two test fixtures (`:798-803`, `:1093-1096`). Spot-verified each marker against its emission site:
- `SEARCH_NUDGE_MARK` = `is an indexed symbol` (`src/agent/steering_stats.rs:279`) — present in both `symbol_nudge` branches (`src/tool/agent/search.rs:677`, `:683`) and in every `alternation_nudge` entry (`src/tool/agent/search.rs:612`); the alternation prefix `from the alternation pattern '` (`search.rs:622`) is unchanged. The metric therefore still counts bare and prefixed nudges alike.
- `TIP: output redirection detected` — `REDIRECT_NOTE` still opens with it (`src/tool/agent/shell.rs:50`); `has_blinding_redirection` and the C4 doc are unchanged.
- `working-memory events accumulated this session` — consolidation note (`src/tool/steering.rs:118`); the dispatch fixture (`steering_stats.rs:1093-1096`) matches the new production text exactly.
- Untouched by the diff, verified present: `TIP: for file-content search` (`shell.rs:41`), `No symbols matched` (`codegraph.rs` `miss_hint`), `RECALLED CONTEXT`, `SYMBOL NUDGE:` (`read_files.rs:267`), `TIP: pattern has no regex metacharacters`, `known memory hit:`, `Re-read the file` (`steering_stats.rs:288`).
- Escape hatch: the `ESCAPE` const in `symbol_delegation_block` (`codegraph.rs:129`, `" (re-issue this exact search to get the plain file search instead):"`) and its memory twin are untouched — `codegraph.rs`'s two hunks are doc comments only.

### 2. Rendered text of every touched literal (the `\`-continuation joins)
Checked the rendered result, not the source lines:
- alternation tail → `…; use graph_search/graph_context for symbol hunts — use search only for text` (`search.rs:622-623`).
- bare `symbol_nudge` branch → `'{p}' is an indexed symbol — graph_context(id="{id}") gives its definition + callers in one call; use graph_search/graph_context for symbol lookups — use search only for text` (`search.rs:677-679`); fuzzy branch uses the same tail behind the `the symbol '{name}' (from pattern '{pattern}')` head (`search.rs:683-685`).
- glob hint (`search.rs:1491-1492`, `search_read.rs:352-353`) → `; the glob matched no files — if that's unexpected, use extension-anchored shapes like **/*.rs`.
- `REDIRECT_NOTE` (`shell.rs:50-51`) → `TIP: output redirection detected — failure details may be hidden; the tool caps output itself — run the command unredirected.\n`.
- consolidation note (`steering.rs:118-119`), `file_write` schema (`file_write.rs:81`), `file_edit` large-payload note (`file_edit.rs:197-202`), plan.rs `update_plan` (`:925-929`), `abandon_plan` (`:1512-1517`), the feature-scale NOTE (`:828-834`), the prompt preamble (`prompt.rs:42-44`): all imperative, all grammatical after the joins, no duplicated/missing whitespace introduced by the continuations.
Repo-wide search over `src/**` and `src-tauri/**` for `prefer / consider / are safest / your call` finds only internal comments, test names, config "preferences", and the two deliberate leaves judged below — no advisory-framed model-facing string survives.

### 3. Pins moved, not weakened
- `src/tool/agent/search.rs:2167-2175` still asserts the FULL bare-identifier sentence (a single contains-assertion spanning `'hello' … use search only for text`); I compared it char-by-char against the rendered branch-1 production string — identical. Not shortened, not relaxed to a marker-only check.
- `steering_stats.rs:798-803` (ShellRedirect matrix row) and `:1093-1096` (ConsolidationDue dispatch fixture) carry the new production strings.
- No test anywhere still pins an old phrasing, and none was relaxed: searches for `prefer the graph tools`, `prefer running the command`, `prefer smaller fragments`, `the preferred way`, `are safest`, `your call` return zero hits in `src/**`/`src-tauri/**`.

### 4. Frontend display filter unaffected
Every matcher in `frontend/src/lib/delegationNotes.ts` keys on stable phrases (`AUTO-DELEGATED`, `from the alternation pattern`, `'…' is an indexed symbol`, `the symbol '…' (from pattern '…') is an indexed symbol`, `TIP: for file-content search`, the `"hint": "No symbols matched` JSON shape, `RECALLED CONTEXT`, `SYMBOL NUDGE:`, `TIP: pattern has no regex metacharacters`, `known memory hit:`, `/^NOTE: \d+ working-memory events accumulated this session/`, `TIP: output redirection detected`, the stale-read drift sentence) — none references a removed tail, so no toggle regressed. The new `; use graph_search/…` tail is not a recognized component start (`isComponentStart`/`matchesDef`), so the note cannot mis-split, and `delegationNotes.test.ts:189-196` pins exactly that merged case with the new text. The updated fixtures (`:50-51`, `:142-160`) are accurate renderings of the new production text; `shell_redirect` remains a documented "representative" (not verbatim) fixture, unchanged here and still matching its prefix.

### 5. Docs + constitution
- `docs/FEATURES.md` glob quote and the consolidation sentence ("…consolidation-due note directing `memory_consolidate`") are updated. `README.md`, `PLAN.md`, `docs/CONFIGURATION.md` contain no quote of any changed string (`PLAN.md:246-266` describes the notes in prose only; its "advisory only" still means non-gating, so it remains accurate).
- Multi-platform neutrality: pure string/doc-comment edits, no OS-specific code; no new non-ASCII introduced into a file that lacked it (every touched file already used em dashes/curly quotes on untouched lines), so no encoding hazard in the Rust/TS/MD diffs.
- File-tools-first: no shell-mutation artifacts in the diff (line-scoped hunks, no BOM, no line-ending churn, no whole-file rewrites).
- Warning-free / doc comments: no `#[allow(...)]` added; the only Rust items touched are string literals and `///` comments — consistent with the reported green `cargo test` under `#![deny(warnings)]` at both crate roots.

### 6. Semantics preserved
- `plan.rs`: `update_plan` is still the non-destructive in-place edit path (`Edit the active plan in place WITHOUT abandoning it — use this to…`); `abandon_plan` still `DESTRUCTIVE LAST RESORT` + `Use update_plan whenever the plan is still broadly correct`; the module doc still calls update_plan the non-destructive way to adjust scope; the bug_fixing feature-scale NOTE is still non-gating and now says so explicitly (`This is a NOTE, not a gate.`), with its test (`plan.rs:2715-2763`) asserting only `NOTE (backlog 51ee41c1)` + `kind=implementation`, both still present.
- File tools still steer to the targeted edit, imperatively: `file_write.rs:81` `For a targeted change to an existing file, use file_edit.` and `prompt.rs` `Edit existing files with file_edit (targeted), not a full file_write rewrite`.

## Findings

### 1. LOW — doubled, contradictory amendment prefix in two knowledge records (bookkeeping, introduced by this plan)
`.coding/knowledge/how/2026-08-29-search-glob-shapes-extension-anchored-only-bare.md:8` and `.coding/knowledge/spec/2026-09-01-symbol-intent-patterns-earn-the-graph-nudge-shap.md:14` now open their amendment paragraph with `Amended 2027-01-11: Amended 2027-01-14: …` / `Amended 2027-01-11: Amended 2027-01-14 (plan 987fef4c…`. Cause: `memory_amend` already prefixes the paragraph with `Amended {stamped_date}: ` (`src/memory/knowledge.rs:792-797` — `"{}\n\nAmended {}: {}"`), and the supplied paragraph started with its own `Amended 2027-01-14…` heading. The two dates also disagree in the same line, which will confuse the next reader of the record.

Fix: repair the prefix in place so each paragraph opens with exactly one heading, e.g. `Amended 2027-01-14 (plan 987fef4c, backlog 56168c38): …`, leaving the rest of the paragraph byte-identical. `.coding/knowledge/**` is file-tool-protected by design, so this is the sanctioned last-resort shell-fallback case (state the justification in the call); a further `memory_amend` cannot remove the stray prefix, it only appends another `Amended …:` paragraph.

### 2. LOW — `docs/FEATURES.md` glob quote is still not verbatim (pre-existing elision, carried through)
`docs/FEATURES.md:41` quotes `"the glob matched no files — use extension-anchored shapes like **/*.rs"`, while production (`src/tool/agent/search.rs:1491-1492`, `src/tool/agent/search_read.rs:352-353`) emits `"the glob matched no files — if that's unexpected, use extension-anchored shapes like **/*.rs"`. The elision of `if that's unexpected, ` predates this plan (the previous quote dropped it too), so the change did not introduce it — but the text is presented as the emitted string and is the thing a reader greps.

Fix: add the missing clause to the quoted string (three words), or mark it as an excerpt (`the glob matched no files — … use extension-anchored shapes like **/*.rs`).

### 3. LOW — stale spec pointer in the `symbol_nudge` doc comment
`src/tool/agent/search.rs:646-650` still says "…the bare-identifier sentence is byte-pinned by the 2026-09-15 steering spec and must not change." That record is `status = "superseded"` (`.coding/knowledge/spec/2026-08-29-symbol-nudge-carries-the-symbol-id-graph-context.md:2-5`, where the removed tail now lives only as history), and the sentence deliberately changed under this plan; the pin that actually enforces it is the test.

Fix: point the comment at the live pin and record the rewording, e.g. "…the bare-identifier sentence is byte-pinned by `bare_identifier_symbol_searches_earn_the_graph_nudge`; wording updated 2027-01-14 (plan 987fef4c, backlog 56168c38)" — keeping the "don't change it casually" intent.

## Deliberate leaves — judgment (as requested; no conversion required)
- `src/tool/agent/codegraph.rs:241` `/// A symbol name to look up (exact match preferred).` — **DEFENSIBLE, leave it.** The graph tools build their model-facing schemas by hand (`json!`, e.g. `graph_search` at `codegraph.rs:271-290`, whose `query` description already reads factually: "The symbol name to search for (exact matches first, then case-insensitive, then substring)"), so this doc comment does not reach the model — and no schema string in `codegraph.rs` contains `prefer`. It also states resolution ORDER (exact beats fuzzy candidates), not a tool-choice preference. Converting it would be cosmetic churn on an internal comment; optional nit only: "exact match first" would mirror the schema wording.
- `src/tool/workflow/ask_user.rs:43` `/// The clickable options (at least 2; 2–6 recommended).` and its model-facing twin `ask_user.rs:106` `"The clickable options (at least 2; 2–6 ideal)."` — **DEFENSIBLE, leave them.** This is cardinality guidance with an ENFORCED floor (`minItems: 2`, plus the runtime check at `:164-170`) and a deliberately soft ceiling; an imperative "use 2–6" would assert a bound the validator does not enforce, and enforcing it would be a behavior change outside this plan's scope. Both strings are factual, not `prefer/consider`-shaped. Optional nit only: align the doc comment with the schema text ("2–6 ideal").
- Explicit recommendation: no conversion needed for either. If the user later wants a literal zero-occurrence sweep, the honest edits are the two rewordings above — not imperative commands that over-state a contract.

## Non-findings (checked, deliberately not flagged)
- The three superseded nudge SPEC records still quote the old tail — correct: they are `status = "superseded"` history (`memory_amend` refuses superseded records, `src/memory/knowledge.rs:780-785`), and the plan amended the LIVE records (the 2026-09-01 symbol-intent spec + the glob HOW record) with the new wording.
- `plan.rs:925-929` "use this to adjust … and use it whenever the plan is still broadly correct" is slightly repetitive — taste only; imperative form and semantics are correct.
- `search.rs:29-33` module-doc prose ("grep is for text", "symbol lookups belong to the graph tools") is internal documentation, not a model-facing string; correctly left as-is.
- `frontend/src/lib/delegationNotes.test.ts` `shell_redirect`/`consolidation_due` fixtures are shortened variants — the file documents its fixtures as representative shapes (only `graph_miss`/`edit_stale_read` are verbatim) and the matchers key on prefixes; not a defect, and unchanged by this plan.

## Test status
Not re-run (read-only reviewer). Basis for accepting the reported green suites: every literal changed in production is either covered by an updated assertion (`search.rs:2167-2175`; `steering_stats.rs:798-803`, `:1093-1096`; `delegationNotes.test.ts:142-160`, `:189-196`) or asserted nowhere (zero hits in `src/**`, `src-tauri/**`, `frontend/**` for every removed phrasing), so the diff leaves no un-updated pin behind. The three findings above are documentation/bookkeeping accuracy items only — none touches a marker, a label, a matcher, or a test contract.

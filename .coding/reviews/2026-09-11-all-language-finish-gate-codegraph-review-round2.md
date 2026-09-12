## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of plan c87093c1 (bug_fixing) at commit 9180626 on wt/agenticcoding (working tree clean, main untouched). All four round-1 findings are fixed, correctly implemented, and pinned by genuine regression tests; the fixes introduced no regressions. Independent verification of the HIGH-1 fix against the exact pinned grammar versions surfaced one new low finding: the arm's `generator_function_expression` kind string does not exist in either pinned grammar, so generator-valued const bindings still yield no symbol.

## Round-1 fix verification

### HIGH-1 — arrow-const regression tests: FIXED (verified)

- `visit_ts` (src/codegraph/extract.rs:449-490) gained the `"lexical_declaration" | "variable_declaration"` arm: function-valued `variable_declarator`s (identifier names only — destructuring patterns are filtered out) become `Function` symbols; non-function bindings keep the generic recursion so calls in initializers are still recorded. Scope push/pop mirrors the `function_declaration` arm.
- Tests: `extracts_js_arrow_const_functions` (extract.rs:1789) pins arrow-const → Function, `let v = function () {}` → Function, `const c = new Widget(1)` → no symbol, and the `helper()` call ref inside the arrow body. `finish_resolves_arrow_const_regression_test_in_js` (src/tool/workflow/plan.rs:4484) writes a .js arrow-const test, indexes, and asserts finish succeeds — on pre-fix code the symbol is unresolvable, the forced re-index still finds nothing, and the gate errors, so the test genuinely fails without the fix.
- Grammar verification (fetched the exact pinned grammars): tree-sitter-javascript **v0.25.0** grammar.js has `optional('async')` *inside* the `arrow_function` and `function_expression` rules, so `const f = async () => {}` and `async function () {}` are covered by the matched kinds. tree-sitter-typescript 0.23.2's common/define-grammar.js does not override these rules (its package.json pins tree-sitter-javascript ^0.23.1 as the grammar base), so .ts/.tsx behave identically.
- Residual gap → new LOW-1 below.

### LOW-1 — stale doc comments: FIXED (verified)

- `SymbolKind` doc (extract.rs:21-26) now says "unioned across every indexed language".
- `stale_source_files` doc (src/codegraph/mod.rs:750-760) now says "any [`Lang`] extension — the only files that carry symbols". Both are accurate.

### LOW-2 — substring disk containment: FIXED (verified)

- `contains_whole_identifier` (src/codegraph/mod.rs:130-147): byte-level word-bounded matching — an occurrence counts only when both flanking bytes are non-identifier (or string edges); `from = start + 1` correctly resumes after non-whole hits, so overlapping occurrences are handled. The empty needle is guarded in `reindex_files_containing` (mod.rs:643-650, zeroed outcome), so the `windows(0)` panic is unreachable.
- The pass-arm comment (plan.rs:1677-1690) no longer overclaims: it states word-bounded containment as the evidence and explicitly carries the comment-mention residual in the note.
- Test extension (mod.rs:1009-1016): the partial name `swift_gate_regressio` yields `matched_unparsed == 0` — substring hits inside a longer identifier are not containment.

### LOW-3 — .mts/.cts drift: FIXED (verified)

- `LANG_EXTENSIONS` (src/codegraph/walk.rs:67-94) is now the single source of truth; `Lang::from_extension` consumes it (walk.rs:99-104); `is_code_extension` (walk.rs:120+) admits mts/cts/html/htm plus the broader non-Lang code set.
- `is_code_extension_is_a_superset_of_lang_extensions` (walk.rs:360-373) iterates the table exhaustively, asserting both the `from_extension` round-trip and `is_code_extension` — the drift cannot recur silently.

## New finding

### LOW-1 (new): dead kind string `generator_function_expression` — generator-valued const bindings still yield no symbol

- Location: src/codegraph/extract.rs:475 (the HIGH-1 arm's `is_fn` kind list).
- Evidence: tree-sitter-javascript **v0.25.0** (the exact Cargo.lock pin) names the generator function *expression* rule `generator_function` — there is no `generator_function_expression` rule in that grammar. tree-sitter-typescript 0.23.2 inherits its function-expression rules from the base grammar (^0.23.1, not overridden in common/define-grammar.js), and v0.23.1 also names it `generator_function`. The repo's own green fixture (`let v = function () {}` → Function) empirically confirms that grammar rule names are the emitted node kinds for these crates. No repo fixture uses `function*`; the only occurrence of the string is the arm itself.
- Consequence: `const gen = function* () {}` (and `async function*`) → `is_fn == false` → generic recursion → no symbol, so the finish gate would false-error on a generator-valued const regression test — the same class as HIGH-1 in a rare shape (jest/vitest tests are sync or promise-returning, so this is uncommon). The kind string is also dead code that gives false confidence of coverage.
- Fix: match `"generator_function"` instead of `"generator_function_expression"`, and extend the `extracts_js_arrow_const_functions` fixture with `const gen = function* () {};` — optionally also `const a = async () => {};` to pin the async-inside-`arrow_function` behavior the grammar guarantees but no fixture currently covers.

## Regression check

- The arm's non-function path preserves generic recursion — calls in initializers still recorded (pinned by the `new Widget(1)` fixture); for-header declarators (no value) and destructuring names fall through harmlessly; `export const f = () => {}` reaches the arm via the export_statement generic-recursion fallthrough.
- Gate arms (plan.rs:1654-1715) are coherent: absent-everywhere → error; unparsed-language hit → pass-with-note; parse-failed → inconclusive error; reparsed-but-absent → final error.
- No other callers of `reindex_files_containing` are affected by the word-boundary change; the empty-needle path returns the zeroed outcome that correctly feeds the "absent" error arm.

## Observations (no action required)

- `$` is a valid identifier byte in JS but not in `is_ident` (mod.rs:126-128), so a needle that is a prefix of a `$`-containing identifier still counts as contained. Edge-of-edge case in a note-carrying soft-pass arm whose comment already documents residual softness; per-language identifier rules are not warranted.
- The `windows()` O(n·m) containment scan is already tracked as polish (backlog a6e031a9 / plan 66c08b9e).

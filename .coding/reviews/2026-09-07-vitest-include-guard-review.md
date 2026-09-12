## Verdict: PASS

The vitest include allow-list guard is correct, minimal, and faithful to vitest's own include matching for the config's vocabulary; both include additions are right and the guard is self-registered with the caveat documented; root cause and regression proof are documented; docs and memory hygiene are accurate; no security, multi-platform, or regression issues. Three non-blocking informational notes below — all within the plan's stated scope, no change required.

## Scope reviewed

All uncommitted changes on wt/agenticcoding (git diff HEAD + untracked files):

- `frontend/src/lib/vitestInclude.test.ts` (NEW — the guard)
- `frontend/vitest.config.ts` (+2 include entries: the guard itself, `src/App.shellRender.test.ts`)
- `README.md` (Tests line)
- `.coding/knowledge/decision/2027-01-07-frontend-vitest-uses-an-explicit-include-allow-l.md` (updated to record the guard), `.coding/knowledge/how/2027-01-06-…` (deleted duplicate), `.coding/knowledge/how/2027-01-07-…` (updated)
- `.coding/backlog.jsonl` (item 263313de pending → in_flight, plan_id stamped), `.coding/plans/f6ea6fc0.md` (plan, untracked)


## Independent verification performed

- **File census (corroborates the guard's green status):** searched all `*.test.ts` (66 files) and `*.test.tsx` (8 files) under `frontend/src` → **74 files total**, matching the reported 74-file green run exactly. Cross-checked against `test.include`: the 66 literal entries each correspond to exactly one discovered file (no dead/stale entries), and the single glob `src/components/settings/**/*.test.ts` covers exactly the 8 settings `*.test.ts` files (mcpSection, types, sections/{Advanced,Chat,Pricing,Sounds}Section, sections/maintenanceUi, sections/modelOptions). 66 + 8 = 74 — no unregistered file exists under src/, and no include entry matches nothing.
- **tsconfig:** `allowImportingTsExtensions: true` + `noEmit: true` + `moduleResolution: "bundler"` confirm `import vitestConfig from "../../vitest.config.ts"` type-checks (consistent with the reported `npx tsc` exit 0; the import target outside the `include: ["src"]` root is fine — tsc follows imports from root files).
- **Star-slash audit (review focus 4):** all 5 `\*/` occurrences in vitestInclude.test.ts are the 3 legitimate block-comment terminators (lines 23, 32, 47) plus 2 inside string literals in code (line 34 — the glob pattern, line 54 — the regex fragment). No premature block-comment termination remains; the one fixed during development is confirmed fixed.
- **frontend/ top level:** no test files outside `src/` (bench/ is for benchmarks; no e2e/tests dirs) — the guard's src-rooted scope covers every test file that exists.
- **App.shellRender.test.ts:** pre-existing committed file (not modified by this change), 5 source-contract tests using `?raw` imports typed by vite/client. Registering it (not touching it) is the correct minimal fix for the second live catch; its 5 tests now run and pass per the green run.
- **Vitest 4.1.10 / Vite 8.2.1** (package.json): brace expansion in `import.meta.glob("/src/**/*.test.{ts,tsx}")` is fully supported; the reported green run including all 8 `.test.tsx` files proves discovery works.

## Review focus findings

### 1. Guard correctness — PASS

- **Discovery:** `import.meta.glob("/src/**/*.test.{ts,tsx}")` is Vite's transform-time glob rooted at the project root; keys come back as "/src/…" and `replace(/^\//, "")` normalizes to the config's "src/…" form. `**` matches zero or more segments (the red proof at both src/ root and src/lib/ confirms the zero-segment case). This mirrors the filesystem view a default discovery include would use.
- **Matching:** `globToRegExp` faithfully mirrors vitest's (tinyglobby/fast-glob) semantics for the vocabulary the config uses:
  - `**/` → `(?:.*/)?` — zero or more whole segments. Because the remainder (`[^/]*\.test\.ts`, anchored by `$`) cannot contain `/`, the split point is forced to a segment boundary — exactly glob `**/` semantics. Files directly in the directory (zero segments) match, as in fast-glob.
  - bare `**` → `.*`, single `*` → `[^/]*` (does not cross `/`), `?` → `[^/]` — all equivalent to fast-glob.
  - Escape set `"\\^$.|+()[]{}"` plus `*`/`?` handled as glob semantics covers every JS regex metacharacter outside character classes — complete. Literal entries therefore match exactly themselves, identical to vitest's literal-path matching.
  - Anchoring: JS `$` without the `m` flag matches only at end of input (no trailing-newline leniency) — correct; `^…$` is the right anchoring.
  - Case sensitivity: the regex is case-sensitive, matching fast-glob's default `caseSensitiveMatch: true` — consistent with vitest.
- **False-positive/false-negative risk in the current tree: none** — independently corroborated by the census above (74/74 correspondence, both directions). Out-of-bounds `glob[i+1]`/`glob[i+2]` reads yield `undefined` and fall through safely. No ReDoS surface (anchored, linear patterns on short paths).

### 2. Include additions — PASS

Both entries are correct and minimal: `src/lib/vitestInclude.test.ts` (the guard itself — self-registered, with the self-reference caveat honestly documented in the header: removing it from the list silences it) and `src/App.shellRender.test.ts` (a real second live catch, now running its 5 tests). No other entries touched; the allow-list curation design is preserved per the plan.

### 3. Root cause + regression test — PASS

Root cause (explicit include allow-list instead of default discovery glob → unregistered files invisible to the runner) is documented in the guard's header doc comment and the plan's Goal/Bug sections. The regression test is the guard itself: it fails without registration and passes with it — the red→green proof (scratch file at both directory depths, both discovery implementations) is recorded in the plan. This is the right shape for this defect class: the regression test is a permanent invariant, not a one-off.

### 4. Documentation sync — PASS

- README Tests line accurately documents the registration requirement and the guard's failure mode.
- Guard doc comments are accurate (defect, both live catches, self-reference caveat, glob vocabulary scope).
- Knowledge hygiene: the decision record and surviving how record now record the guard as enforcement with both live catches; the deleted 2027-01-06 how file was a near-duplicate of the 2027-01-07 one (same workflow) — its unique historical detail (the 2027-01-06 review catch, fix 9a0d5b9) remains in git history; acceptable consolidation. The stale in-memory index converges on rebuild per project policy (files are the truth).
- Backlog item 263313de correctly reflects in_flight + plan_id.

### 5. Multi-platform neutrality — PASS

No Windows-only APIs, paths, or shell syntax. Both comparison sides use forward-slash, root-relative paths: Vite normalizes `import.meta.glob` keys to forward slashes on all platforms, and the config entries are forward-slash relative paths — the comparison is platform-independent. No `node:` imports (deliberately avoided; the project has no @types/node).

### 6. Security / bugs / regressions — PASS

No eval or dynamic code execution; regexes are built from repo-controlled config entries with complete escaping; no injection or ReDoS surface. The change is purely additive (one new test file + two include entries) — zero behavior change to existing tests, confirmed by the flat cargo/src-tauri results and the 1027-test green run.

### 7. BUG: memory — noted, not a finding

Auto-captured by the bug_fixing plan kind at finish; expected absent at review time.

## Informational notes (non-blocking, within plan scope — no change required)

1. **globToRegExp vocabulary is scoped to the config's current syntax** (double-star+slash, bare double-star, single star, `?`, literals). A future include entry using brace expansion, character classes, or extglobs would make the guard fail LOUD (listing registered files as unregistered) until the vocabulary is extended — a confusing-but-safe failure mode, and the doc comment explicitly scopes the supported vocabulary. If include syntax ever grows, extend globToRegExp alongside.
2. **Discovery is rooted at /src/** — a test file created outside `frontend/src` (e.g., at the frontend root) would remain silently unguarded. None exist today (verified: frontend root has no test files; bench/ is benchmarks), and the plan's goal is explicitly scoped to "under frontend/src". The README's "new frontend test files" wording is marginally broader than the guard's src/ scope — optional nit: could say "under src/".
3. **The guard validates include-coverage only** — a future `test.exclude` entry could silently skip a registered file while the guard stays green. No exclude exists today; noted so the invariant is understood if one is ever added.

## Conclusion

The change delivers exactly what the plan scoped: the allow-list trap is now loud, the guard is itself the regression test, both live catches are fixed, and documentation/memory records are in sync. Test status corroborated independently (74-file census matches the green run exactly). Ready to commit.

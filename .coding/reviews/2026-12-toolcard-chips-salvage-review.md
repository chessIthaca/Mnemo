## Verdict: FINDINGS (2 high, 0 low)

# Review — plan 9a131eb7 "ToolCard headers: unique file-name chips + salvage names from truncated args"

Reviewed delta: commit `73a6c04` (= the entire `30820ec..HEAD` change for the three files; merged to main via `de0f0bb`; tree clean). Scope per stat: `frontend/src/lib/toolCardPaths.ts` (~86 lines: `salvagePathLiteral`, `disambiguateDuplicateTexts`, argPaths salvage branch), `frontend/src/lib/toolCardPaths.test.ts` (+106: 4 salvage tests, 5 unique-name tests), `frontend/src/components/chat/Message.tsx` (4 lines, comment/wiring only), plus plan/knowledge/backlog records. Reviewed from current HEAD file contents + commit stats + plan record; tests were reported green by the session (593/593 vitest incl. 49 in this file, cargo 1518 passed, tsc+vite build exit 0) and not re-run by this read-only reviewer — the code paths were verified by inspection instead.

---

### Finding 1 — HIGH (correctness): salvage bypasses the label-only tool exclusion; the pinned test passes for the wrong reasons

**File:** `frontend/src/lib/toolCardPaths.ts` — `argPaths()` lines 50–73 (catch block 54–57), `salvagePathLiteral()` doc comment lines 18–30.

The `catch` branch returns the salvaged literal **before any `toolName` check** — the label-only exclusions (shell, spawn_agent, skill_start, git, git_read, search, search_read) live at lines 59–73 and are only reached when `JSON.parse` *succeeds*. So an excluded tool with truncated args whose `"path"` value finished streaming before the cut still gets a link chip:

```
argPaths('{"op":"log","path":"src/lib.rs","limit":20', "git_read")  →  ["src/lib.rs"]   // must be []
```

This violates the plan's stated invariant ("those must never gain a link chip even via salvage") and `argPaths`' own doc ("returns [] so the header doesn't render a bogus link"). git_read is the concrete exposure today (its schema carries `path` as a log/diff filter, often a directory); in `Message.tsx` lines 507–512 the non-empty `argPaths` result additionally suppresses that call's `argLabel` chip. The `salvagePathLiteral` doc comment is factually wrong: "no mapping, no exclusions — those ran before the caller ever got here" — they run *after*, and only on the parse-success path.

The regression test "does not salvage for label-only tools even when a path literal is present" (test lines 91–96) is vacuous: the git_read case `{"op":"log","path":"src/li` is truncated *inside* the value string, so the regex can't match (no closing quote) — it pins the regex, not the exclusion; the shell case is *valid* JSON, so salvage never runs. Neither exercises "parse fails + complete literal + excluded tool", and both would pass unchanged against the pre-fix code as well.

**Fix:** hoist the label-only exclusion above the `try` (it needs no parsed JSON), or check `toolName` inside the `catch` before salvaging; correct the `salvagePathLiteral` doc comment; add the counterexample above as a regression test.

---

### Finding 2 — HIGH (correctness/perf, UI hang): `disambiguateDuplicateTexts` termination claim is false for same-segment-sequence paths → infinite loop

**File:** `frontend/src/lib/toolCardPaths.ts` — lines 139–184 (`for (;;)` at 165, doc claim at 142–146).

Termination rests on "at full depth every lowercased rendering differs" given pairwise-distinct normalized paths. But `normalizePath` (116–118) does **not** collapse interior double slashes or strip a leading slash, while `segmentsOf` (151–155) filters empty segments. Two paths that are distinct under `normalizePath` yet share an identical non-empty segment sequence collide at *every* depth:

- `"/src/main.rs"` vs `"src/main.rs"` (leading-slash — a classic LLM absolute-path typo in one of two grouped calls), or
- `"src//util.ts"` vs `"src/util.ts"` (interior double slash).

Both survive `dedupePaths`, land in one basename group, produce identical `qualified` at every `segs` → `collided` stays true → `segs += 1` forever. `buildPathChips` runs synchronously in `ToolCard` render (`Message.tsx` line 502), so the UI thread freezes on any card containing such a pair. The loop is unbounded (`for (;;)`, no depth cap) — the checklist's termination argument is sound only under the missing assumption "distinct normalized ⇒ distinct segment sequences", which the leading-/double-slash cases disprove.

**Fix (either; the first is the safer belt-and-braces):**
1. Bound the loop: precompute `maxDepth = max(parts.length)` and iterate `segs = 2..maxDepth`; if still colliding at `maxDepth`, fall back to `text = entries[i].path` (the raw path — raw paths are pairwise distinct whenever normalized paths are, so even lowercased they differ) and return.
2. Or make the invariant actually hold: extend `normalizePath` to collapse `/{2,}` → `/` and strip a leading `/` (after backslash normalization), so the pathological pairs dedupe away.

Add regression tests feeding `["/src/main.rs","src/main.rs"]` and `["src//util.ts","src/util.ts"]` through `buildPathChips` — today these hang the runner (vitest timeout); post-fix they must return promptly with distinct texts.

---

## Checklist findings

**1. Correctness of the fix (beyond the two findings):**
- Unique-chip guarantee holds for all *covered* shapes: collision groups keyed by case-insensitive basename are correct (group texts always end in the distinct basename, so cross-group collisions are impossible); per-group walk-up with `Math.min(segs, parts.length)` handles shorter paths; comparison is consistently `toLowerCase()`; Windows backslashes normalized in both `segmentsOf` and `normalizePath` and pinned by a dedicated test; overflow `+N` accounting is computed from `deduped.length` *before* qualification and can't be perturbed by it (pinned by a test); chip React keys stay unique (`path:<normalized>` from the deduped list + `path:overflow`).
- Salvage false-positive audit: values quoted *inside* JSON strings are escaped (`\"path\"`), which defeats the regex; first-literal-wins matches the first `read_files` spec (pinned); escaped quotes inside a salvaged value mis-decode (`"a\"b.rs"` captures `a\`) — cosmetic and vanishingly rare, noted but not a finding. The material false positive is Finding 1 (excluded tools).
- `argPaths`/`argLabel` interplay is otherwise consistent: for a truncated non-excluded call, `argLabel` returns null (its own parse fails), so the salvaged chip replaces nothing.

**2. Regression tests:** the mechanism is genuinely exercised. Salvage tests 3 of 4 fail against the pre-fix code (30820ec `argPaths` returned `[]` on parse failure, per the plan record and prior commit); the "unique display names" tests fail pre-fix (texts were bare, duplicated basenames). "leaves non-colliding chips unqualified" also fails pre-fix (expected `src/util.ts` vs old `util.ts`). Material coverage gaps are exactly the two counterexamples in the findings (excluded-tool salvage; same-segments pair). No other gaps considered material.

**3. Bug-plan requirements:** BUG memory `9d0d4695` exists (verified via memory query) with symptom → root cause → fix pointer; the follow-up residual BUG `ef81d215` and the committed knowledge file (6 lines, in `73a6c04`) document this plan's root cause; plan record `9a131eb7` is kind `bug_fixing` with `regression_test: frontend/src/lib/toolCardPaths.test.ts`, and the tests exercise both changed paths (`argPaths` catch branch and `buildPathChips` → `disambiguateDuplicateTexts`). ✔

**4. Project review expectations:** Documentation — no README.md/PLAN.md update needed (README's only tool-card line, `README.md:72`, is still accurate — link targets unchanged; PLAN.md contains no chip-related content). Module doc comments are otherwise thorough and updated; the two inaccurate doc claims are folded into Findings 1 and 2 rather than counted separately. Multi-platform neutrality — pure TS, separator handling is slash-agnostic and backslash behavior is *tested*, no Windows-only APIs, no `cfg(windows)` additions. ✔

**5. Bugs/security:** No new path-traversal class (salvaged paths originate from the same LLM tool args as parsed paths; the Files-viewer link behavior is pre-existing). The salvage regex is linear (simple alternation + `[^"]+`) — no backtracking blowup on large args. Case-insensitive comparisons use deterministic `toLowerCase()`. The only perf hazard is Finding 2's unbounded loop.

## Positive notes

- Keeping both mechanisms in React-free pure TS with node-env vitest coverage is exactly the right shape, and the plan's root-cause documentation chain (memory → knowledge file → plan → tests) is complete.
- The tsc build break (split string literal) was correctly diagnosed and fixed inside the same commit, with the vitest/esbuild-vs-tsc gap documented in the commit message.

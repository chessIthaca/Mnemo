## Verdict: PASS

Round-2 verification of the single round-1 finding (LOW 1 — frontend label mirror diverged for `paths: []` + singular `path`) from `.coding/reviews/2026-09-08-git-restore-subcommand-review.md`. Plan 514dcee1, branch wt/agenticcoder, HEAD = `e039f56` (the fix is committed; verified via `git show e039f56` + direct file reads of the committed state). The finding is correctly fixed and the fix introduces nothing new.

## 1. Message.tsx — fix verified correct (committed lines 477-501)

The restore branch now reads:

```ts
const pathArray: unknown[] = Array.isArray(parsed.paths) ? parsed.paths : [];
const pathList =
  pathArray.length > 0
    ? pathArray.filter((p): p is string => typeof p === "string" && p.trim().length > 0).join(" ")
    : typeof parsed.path === "string" && parsed.path.trim().length > 0
      ? parsed.path.trim()
      : "";
```

Checked against the backend normalization in `src/tool/agent/git.rs` (`has_paths = args.get("paths").map(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)).unwrap_or(false)`; lift `path` only when `!has_paths`). Truth tables match on every input class:

| Input | Backend | Frontend chip |
|---|---|---|
| `paths` absent | `has_paths`=false → lift `path` | `pathArray=[]` → `path` branch ✓ |
| `paths` non-array (e.g. string) | `as_array()`→None → false → lift `path` | `Array.isArray` false → `[]` → `path` branch ✓ |
| `paths: []` + `path: "b.txt"` (the finding) | empty → false → lifts `path` → restores b.txt | length 0 → `path` branch → "restore -- b.txt" ✓ (was bare "restore") |
| `paths: ["a.txt"]` + `path: "b.txt"` | non-empty → true → `path` ignored → restores a.txt | array branch, no fall-through → "restore -- a.txt" ✓ |

The non-empty-array branch has no `path` fallback, exactly as the round-1 finding required. The comment above the code accurately documents the backend rule it mirrors.

## 2. messageArgLabel.test.ts — regression expects verified (committed lines 76-88)

Both regression expects are present in the "labels restore calls" test and assert the right behavior:

- `{"subcommand":"restore","paths":[],"path":"b.txt"}` → `"restore -- b.txt"` — would FAIL on the pre-fix code (`Array.isArray([])` took the array branch → empty join → bare "restore"), so it genuinely pins the fix.
- `{"subcommand":"restore","paths":["a.txt"],"path":"b.txt"}` → `"restore -- a.txt"` — pins the no-fall-through requirement, guarding against a sloppy fix that falls back to `path` even for non-empty arrays.

## 3. Sanity — no new issues

- **Type-safety:** the intermediate narrowing-through-a-boolean-alias error is absent from the committed state; the `pathArray: unknown[]` local with the `Array.isArray` ternary and the `(p): p is string` predicate on `unknown[]` are sound TS. Consistent with the reported clean `tsc --noEmit` (reviewer is read-only and did not re-run it, per instructions).
- **No behavior change elsewhere:** the new code sits entirely inside `if (sub === "restore")` and returns on every path; the generic git `args` branch below it and the `git_read`/`spawn_agent`/`skill_start` branches are untouched.
- The blank-string filtering inside the array branch matches the pre-existing filter style of the `args` branch (Message.tsx:504) and is display-only.

## Informational note (not a finding)

A pathological `paths: [" ", "a.txt"]` shows only "a.txt" on the chip while the backend forwards both pathspecs to git — pre-existing filter behavior from the original implementation (round-1 reviewed it and did not flag it), display-only, unchanged by this fix.

Reported post-fix verification (frontend `tsc --noEmit` clean, `npm test` 636 passed) is consistent with the code read.

## Verdict: PASS

The `backlog_add` title-chip change (plan 6fe36ffb) is correct, well-tested, frontend-only, and faithfully mirrors the established `search` quoted-free-text chip convention. No blocking findings; one optional, non-blocking doc-comment observation noted in §5.

### Scope reviewed
- `frontend/src/components/chat/Message.tsx` — `BACKLOG_TITLE_MAX` const + `backlog_add` branch in `argLabel()` (lines 437-438, 585-600).
- `frontend/src/lib/toolCardPaths.ts` — `backlog_add` added to the label-only exclusion list in `argPaths()` (lines 66-68).
- `frontend/src/components/chat/messageArgLabel.test.ts` — `argLabel — backlog_add` describe block (lines 225-266).

Out-of-scope pre-existing WIP observed but NOT evaluated for this verdict (per task): `src/provider/anthropic.rs`, `src/provider/openai.rs`, `src/provider/stream.rs`, `.coding/knowledge/decision/*.md`, `.coding/backlog.jsonl`.

### 1. Correctness — PASS
- **Field name verified.** `backlog_add`'s title argument is `text` — confirmed by the frontend invocation `invoke("backlog_add", { text, images })` (frontend/src/lib/tauri.ts:1497) and the backlog item shape (`{"id","text",...}` in backlog.jsonl). Reading `parsed.text` is correct.
- **Pattern match.** The branch mirrors `search`/`search_read` (Message.tsx:579-584): typeof-guard → trim → null-if-blank → return `"${...}"` quoted. The only additions (first-line extraction + truncation) are appropriate because backlog `text` can be a multi-line prompt up to 4000 chars, unlike a search pattern. Placement (after `search`/`search_read`, before the `graph_*` dispatch) matches the plan.
- **Edge cases all correct:**
  - Empty/blank/missing/non-string `text` → `text=""` → `firstLine=""` → `return null`. ✓ (test 256-261)
  - Multi-line → `split(/\r?\n/)[0]` takes the first line only. ✓ (test 241-244)
  - Exactly 80 chars → `80 > 80` is false → verbatim, no ellipsis. ✓ (test 251-254)
  - >80 chars → `slice(0, 79) + "…"` = 79 chars + ellipsis. ✓ (test 246-249)
  - Unparseable args → `JSON.parse` throws → `return null` via the function's top-level try/catch (443-447). ✓ (test 263-265)

### 2. Bugs — none
- **No off-by-one.** `firstLine.length > BACKLOG_TITLE_MAX` (i.e. `> 80`, so ≥81) → `slice(0, BACKLOG_TITLE_MAX - 1)` = `slice(0, 79)`; verbatim when `≤ 80`. The chip body is therefore at most 80 chars in both branches (80 verbatim, or 79 + ellipsis = 80) — a clean, consistent ceiling. The `trimEnd()` after slicing cannot empty the result (the leading char is non-space after the initial `.trim()`), so `"…"`-only or empty chips are impossible.
- **`argPaths` exclusion is correct and well-placed.** `backlog_add`'s `text` is a label, not a file path; adding it to the early-return exclusion list (toolCardPaths.ts:55-71) prevents the truncated-args `salvagePathLiteral` path from ever synthesizing a bogus file-link chip for a backlog card. Consistent with shell/git/git_read/search/search_read already in that list.

### 3. Security — PASS
- The chip string is returned from `argLabel` and rendered as a React text node (Message.tsx:693, 881) — React escapes it; no `dangerouslySetInnerHTML`. This is the identical mechanism the already-shipped `search` branch uses to render arbitrary user-supplied `pattern` text, so no new injection surface is introduced. The `text` value is a JSON-parsed string inserted into a template literal; no HTML/eval.

### 4. Multi-platform neutrality — PASS
- Frontend-only TypeScript/TSX; no Windows/macOS-specific APIs, paths, or shell syntax. `split(/\r?\n/)` correctly handles both `\n` and `\r\n` line endings.

### 5. Docs sync — PASS (one optional observation)
- No README.md / PLAN.md update required for a ToolCard header label tweak — it is not a new feature, config key, or user-facing behavior change. The new code carries thorough inline doc comments (the `BACKLOG_TITLE_MAX` const doc and the branch comment citing backlog fef458f1 + the search convention).
- **Optional, non-blocking (low):** the `argPaths` doc comment (toolCardPaths.ts:41-43) lists label examples — "shell `purpose`, git `subcommand`, search `pattern`, spawn_agent `name`, skill `skill`" — and could add `backlog_add` `text` for completeness. This list is already illustrative/non-exhaustive (it omits `git_read` and `search_read`, both already excluded), so adding `backlog_add` is polish, not a requirement. Not counted as a finding.

### 6. Consistency — PASS
- The raw tool name `backlog_add` is intentionally kept (no `displayName` mapping), matching the shell/git/search convention (raw name + label chip). Only `spawn_agent` and `graph_*` have friendly displayNames. Confirmed consistent — the diff touches no displayName mapping.

### Verification
- Implementer ran `npx tsc --noEmit` (clean), `npx vitest run` (709 passed), `npm run build` (8s, green). The new `argLabel — backlog_add` block is a valid regression suite: the "short title quoted" case returns `null` before the branch existed (backlog_add previously fell through to the common path/file fallback, which returns null for a `text`-only args object) and the quoted title after — so it fails-without / pass-with the fix.

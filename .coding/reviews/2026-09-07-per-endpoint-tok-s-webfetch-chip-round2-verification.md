## Verdict: PASS

All four round-1 findings are correctly fixed in commit dcda445 (HEAD of wt/agenticcoding, clean working tree — final state = commit). No regressions found: the StatsView JSX is clean (the LOW 2 comment sits as a `//` comment above the map's `return (`), ModelTable renders one row per (model, endpoint) with the Endpoint column and a colSpan=2 totals label, and the keyboard fix composes correctly with the chip's click stopPropagation.

### Per-finding verification (all against commit dcda445; working tree clean, so file state = commit state)

**LOW 1 — README doc gap: FIXED.**
README.md:77 now reads "Tool-card file links open the file in the Files tab — except `file_edit` chips, which open that file's diff in the Diff tab; a `web_fetch` chip opens the fetched URL in the system browser" — exactly the extension the finding requested, in the natural home (the ToolCard bullet).

**LOW 2 — React key collision: FIXED.**
StatsView.tsx:127-131: the map callback carries a `//` comment above the `return (` explaining the collision rationale ("(model, endpoint) pairs are unique per the SQL GROUP BY, but naive concatenation can collide ("glm" + "5.3" vs "glm5.3" + NULL) — the JSON form is collision-free"), and the row key is `key={JSON.stringify([mb.model, mb.endpoint])}` — the collision-free form the finding recommended. `JSON.stringify` is deterministic and `undefined`/`null` endpoints both serialize to `null`, so the key is stable per row.

**LOW 3 — raw URL reaches the OS opener: FIXED.**
- `openExternal.ts:37` opens `parsed.href` (and :40 logs `parsed.href` on failure) — the normalized href, not the raw input, reaches the shell opener.
- `toolCardPaths.ts` `webFetchUrl` returns `parsedUrl.protocol === "http:" || ... === "https:" ? parsedUrl.href : null` — the normalized href, so the chip's click target and tooltip (`Open ${url} in your browser`) show exactly what a browser would resolve.
- `toolCardPaths.test.ts` webFetchUrl suite pins the normalization: bare host `http://example.com` → `http://example.com/` (trailing slash), `http:example.com` → `http://example.com/`, `https:\example.com` (single backslash after JSON.stringify) → `https://example.com/` — all matching WHATWG URL normalization for special schemes — alongside the pre-existing scheme-rejection cases (file/ftp/mailto/javascript/not-a-url → null) and the untruncated-vs-60-char-label decoupling.

**LOW 4 — keyboard activation suppressed: FIXED.**
Message.tsx:536-541: the header span's `onKeyDown` opens with `if (e.target !== e.currentTarget) return;` (with a comment naming the chip buttons and the preventDefault-cancellation hazard) before the Enter/Space check. A keydown on a focused chip no longer toggles expand nor cancels the chip's activation; a keydown on the span itself (target === currentTarget) still toggles. The fix composes correctly with the chip's `onClick` `stopPropagation()`: the keyboard-synthesized click on the chip button is stopped at the chip, so the header's click-toggle never fires either — both activation paths now open the link.

### Regression spot-check (checked and passed)

- **StatsView JSX final state is clean.** The briefly-broken JSX comment was repaired correctly: lines 124-130 place the rationale as a `//` comment inside the map callback body *before* `return (` — valid TSX, no stray `{/* */}` inside the return parentheses. Consistent with the stated green tsc/vitest matrix.
- **ModelTable structure.** thead: Model | Endpoint | Reqs | Prompt | Compl | Reason | Cached | In/s | Out/s | Cost (10 columns). Each body row: model cell + endpoint cell (`mb.endpoint ?? ""`, blank for NULL/pre-endpoint rows) + 8 numeric cells = 10. tfoot: `<td colSpan={2}>Total</td>` + 8 numeric cells = 10 — column counts consistent across header/body/footer. One row per (model, endpoint) is driven by the SQL `GROUP BY model, endpoint` in both `session_stats` and `project_stats` (mod.rs, SUM indices shifted 2-9 consistently in both), so the same model on two endpoints renders two rows with their own In/s + Out/s; NULL endpoints group into one row per model (SQLite NULL grouping) — the pre-dimension view.
- **Backend unchanged by the fixes and still sound:** turn.rs records `provider_name()` at the Usage event (empty → None); schema.rs adds `endpoint TEXT` to CREATE TABLE plus the idempotent `migrate_add_column_if_missing` with the legacy-DB regression test; the INSERT names all 11 columns positionally.
- **Tests:** `session_stats_splits_same_model_across_endpoints` (a100/b200/NULL → three rows, per-row sums, cross-endpoint totals) and `project_stats_splits_same_model_across_endpoints` (split across two sessions) both present in src/memory/tests.rs; the `record()` helper's call sites all pass the new `endpoint` param; the agent test covers both the None default and the `named("a100")` Some path end-to-end through `run_turn`.
- **Chip render order unchanged and conflict-free:** path branch → url branch → md branch; `backlog_add` chips get `url: null` (tool-name gate) so the md render is untouched, `web_fetch` chips get `md: false`. AboutDialog's private openExternal copy is deleted and imports the shared helper; its `void openExternal(...)` call sites are compatible with the boolean return.
- **Multi-platform neutrality:** no new platform-specific code in any of the fixes (URL normalization and the key/handler changes are pure web APIs); the shell open remains the Tauri plugin's cross-platform path.

### Test status
Reviewed against the stated green matrix (frontend vitest 1007 + tsc clean, root cargo test 2045+16, src-tauri 233+4); the reviewer toolset is read-only, so suites were not re-run. The LOW 3 fix added direct unit coverage for the normalization behavior (the only finding whose fix warranted new test assertions); LOW 1/2/4 are doc/key/handler-shape changes verified by inspection against the stated green tsc + vitest runs.

## Verdict: PASS

Review of the uncommitted changes on `wt/agenticcoding` implementing steps **R15** (file_read stale-2000 references) and **R17** (sane 32K max_output_tokens ceiling) of the R12–R17 cache-hit + context optimization batch. R12–R14 were committed in a prior session and are not in this diff.

**Files reviewed:** `src/tool/agent/file_read.rs`, `src/provider/openai.rs`, `src/provider/anthropic.rs` (plus the plan bookkeeping in `.coding/plans/6889482e.md`).

---

### R15 — `src/tool/agent/file_read.rs` (stale 2000 → 500 references)

The `DEFAULT_MAX_LINES` constant was already changed to `500` in a prior commit (confirmed at line 22). This diff fixes the two stale references that remained:

1. **Schema description** (line 67): `"Capped at 2000 lines / 100 KB by default"` → `"Capped at 500 lines / 100 KB by default"`. Correct — now matches the constant. The `max_lines` field description (line 82) already said "default 500", so the two are now consistent.

2. **`truncates_large_file_by_line_count` test** (lines 308–333): the file is 3000 lines (`line0`..`line2999`); with `DEFAULT_MAX_LINES=500`, `take(500)` yields lines 1–500 (1-indexed) = contents `line0`..`line499`.
   - `"showed 2000"` → `"showed 500"` ✓ (truncation note reads `"... (truncated: 3000 lines total, showed 500)"`).
   - `"2000: line1999"` → `" 500: line499"` ✓ — **correctly accounts for the `{:>4}` formatting change.** `format!("{:>4}", 2000)` = `"2000"` (4 chars, no padding), but `format!("{:>4}", 500)` = `" 500"` (3 digits → one leading space). The new assertion `" 500: line499"` (with leading space) is exactly what `format!("{:>4}: {}", 500, "line499")` produces. The test comment documents this. Good catch.
   - `"!line2000"` → `"!line500"` ✓ — line 501 (1-indexed) carries content `line500`, which is truncated and absent. No visible line (`line0`..`line499`) contains the substring `line500`, so the negative assertion is sound.

   Verified `read_files.rs` (the sibling tool, changed in the prior commit) is also clean: its `DEFAULT_MAX_LINES` is `500` (line 30) and a full-tree search for `2000` returns no hits in either `read_files.rs` or `file_read.rs`. No stale references remain.

### R17 — sane 32K max_output_tokens ceiling (`openai.rs` + `anthropic.rs`)

Both providers add an identical function-local constant and cap, mirrored correctly:

```rust
/// Absolute ceiling on output tokens regardless of endpoint config.
/// Most coding tasks need 2-8K; 32K is generous. Prevents wasteful
/// 128K output budgets on large-context endpoints where the R9
/// context-window cap never triggers.
const SANE_MAX_OUTPUT_TOKENS: usize = 32_000;
```

…inserted into the existing token-budget chain as `.min(SANE_MAX_OUTPUT_TOKENS).max(1024)` — i.e. the sane ceiling is applied **before** the final `.max(1024)` floor.

- **Placement verified (the key focus point):** the 1024 floor still applies when the context-window cap produces a value below 1024. Traced `max_completion_tokens_floors_at_1024_when_prompt_exceeds_context` (openai.rs:2742): `max_context=1000`, 10K-char prompt → `prompt_est=2504` → `.min(0)` → 0 → `.min(32000)` → 0 → `.max(1024)` → **1024**. The new `.min()` is a no-op on this path (0 < 32000) and the floor still raises it to 1024. ✓
- **Existing cap tests unchanged:**
  - `max_completion_tokens_uncapped_when_prompt_small` (openai, default 32K output) → `min(32000, 32000)=32000` ✓
  - `max_completion_tokens_capped_when_prompt_large` (openai, 8000 output, 10K context) → 7472 (below ceiling, unaffected) ✓
  - `max_tokens_uses_capability_budget` (anthropic, default 32K output) → 32000 ✓
- **New tests** (`max_completion_tokens_capped_at_sane_ceiling` in both files): `max_output_tokens=131_072`, `max_context=1_000_000` → context-window cap never triggers (1M − tiny − 1024 ≫ 131_072) → `.min(32000)` caps 131_072 → **32_000**. Assertion matches. ✓
- **No other test breaks:** a full-tree search for `max_output_tokens: Some(\d)` found only values ≤ 131_072; the only >32K values are the two new tests (intentional). Config tests in `endpoints.rs`/`patch.rs` (2048/8192/16384) test config round-tripping, not token computation, and are all below the ceiling.
- **Mirroring:** both providers use the same constant value (`32_000`), the same 4-line doc comment, the same `.min(SANE_MAX_OUTPUT_TOKENS).max(1024)` position relative to the existing chain, and structurally parallel tests. ✓

### Constitution checks

- **Warning-free under `#![deny(warnings)]`:** the constant is used in the `.min()` call (no dead-code warning); no unused imports or `mut` introduced; the new tests reference real fields. A green `cargo test` (per the plan's step 6) proves zero warnings. ✓
- **Multi-platform neutrality:** all changes are pure integer arithmetic + `serde_json` construction — no Windows APIs, paths, or shell syntax. ✓
- **Doc comments on public items:** the constant is function-local (private); the doc comment is a bonus, not required, and is present on both. The inline comments at each cap site explain *why* (large-context endpoints where R9 doesn't trigger). ✓
- **Documentation sync:** the README.md / PLAN.md mentions of `max_output_tokens` describe the *user-configurable endpoint field* (config examples), not the runtime ceiling — they are not stale. The 32K cap is an internal safety net documented inline at the code site; no user-facing doc enumerates it. The 500-line default is documented in the tool schema description (updated in this diff), which is the primary doc the model sees; README does not enumerate per-tool line caps, so no README change is needed. Historical `.coding/analysis/` and `.coding/plans/` references to "2000 lines" describe past observed behavior and are correctly left as historical record. ✓ No doc updates required.
- **Security:** the cap *reduces* output-token budgets (a cost/safety improvement against the wasteful 128K budgets the round-4 analysis flagged). 32K tokens ≈ 24K words is generous for any single coding response, and the agent loop splits long work across turns. No security concern. ✓

### Non-blocking observation (not a finding)

`SANE_MAX_OUTPUT_TOKENS` is declared as a function-local `const` in both `openai.rs` and `anthropic.rs` (duplicated). This matches the plan's explicit "function-local const" spec and keeps each provider self-contained — a common, acceptable Rust pattern for a one-line constant. A shared `pub(crate) const` in a common provider module would be marginally DRY-er, but is not worth a follow-up change. No action needed.

---

**Conclusion:** Both R15 and R17 are correctly implemented, fully mirrored across providers, and constitution-compliant. The `{:>4}` formatting edge case in the R15 test (leading space for 3-digit line numbers) was handled correctly. The R17 cap is placed before the 1024 floor so the floor is preserved, and all existing cap/floor tests pass unchanged. No findings.

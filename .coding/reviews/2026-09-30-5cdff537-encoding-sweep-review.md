## Verdict: FINDINGS (0 high, 2 low)

Reviewed the uncommitted diff for plan 5cdff537 against the plan record at `.coding/plans/5cdff537.md` (read in full), the shipped guard, and every changed region read directly in the working tree. Overall this is a clean, well-evidenced sweep: all six class-1 needles are byte-identical to the image_tools guard, all 15 repairs carry the correct glyph and read naturally, every fixture replacement preserves its test's property, and the walk is non-vacuous with a useful failure report. Two low findings, both in the new guard.

### What was verified

**1. Guard integrity — `tests/integration/encoding_guard.rs`**
- **Needle fidelity:** each of the six CLASS1 escape tuples was cross-checked character-for-character against the shipped guard in `src/tool/agent/image_tools/mod.rs:371-376` (em dash, arrow, box drawing, en dash, greater-or-equal, CJK hint) — all six byte-identical.
- **Class-2 signature:** U+00CE as UTF-8 (`0xC3 0x8E`) followed by a non-ASCII byte is sound for the observed family: in the CP437-then-cp1252 chain, any original char whose first UTF-8 byte is `0xE2` (all general punctuation — dashes, arrows, quotes, ellipsis) maps through CP437 Γ (0xE2) whose UTF-8 lead is `0xCE`, surfacing as `Î“…` in round two. I re-derived the em-dash chain independently (E2 80 94 → ΓÇö → Î“Ã‡Ã¶) and it matches the plan's machine-confirmed table. In valid UTF-8, `C3 8E` can only be U+00CE, so no cross-boundary false positives.
- **No self-match:** the guard's own source is pure ASCII (all needles as `\u{...}` escapes, the class-2 lead as hex bytes), so scanning `tests/` cannot flag itself — confirmed by reading the whole file.
- **Non-vacuity + scoping:** per-root `!files.is_empty()` assert, 100-file floor, skip list (target/node_modules/.git/dist/.worktrees/vendor/.coding), nine text extensions, binary skip via NUL in the first 8 KB. Failure message names repo-relative file, line (LF count before offset + 1 — correct for this LF-enforced repo), and class label. RED→GREEN history (exit 101, exactly 15 findings, all `src/agent/tests.rs` at the recorded lines; green post-fix) is credible and internally consistent with the step-1 sweep inventory.
- `mod encoding_guard;` registered in `tests/integration/main.rs` in the existing alphabetical plain-mod style.

**2. The 15 repairs — `src/agent/tests.rs`**
Read all 15 lines (1278, 2273, 2325, 2448, 2787, 2889, 2950, 2984, 3216, 3232, 4781, 4829, 4872, 4943, 4944). Em dash (x11), arrow (x3), double arrow (x1) each match the recorded decode chain and every sentence reads naturally in context (e.g. :2950 "fill_rate 0.5 → summarize_at = 50", :2984 "8+ messages ⇒ over 50", :4943-44 "multimodal=false → … multimodal=true → true").

**3. Fixture replacements — property preservation, no behavior change beyond strings**
- `knowledge.rs:1280` `slug_for("\u{e9}\u{e8}\u{ea}")` → `"2026-08-23-record"`: all-non-ASCII letters, exercises the no-ASCII fallback, in the file's existing escape style; the :1276 mixed case still covers the separator behavior.
- `convert_line_endings.rs:407/:413` `café\r\ntail-no-newline`: multibyte content-preservation round trip intact, expectation string matches.
- `agent/mod.rs:103` `"→".repeat(70_000)` (~210 KB of a 3-byte char) with the comment updated — mid-char-cut property preserved.
- `read_files.rs:1158` `"→→→".repeat(20_000)` (~180 KB > 100 KB cap) with the matching `contains("→→→")`; the tempdir filename is consistently `multibyte.txt` in both write and execute (it is an inline tempdir fixture, not a repo file — nothing to rename in git); no leftover `cjk.txt` reference anywhere outside the plan record.
- `web_fetch.rs:525` `"→".repeat(20_000)` (60 KB, cap 50 KB) — byte-cap property preserved.
- `console.rs:1878` `truncate("→→→→→→", 3) == "→→…"` — all-multibyte, char-safe truncation, expectation matches the ASCII sibling case's pattern (limit 3 → 2 chars + ellipsis).
- `image_tools/tools.rs:306` schema description now English (`'zh' or 'Chinese'`); repo-wide searches confirm neither the old nor new string appears in README/PLAN/CHANGELOG/docs — no doc update needed.

**4. Collateral:** diff touches exactly the ten source files + `.coding` records; no Cargo.toml/Cargo.lock change, no new dependency, no `cfg(windows)` additions (the guard is portable `std::fs`), file-tools-first honoured per the plan records.

**5. Formatting:** every changed line I read is rustfmt-conformant (including the reflowed `knowledge.rs` assert, `read_files.rs` chain, and the guard's wrapped 6-escape tuple); the guard file is fully clean. The repo-wide `cargo fmt --check` drift named in the plan is consistent with pre-existing state and no flagged file's *changed lines* intersect this diff. (I cannot run cargo/rustfmt as a read-only reviewer; this is a by-inspection verification consistent with the plan's step-5 record.)

**6. Bookkeeping accuracy (one line):** the plan record's step evidence (line numbers, glyph counts, exit codes, fixture text) matches what actually shipped in every spot I cross-checked; the residual CJK/`cjk.txt` mentions live only in `.coding/**` records, which quote them deliberately.

### Findings

**LOW 1 — class-2 doc comment overclaims coverage (`tests/integration/encoding_guard.rs:10-13`).** The comment says *"Every character damaged this way starts with U+00CE"* — true only for originals whose first UTF-8 byte is `0xE2` (the punctuation family that occurred). A CP437-then-cp1252 mangle of Latin-range text starts elsewhere: e.g. é (C3 A9) → CP437 ├⌐ → UTF-8 → cp1252 → `â"œâŒ`, which is U+00E2-led, matches none of the six fixed class-1 needles, and slips past the Î signature. Fix: either soften the comment to state the covered family (punctuation/`0xE2`-led originals, which is all this repo ever contained) or, better, extend the generic signature with U+00E2 followed by a non-ASCII byte as a second class-2 lead — that generically catches the remaining CP437+cp1252 variants (and would subsume most class-1 needles) with negligible false-positive risk in this English codebase. If extended, re-run the guard to confirm it still passes (it should — the sweep found zero).

**LOW 2 — read errors are silently skipped (`tests/integration/encoding_guard.rs:124`).** `std::fs::read(&file).unwrap_or_default()` turns an unreadable file (permission, race) into an empty buffer that is skipped — a file the guard cannot read is a file it does not guard, and unlike the binary skip this is not deliberate. Fix: `std::fs::read(&file).unwrap_or_else(|e| panic!("encoding guard cannot read {}: {e}", file.display()))` (or `.expect` with the path) so a broken read fails loudly instead of shrinking coverage behind the same 100-file floor.

Both findings are guard-hardening nits; neither invalidates the sweep, the repairs, or the fixture replacements. Re-run `cargo test --test integration` after addressing them.

Reviewed-state: 0c4826d107c8d5e351f812a0e3af7c19c1d44f61

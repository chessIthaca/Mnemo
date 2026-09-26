## Verdict: PASS

Verification round for plan aff95a51 ("Tighten empty-call descriptors + wire the graph_search → graph_context pointer"), confirming the L1 fix from the first-round report (.coding/reviews/2026-09-23-aff95a51-empty-call-descriptors-graph-pointer-review.md). Focused re-check, not a full re-review.

### 1. L1 fix — RESOLVED

**Restored sentence reads naturally in place.** `src/tool/agent/shell.rs:396-414` renders as: `Always pass `command` + `purpose` — e.g. {"command":"cargo test","purpose":"running tests"}. No zero-argument form; on a required-field error rewrite the full call, do not resend the empty shape. If you just made a shell call, the next one needs its own command. Execute a shell command — PowerShell 5.1 on Windows, sh on Unix. …` The hint sits as the leading substance sentence right after the shared contract — exactly the placement the finding suggested — and flows into the pre-existing substance without a seam.

**No other substance lost or altered.** Diffed the old shell description against the new one sentence by sentence: "Execute a shell command — PowerShell 5.1 on Windows, sh on Unix", "Requires approval", the `;`-not-`&&`/`||` chaining guidance, the success/exit-code semantics, the ~100 KiB cap + truncation note, noise filtering, the data-field note, and the timeout kill — all retained verbatim. The only delta vs. the pre-fix text is the hint's restoration (parenthetical → full sentence, same wording minus the "if" wrapper). The error-path hint swap to `tool_contract::recovery_hint("command + purpose")` is unchanged from round one.

**Pin assertion is non-vacuous.** `schema_description_names_the_calling_traps` (shell.rs:1181-1188) asserts `description.contains("the next one needs its own command")` — an exact substring of the restored text (the `\`-continuation at shell.rs:397-398 joins to precisely that string), with a comment citing review L1. If the sentence were dropped again, `contains` fails and the test message prints the full description — it cannot pass vacuously. The surrounding pins (lead-with-contract, no "If you catch yourself", "No zero-argument form", chaining warning, inline example, "do not resend the empty shape") are all still present and still assert the fixed text.

### 2. Spot-check of the uncommitted diff — no regression

Changed set is exactly as expected, nothing extra: `src/tool/agent/tool_contract.rs` (NEW), `mod.rs` (one-line `pub mod tool_contract;`), `codegraph.rs`, `read_files.rs`, `shell.rs` (now including the L1 fix), `git_read_tool.rs`, `memory/mod.rs`, plus `.coding` bookkeeping (backlog.jsonl: two done items gaining `deleted_at`, one new pending item — state bookkeeping only), the untracked `.coding/plans/aff95a51.md` / knowledge HOW record / first-round review, and the pre-existing `.npmrc` + `start.bat` environment fixes (assessed sound in round one; unchanged since). No source file outside the expected set is touched; the graph-pointer code, the flipped negative pins, and the drift-guard test are byte-identical to what round one verified. Tests were re-run green after the fix per the fix report (2522 passed / 0 failed / 5 ignored lib + 19 integration + 1 doc-test, exit=0; `#![deny(warnings)]` at both crate roots makes green == warning-free).

### Conclusion

The single L1 finding is resolved with the suggested fix (restore + pin), nothing regressed, nothing new surfaced. Ready to commit.

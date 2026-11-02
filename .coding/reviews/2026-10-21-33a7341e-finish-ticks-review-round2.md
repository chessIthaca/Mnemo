## Verdict: PASS

Round 2 of plan 33a7341e ("Finish ticks appended review-fix steps") on wt/mnemo. Scope: the delta since base `6219ec93268d04fa0d346389635c1a21b7f8beea` — the uncommitted working tree (no commits since the base; `git log` confirms 6219ec9 is HEAD, and the round-1 fixes ride the same uncommitted change as the round-1 feature). Round 1 returned FINDINGS (0 high, 3 low); every finding's fix is verified present below.

Read: the round-1 report, `.coding/plans/33a7341e.md`, `git_read diff` (stat + full), and the changed code in context — `src/app/workflow/plan_file.rs` (`tick_checkbox_line`:21-35, `complete_all_steps`:250-270, `persist_completed_marks`:307-348, unit tests :770-864), `src/app/workflow/mod.rs` (`finish`:1066-1095), `src/app/tool/workflow/plan.rs` (FinishTool cost-report append :2446-2477 + state re-check :2479-2490, regression test :4809-4881, update_plan schema :1095-1112), `src/agent/prompt.rs` (:78-82, GOLDEN re-pin :971-977), and `parse`'s section handling (`src/memory/plan_format.rs`:204-245) for symmetry.

### Round-1 findings — all three fixed and verified

1. **L1 fixed — section-scoped flip.** `persist_completed_marks` (plan_file.rs:321-334) now tracks the current `## ` header per line (`content.trim_start().strip_prefix("## ")`, `trim_end` tolerated) and sets `in_step_section` only for exactly `Steps` / `Detailed steps`; `tick_checkbox_line` runs only when the flag is true. Goal/Context literals stay untouched. New test `persist_completed_marks_skips_checkbox_literals_outside_steps` (:844-864) puts a `- [ ]` literal in both Goal and Context and asserts both survive verbatim while the real step flips — genuinely red against the pre-fix whole-file flip (it would have flipped both literals). The parser itself (`plan_format.rs:217-243`) treats any `## ` line as a section switch, so a `## Steps`-like line inside Context prose already degrades the parse the same way — the tracker is symmetric with the parser, no divergence possible.
2. **L2 fixed — retry-after-IO-error healed.** `Workflow::finish` (mod.rs:1082-1090) now calls `complete_all_steps()` and then `persist_completed_marks` **unconditionally** (the flipped-count gate is gone, with a comment naming review L2). A retried finish after a transient write error re-persists even though nothing flips; the flip is idempotent and byte-preserving (verified below), so the retry heals the frame.
3. **L3 fixed — fallback covered.** `persist_completed_marks_preserves_appended_sections` (:834-840) now removes the frame file, calls persist, and asserts the `write_to_dir` fallback recreates it fully ticked (`!contains("- [ ]")`, `is_complete()`). The CRLF sub-case (:828-833) now carries a `## Steps` header (required by the scoping) and still asserts **byte-exact** CRLF preservation (`"## Steps\r\n- [x] 1. a\r\n- [x] 2. b\r\n"`).
4. **Nit (a) fixed —** `tick_checkbox_line` (:30-31) now splits at `]` and re-emits everything after it verbatim, so a bare `- [ ]` becomes `- [x]` with **no** injected trailing space; the round-1 doc comment (:27-29) states this.

### Round-1 acceptance — still holds end to end

- `finish_ticks_appended_review_fix_steps` (plan.rs:4809-4881): after FinishTool runs on a Reviewing plan with two appended steps, no line (indented or not) starts with `- [ ]`, both appended step texts survive, and the in-memory frame agrees (`is_complete()`, `completed_count() == 4`). `Workflow::finish` ticks via `complete_all_steps` then persists before the state flip — all three hold by construction.
- **Cost-report ordering hazard safe under the unconditional persist.** FinishTool appends `## Cost report` at :2446-2477 *before* `wf.finish()` at :2490; the `## Cost report` header line clears `in_step_section`, so the report's bytes (including any checkbox-shaped text inside it) survive verbatim. Byte-preservation verified by construction: `split_inclusive('\n')` with `\r\n`/`\n`/EOF terminator reconstruction makes a no-flip rewrite byte-identical to the input — the newly unconditional persist in the already-ticked cost-report tests (plan.rs:5015) cannot regress them. The unit test also appends a report manually before persist and asserts it survives (:810-824).
- **Sole transition + state gate unchanged.** `finish` still errors outside Reviewing (mod.rs:1067-1072) and remains the only Reviewing→Complete path; FinishTool re-checks state after the unlocked window (:2483-2489). The finish-side tick (mod.rs:1073-1081) and the doc comments on `finish`/`complete_all_steps`/`persist_completed_marks` (which now state the section scoping at :316-320) are the only changes to this path.

### New-code edge cases (all safe)

- `## Steps`-like line in Context prose: flips the tracker, but the parser treats that line identically as a section switch — symmetric, no disk/parse divergence (see L1 above).
- Detailed steps present/absent: `complete_all_steps` skips via `if let Some` (:256); the tracker never enters step scope without a `## ` header, so a header-less hand-authored file is conservatively left untouched.
- `## Cost report` / `## Bug` / `## Reviews` headers all clear the flag; an h3 (`### x`) inside Detailed steps does not match `## ` and does not leave the section — matches the parser.
- CRLF: header lines reach the tracker with `\r` already stripped by the terminator handling (:324-330); `trim_end` tolerates trailing whitespace on headers.
- Empty-text `- [ ]` and `- [ ]x` (no space): handled verbatim / left untouched (`parse_checkbox` → `None`).
- One informational nit (not counted): the tracker matches `Steps`/`Detailed steps` case-sensitively while the parser lowercases (`plan_format.rs:218`) — a hand-edited `## steps` header parses as the section but would not be flipped by persist. `serialize()` always writes canonical headers, so this cannot occur in app-written frames; harmless.

### Constitution checks

- **Documentation sync** — done: `update_plan` schema text (plan.rs:1105-1107), the REVIEWING bullet in the stable head (prompt.rs:78-82) with the GOLDEN_HEAD re-pin (:971-977) commented and plan-named, and the method doc comments including the new section-scoping note. PLAN.md/README unaffected (internal bookkeeping).
- **Multi-platform neutrality** — clean: `std::fs` + `Path` joins only, explicit LF/CRLF handling is platform-neutral by design; no `cfg(windows)`, no Windows paths or shell syntax.
- **File-tools-first** — clean: `std::fs::write` in app code matches the existing `write_to_dir`/cost-report idiom (the policy governs agent-side edits, not app code). No `#[allow(...)]` anywhere in the delta.
- **Warning-free build** — cannot be re-verified read-only (no shell); no `#[allow]` and no obvious dead code/unused imports in the delta; the parent's `cargo test --workspace` gate covers it.

### Bookkeeping accuracy (one line)

`.coding/plans/33a7341e.md` matches the shipped state: 4/5 steps ticked, step 5 (`- [ ] 5. Fix review round 1 (L1-L3)`) is the appended review-fix step — intentional, and `finish` will tick it (it lives inside `## Steps`, the scoping's exact target); regression_test recorded, round stamps 1-2 at the base sha; the other `.coding/` deltas (`backlog.jsonl` +1, `8813e7ab.md` +7 own cost report, the chat-tool-cards SPEC file) are prior-plan bookkeeping, accurate.

### Process remark (one line)

The entire round-1-verified feature remains uncommitted alongside the round-2 fixes (no commits since the base) — expected per the dispatch note; the closing-sequence commit will carry both.

## Verdict: PASS

Reviewed-state: 6219ec93268d04fa0d346389635c1a21b7f8beea

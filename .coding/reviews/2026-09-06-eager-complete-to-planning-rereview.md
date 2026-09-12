## Verdict: PASS

Re-review of the "eager Complete → Planning transition" change
(`src/agent/prompt.rs`, `src/agent/turn.rs`) after fixing the 3 low-severity
findings from the first review
(`2026-09-06-eager-complete-to-planning-review.md`). All three findings are
genuinely fixed, no new issues were introduced, and the change remains
correct, secure, and constitution-compliant. It ships.

The changes are committed at `f34d527` (working tree clean); the diff reviewed
is `git show f34d527`.

---

### Finding 1 (Low) — FIXED: exact-or-suffix matching replaces raw `starts_with`

`src/agent/prompt.rs:276-300`. `looks_like_work_intent` now matches a stem
against a verb via exact equality OR `strip_prefix(verb)` whose remainder is
in a `SUFFIXES = ["ing", "ed", "es", "s", "d", "er", "ers"]` array.

Traced each false positive from the original finding through the new logic:

| word      | verb tried | `strip_prefix` remainder | in SUFFIXES? | result |
|-----------|------------|---------------------------|--------------|--------|
| address   | add        | "ress"                    | no           | false ✓ |
| wireless  | wire       | "less"                    | no           | false ✓ |
| movement  | move       | "ment"                    | no           | false ✓ |
| fixture   | fix        | "ture"                    | no           | false ✓ |

The new match set (exact OR verb+suffix) is a **strict subset** of the old
`starts_with` set — `verb+suffix` implies `starts_with(verb)` — so the fix can
only *remove* false positives, never introduce new ones. No regression risk.

The new test `looks_like_work_intent_excludes_false_positives_from_prefix_match`
(prompt.rs:861-876) pins all four cases plus the hyphenated "fix-the-bug" token
(a single whitespace token whose stem "fix-the-bug" matches no verb+suffix — a
documented, acceptable false negative that falls back to `STATE_COMPLETE`).

The `SUFFIXES` array is well-formed: `"ers"` is not redundant with `"er"`
because matching is an exact `contains` check — "builders" → remainder "ers"
(not "er"), so both entries are needed. The doc-comment claim that "fixing",
"added", "updated", "builder" all match is accurate (verified: fix+ing, add+ed,
update+d, build+er).

### Finding 2 (Low) — FIXED: English-only limitation documented

`src/agent/prompt.rs:273-275`. The `looks_like_work_intent` doc comment now
carries: "English-only: the verb list is English, so CJK and other non-Latin
scripts are false negatives (they fall back to `STATE_COMPLETE`, which still
directs the agent to plan)." Accurate and well-placed — the limitation is now
explicit rather than implicit.

### Finding 3 (Low) — FIXED: punctuation-stripping path is directly tested

`src/agent/prompt.rs:856-858`. Three assertions added to
`looks_like_work_intent_fires_on_imperative_verbs`, each exercising the
`trim_matches(|c: char| !c.is_alphanumeric())` path (prompt.rs:290) in a
distinct way:

- `"fix the bug in login.rs!"` — sentence-final `!` sits on a later token;
  "fix" is a clean token → exact match.
- `"Please (refactor) the parser."` — "(refactor)" trims leading `(` and
  trailing `)` → "refactor" → exact match.
- `"Fix, then test."` — lowercased "fix," trims trailing `,` → "fix" → exact
  match.

A regression that narrowed the trim (e.g. switching to
`trim_end_matches(|c| c.is_ascii_punctuation())`) would now be caught.

---

### Re-verified clean (carried from first review, still holds)

- **Borrow/lifetime soundness (turn.rs:674-683).** `latest_user_msg` is
  `Option<String>` — owned, because `MessageContent::as_text()` returns an
  owned `String` (confirmed at `src/provider/mod.rs:196`, which `clone()`s the
  text). So the immutable `messages.iter().rev().find(...)` borrow is fully
  released before `append_plan_nudge(&mut volatile_tail, …)` takes the mutable
  borrow of the local `volatile_tail`. No aliasing with `messages` or the held
  `wf` lock. `wf.state()` is taken by value (`WorkflowState: Copy`, confirmed
  at `src/workflow/mod.rs:24`), so no borrow of `wf` is retained across the
  mutable call. Compiles cleanly.
- **Firing conditions.** Nudge fires only when `state == Complete` AND
  `user_message.is_some()` AND `looks_like_work_intent(msg)`
  (prompt.rs:316-323). `WorkflowState` derives `PartialEq` (mod.rs:24), so the
  `==` comparison is sound.
- **Security.** Prompt-only. The user message is read solely to produce a
  `bool`; the appended text is the fixed `PLAN_NUDGE_TEXT` const. User content
  is never interpolated into the system prompt — no prompt-injection vector.
- **Doc comments.** `append_plan_nudge` (`pub(crate)`) and
  `looks_like_work_intent` (private) both carry `///` doc comments; `STATE_COMPLETE`
  and `PLAN_NUDGE_TEXT` consts are documented. Satisfies "All public functions
  must have doc comments."
- **Warning-free.** All new items (`PLAN_NUDGE_TEXT`, `looks_like_work_intent`,
  `append_plan_nudge`, the `work_verbs` array, the `SUFFIXES` const) are
  referenced. No unused imports, dead code, or stray `mut`. (Could not run
  `cargo test` directly — read-only reviewer with no shell — but static analysis
  found no warning source and the reported green build under
  `#![deny(warnings)]` is consistent.)
- **Multi-platform neutrality.** Pure `&str` processing (`to_lowercase`,
  `split_whitespace`, `trim_matches`, `strip_prefix`); no platform-specific
  APIs, paths, or shell syntax.
- **Documentation sync.** No `README.md` / `PLAN.md` update required: this is
  an internal agent-behavior refinement (prompt directive + volatile-tail
  heuristic), not a user-facing or user-configurable feature. No `endpoints.toml`
  or config examples touched.

### Test coverage

9 tests total (prompt.rs:807-942), covering: fires on imperative verbs +
-ing/-ed stems; excludes the prefix-match false positives (Low 1); does not
fire on how/where/what questions, empty string, or conversational filler;
exercises the punctuation-stripping path (Low 3); skips outside Complete; skips
with no user message; pins the enhanced `STATE_COMPLETE` directive language.
Coverage is sufficient — the three fixed paths each have a dedicated,
fail-without-the-fix assertion.

No findings.

## Verdict: FINDINGS (0 high, 3 low)

Review of the "eager Complete → Planning transition" change
(`src/agent/prompt.rs`, `src/agent/turn.rs`). The change is **correct, secure,
and constitution-compliant** — it ships. The three findings below are
low-severity refinements/coverage gaps, not blockers.

### What was verified clean

- **Borrow/lifetime soundness (turn.rs:674-683).** `latest_user_msg` is
  `Option<String>` (owned, because `MessageContent::as_text()` returns an owned
  `String` at src/provider/mod.rs:196), so the immutable `messages.iter()`
  borrow is released before `append_plan_nudge(&mut volatile_tail, …)` mutably
  borrows the local `volatile_tail`. No aliasing with `messages` or the held
  `wf` lock. `wf.state()` is taken by value (`WorkflowState: Copy`), so no
  borrow of `wf` is retained. Compiles cleanly.
- **Firing conditions are correct.** Nudge fires only when
  `state == Complete` AND `user_message.is_some()` AND
  `looks_like_work_intent(msg)` (prompt.rs:297-304). Verified against the
  `ToolFilter::Complete` definition (src/tool/mod.rs:217: "read agent tools +
  `create_plan` + all memory tools") — so the prompt's "explore with read
  tools, then call create_plan" guidance is accurate, not misleading.
- **No spurious firing on tool-result-only turns.** `find(Role::User)` scans
  back to the latest User-role message; a turn whose last message is a `Tool`
  result keys off the original user prompt. If that prompt was a pure question
  (no work verb) the nudge does NOT fire; if it was a work-verb task the
  re-fire is benign (the volatile tail is rebuilt each iteration, so the nudge
  does not accumulate — it just re-applies the same ~40-token directive while
  the agent explores before calling `create_plan`, which is the desired
  pressure).
- **Security.** Prompt-only. The user message is read solely to produce a
  `bool`; the appended text is the fixed `PLAN_NUDGE_TEXT` const. User content
  is never interpolated into the system prompt — no prompt-injection vector.
- **Doc comments.** `append_plan_nudge` (`pub(crate)`) and
  `looks_like_work_intent` (private) both carry `///` doc comments, satisfying
  "All public functions must have doc comments." `STATE_COMPLETE` and
  `PLAN_NUDGE_TEXT` consts are documented too.
- **Warning-free.** No unused imports, dead code, or stray `mut`. All new
  items (`PLAN_NUDGE_TEXT`, `looks_like_work_intent`, `append_plan_nudge`, the
  `work_verbs` array) are referenced. (Could not run `cargo test` directly —
  read-only reviewer with no shell — but static analysis found no warning
  source and the reported green build under `#![deny(warnings)]` is
  consistent.)
- **Multi-platform neutrality.** Pure `&str` processing; no platform-specific
  APIs, paths, or shell syntax.
- **Test coverage** is solid for the key behaviors: fires on imperative verbs +
  -ing/-ed stems; does not fire on how/where/what questions, empty string, or
  conversational filler; skips outside Complete; skips with no user message;
  pins the enhanced `STATE_COMPLETE` directive language.

---

### Low 1 — Short work-verbs match many unrelated words via `starts_with`

`src/agent/prompt.rs:279` — `stem == *v || stem.starts_with(v)`.

The verb list (prompt.rs:271-276) includes several 3-4 letter verbs (`add`,
`fix`, `move`, `hook`, `wire`). Because matching is a raw prefix test, common
non-work words trigger the nudge:

- `add` → **address**, addition, additional, addict, addendum
- `wire` → wireless, wiring
- `move` → movement
- `fix` → fixture, fixate

`address` is the standout — it is frequent in coding contexts ("what's the
memory address?", "address this concern", "email address"). A pure question
containing "address" fires the nudge, which then says "looks like a
code-change task … Do NOT answer freeform" — a mild contradiction with the
`STATE_COMPLETE` line "Only answer freeform for pure questions (how/where/what)"
(prompt.rs:246-247).

This is **tolerable by the stated design** (the nudge is a suggestion, not a
forced transition; a capable model reconciles via `STATE_COMPLETE`), so it is
Low, not a bug. If the false-positive rate proves noisy in practice, consider
matching the stem against the verb OR verb+common inflection suffixes (e.g.
`stem == v` / `stem == format!("{v}ing")` / `…ed` / `…s` / `…d`), or requiring
the char following the verb prefix to be a plausible inflection char — which
would still catch "fixing/added/updated" while excluding "address/wireless".

### Low 2 — Heuristic is English-only (CJK / non-Latin work intent is a false negative)

`src/agent/prompt.rs:269-281`. `to_lowercase()` + `is_alphanumeric()` are
Unicode-aware (no panic on CJK/emoji — `split_whitespace` yields the token,
`trim_matches` keeps Unicode letters, no verb matches, returns `false`), but
the verb list is English-only. A user message like "修复登录的 bug" or
"バグを修正して" will not trigger the nudge even though it is clearly a
code-change task.

Acceptable (falls back to the enhanced `STATE_COMPLETE` prompt, which still
directs the agent to plan), but worth a one-line note in the
`looks_like_work_intent` doc comment so the limitation is explicit rather than
implicit.

### Low 3 — Punctuation-stripping path (`trim_matches`) has no direct test

`src/agent/prompt.rs:278` — `let stem = word.trim_matches(|c: char| !c.is_alphanumeric());`

This is the subtle correctness path that lets "fix," / "fix!" / "(fix)" match
the verb `fix`, and it is exercised only indirectly (the test inputs at
prompt.rs:825-833 happen to be punctuation-free). A regression that narrowed
the trim (e.g. switching to `trim_end_matches(|c| c.is_ascii_punctuation())`)
would not be caught. Suggest adding 1-2 assertions to
`looks_like_work_intent_fires_on_imperative_verbs`, e.g.

```rust
assert!(looks_like_work_intent("fix the bug in login.rs!"));
assert!(looks_like_work_intent("Please (refactor) the parser."));
```

and a hyphenated case (`"fix-the-bug"` currently matches via `starts_with`,
which is worth pinning).

---

### Documentation sync

No `README.md` / `PLAN.md` update is required: this is an internal
agent-behavior refinement (prompt directive + a volatile-tail heuristic), not
a user-facing or user-configurable feature. The new `STATE_COMPLETE` /
`PLAN_NUDGE_TEXT` consts and the two functions all carry doc comments. No
`endpoints.toml` or config examples are touched. (If a design doc elsewhere
describes the Complete-state behavior as "terse / LLM-judged only," it would
warrant a one-line update — none was found in the changed files.)

## Verdict: PASS

No findings requiring changes. Two system-prompt text edits + three co-located test assertions in `src/agent/prompt.rs` are correct, cache-stable, platform-neutral, and warning-free. The chaining-instruction bounding is sufficient (assessment below). Side-car bookkeeping (`.coding/backlog.jsonl`) and the accompanying knowledge SPEC/DECISION files are appropriate.

---

## Scope reviewed

All uncommitted changes on `wt/agenticcoding` for plan 2fa7d3de:

- `src/agent/prompt.rs` (+24/-3) — the substantive change:
  1. New "Chain consecutive obvious steps" bullet in `CODING_SYSTEM_PREAMBLE` (L34-38).
  2. Cadence clause appended to the never-resend rule in `APP_RULES` (L107-109).
  3. Two assertions in `preamble_carries_core_working_principles` (L747-756).
  4. One assertion in `stable_head_contains_universal_closing_rules` (L799-804).
- `.coding/backlog.jsonl` — marks the "evaluate why agent stops mid-task" item done; removes a stale `failed` item about the finish↔update_plan deadlock (already fixed/merged per memory a6e5e562). Benign side-car bookkeeping.
- Untracked: `.coding/knowledge/spec/...`, `.coding/knowledge/decision/...`, `.coding/plans/2fa7d3de.md` — normal workflow artifacts; the SPEC + DECISION records are the correct documentation home for this internal prompt change.

Full suite: 1705 passed, 0 failed, 16 ignored.

---

## Correctness

**Edit placement — correct.** Both edits land in compiled `const` blocks that `build_stable_head` (L389-411) pushes into the cached head: `CODING_SYSTEM_PREAMBLE` (L26) and `APP_RULES` (L97). Confirmed by reading `build_stable_head` — it pushes PREAMBLE → LIFECYCLE → APP_RULES → TOOL_STRATEGY → MEMORY_RECORDS → constitution. The preamble bullet sits between "Complete one step at a time" and "Read before you write," matching the plan's stated location. The cadence clause extends the existing never-resend bullet in place.

**String-continuation syntax — correct.** Rust `\`-line-continuation strips the backslash, the newline, *and* leading whitespace on the next line, joining the segments into one logical line. This matches the existing style throughout both consts (e.g. L40-41, L66-69, L104-105). Rendered strings:

- Preamble bullet → `...needs no user input and no decision, take it in the same turn (retry a failed call with corrected args, re-run a test after a fix, continue to finish after a commit). Tool results are continuations of your turn, not new questions — keep going until you hit a real choice point or need the user.`
- APP_RULES clause → `...fix the named parameter, re-issue corrected immediately — in the same turn, without pausing to narrate the error first.`

**Test/assertion match — exact.** All three assertions use ASCII substrings that verbatim appear in the rendered strings:
- `"Chain consecutive obvious steps"` ✓
- `"continuations of your turn, not new questions"` ✓
- `"without pausing to narrate"` ✓

The assertions use `head.contains(...)` (substring), not length/count/snapshot checks — a `grep` for `preamble.*len|stable_head.*len|snapshot` in the test module returned no length or count assertions, so the added bullet cannot silently desync any structural test. The two modified tests are the only ones that assert on this content; the suite's 1705/0 result confirms no other test broke.

**Em-dash (`—`, U+2014)** is used consistently with pre-existing usage in both consts (e.g. L100 "Never commit to main — commit..."); Rust sources are UTF-8 by default. No escaping issue. The assertions deliberately avoid the em-dash (ASCII substrings), so there is no encoding coupling.

---

## Bugs

None found. No broken continuations, no assertion/prompt mismatch, no stray escape sequences, no unused imports or dead code introduced (the change is string-literal + assertion additions only).

---

## Constitution

- **Documentation sync — satisfied.** These are internal system-prompt edits to the agent's own working principles, not a user-facing feature, config surface, or provider strategy. `README.md` (feature/config docs) and `PLAN.md` (technical decisions/provider strategy) need no update. The appropriate documentation — a SPEC (four-mitigation catalog) and a DECISION (mitigation evaluation) — was written to `.coding/knowledge/`. Module doc comments remain accurate: the PREAMBLE doc ("carries only the core working principles") still describes the block correctly (the chaining bullet *is* a core working principle); the APP_RULES doc lists "the never-resend-failed-call rule," which the cadence clause extends in place rather than contradicts.
- **Multi-platform neutrality — satisfied.** Both edits are pure prose with no platform APIs, paths, or shell syntax. Builds and behaves identically on macOS and Windows.
- **Warning-free build — satisfied.** The crate roots carry `#![deny(warnings)]`; a green `cargo test` (1705 passed) is sufficient proof of zero warnings. No `#[allow(...)]` introduced.

---

## Cache stability

**Confirmed cache-friendly.** Both edits are in compiled `const` blocks inside `build_stable_head`, which the doc comments (L52, L90) explicitly designate the **stable head** — "byte-stable across turns." Neither edit introduces per-turn/volatile content: no timestamps, no workflow-state interpolation, no dynamic recall. The constitution (`agent.md`) is re-read each turn and appended *after* the compiled consts (L398-408), but the new bullet lives in the compiled `CODING_SYSTEM_PREAMBLE` const, not in `agent.md`, so it does not shift the cache boundary. The volatile tail (`build_volatile_tail`) and `CONTEXT_FOOTER` are untouched.

---

## Side-effects: is the chaining bounding sufficient?

**Assessment: sufficient.** The bullet is bounded on both ends:

- **Trigger (when to chain):** "when the next action needs no user input and no decision."
- **Stop (when to yield):** "keep going until you hit a real choice point or need the user."
- **Examples are all benign and in-task:** retry a failed call with corrected args, re-run a test after a fix, continue to finish after a commit.

The trigger condition ("no user input and no decision") explicitly preserves `ask_user` — when genuinely unsure, a decision *is* required, so the bullet does not fire. The stop condition ("real choice point or need the user") covers the legitimate yield points:

1. **`ask_user` when unsure** — excluded by "no decision" / "need the user." Preserved. ✓
2. **End turn after spawning reviewer** (APP_RULES L121: "After spawning, END the turn — the finish notification resumes you; never sleep/poll") — this is the one place of theoretical tension. It resolves cleanly: (a) the explicit, specific "END the turn" instruction prevails over the general chaining principle (specific-over-general is standard prompt interpretation); (b) waiting for the external finish notification is itself a "need the user / external signal" stop condition; (c) the bullet's examples are within-task tool chaining, not agent handoffs. No conflict in practice.
3. **Wait for finish notification** — external signal = stop condition. ✓
4. **`continue to finish after a commit`** — aligns with the closing sequence (commit → finish is the natural next step, and finish is the gated exit). ✓

The wording "Tool results are continuations of your turn, not new questions" directly addresses systemic factor #3 (conversational turn-taking bias) without overriding any explicit stop rule. The cadence clause ("immediately — in the same turn, without pausing to narrate") addresses factor #4 (error-narration gap) and is reinforced by the existing HOW memory 0af9a3a2.

**Optional (non-blocking) observation:** the bullet could optionally carry a parenthetical noting it does not override explicit handoff/wait instructions (e.g. the reviewer-spawn end-turn), which would make the specific-over-general precedence explicit rather than implicit. This is a nice-to-have, not required — the bounding is sufficient as written and the specific rules are unambiguous. Not raised as a finding.

---

## Conclusion

The change is correct, minimal, well-tested, and cache-stable. It implements mitigations ① (chain-instruction) and ④ (retry-cadence) from the evaluated set, with ② (throughput signal) deferred and ③ (tool-result-as-continuation) folded into the preamble wording — exactly as the plan and the DECISION memory (51cafcd3) specify. No findings.

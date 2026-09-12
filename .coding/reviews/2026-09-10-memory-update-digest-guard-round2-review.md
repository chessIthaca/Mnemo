## Verdict: PASS

Both round-1 findings are correctly fixed and committed at b0dd62d (HEAD of wt/agenticcoding, clean tree); the commit contains exactly the expected nine-file set; no regressions from the two fixes — a pure string-literal schema edit (no test pins the old text) and a knowledge-file prose note, with the guard, regression test, and three repairs byte-identical to what round 1 verified.


# Round-2 verification: memory_update digest-rewrite data loss (plan e8565822, backlog 63f882c9)

Scope: commit b0dd62d (HEAD, wt/agenticcoding) — the two round-1 fixes plus the previously verified changeset. Method: read the committed file states (tree clean — `git diff HEAD` and `git status` both empty, so the on-disk states read here ARE the committed states), the full commit diff, and the round-1 report; searched the repo for schema-text regressions; took test evidence from the recorded runs (commit message + plan), the same method as round 1 (read-only reviewer, no shell).

## LOW 1 — memory_update schema now states the file-body contract — FIXED

src/tool/memory/mod.rs:692-695 — the `content` property's description now reads (Rust `\`-continuations joined; leading whitespace on continuation lines is stripped, so each join is a single space):

"Optional new content — re-embeds the memory. For knowledge-backed records this replaces the truth file's FULL body: pass the complete corrected body (or full body + a dated amendment paragraph); digest-shaped content (shorter + a knowledge-path pointer tail) is refused."

- Matches the expected fix text exactly, and implements round 1's suggested remedy at the suggested location (the one-line addition to the `content` property's description, not a rewrite of the main description). The property now states both halves of the contract — the full-body replacement semantics and the digest-shape refusal — so the main description's generic "tighten" no longer invites the condensed shape without a counter-signal at the point where the agent composes `content`.
- **Well-formed**: the json! block is balanced (quotes, braces, commas, `"required": ["id"]` intact); the description is one string literal with no unescaped quotes and no escapes other than the three line continuations; the committed code compiles (green suites under `#![deny(warnings)]` prove it). The continuation joins produce single spaces ("For knowledge-backed", "corrected body", "a knowledge-path") — one coherent description, no doubled or missing spaces.
- **No snapshot regression**: the only occurrence of "re-embeds the memory" in the repo is the schema itself — no test pins the old description text, so the string change cannot break any assertion.

## LOW 2 — BUG record dating note — FIXED (sanctioned "note why not")

.coding/knowledge/bug/2027-01-07-memory-update-digest-rewrite-destroys-knowledge.md:8 — the appended Dating note covers every element of the described fix:

- The file date (2027-01-07) is identified as the memory store's write-time stamp, carried by every knowledge record written this session — corroborated: the load_tools HOW record referenced in backlog item cc52264b is likewise 2027-01-07-stamped.
- The incident/fix date 2027-01-09 is tied to the plan (ROOT CAUSE CONFIRMED note: "RED run, 2027-01-09"), the backlog item, and the round-1 report's own reading of the body — all three verified to carry that date.
- Git and the review reports read 2026-09-10 — **tiebreaker confirmed**: commit b0dd62d's author date is 2026-09-10, and the round-1 report filename is 2026-09-10-memory-update-digest-guard-review.md.
- The file keeps its stamped date because renaming would orphan the derived row (the row id derives from the slug), and the body keeps the narrative dates.

Round 1 offered "align them or note why not" — the note-why-not option was taken, and the note is accurate on all three disagreeing date surfaces.

## Commit b0dd62d — exactly the expected nine-file set

1. src/memory/knowledge.rs — digest-shape guard + `looks_like_row_digest` + doc comment (39 insertions; identical to the round-1-verified working tree).
2. src/tool/memory/mod.rs — schema fix + regression test (73 insertions; the only deltas vs round 1's scope are the two fixes).
3. .coding/knowledge/spec/2026-12-21-multi-provider-prompt-caching-….md — 3-tier bullet + closing line restored, self-tail dropped.
4. .coding/knowledge/bug/2027-01-07-serving-layer-strips-….md — self-pointer tail dropped.
5. .coding/knowledge/decision/2026-12-21-message-toolcall-construction-….md — successor body expanded from the superseded predecessor.
6. .coding/knowledge/bug/2027-01-07-memory-update-digest-rewrite-destroys-knowledge.md — the BUG record with its dating note.
7. .coding/plans/e8565822.md — the plan file (4/4 steps, regression test recorded).
8. .coding/reviews/2026-09-10-memory-update-digest-guard-review.md — the round-1 report, committed verbatim.
9. .coding/backlog.jsonl — 63f882c9 flipped pending → in_flight with plan_id e8565822 (correct mid-plan bookkeeping; the app resolves it at finish).

No extra files, none missing.

## No regressions from the fixes

- The two post-round-1 changes are (a) a pure string-literal edit inside a json! block — zero logic change, and no test references the old text — and (b) a prose note appended to a knowledge file — no code surface. The guard, regression test, and three repairs inside the commit are the same changes round 1 verified correct.
- Tree is clean at b0dd62d (HEAD): `git diff HEAD` empty, `git status --short` empty.
- Test evidence (recorded, as in round 1): root 2159 + 16 doc-tests, src-tauri 293+4+2, 0 failed — recorded in the commit message and matching the task's stated post-fix re-runs. The regression test `update_refuses_digest_style_content_that_would_shrink_the_file` is in the commit with its control (a legitimate shorter, tail-less update still succeeds and rewrites the file).

## Constitution checks

- **Doc sync**: the one LLM-facing surface that still documented the old contract now matches the enforced behavior; the BUG record's date anomaly is explained in-band. No README/PLAN.md impact from the two fixes (round 1 confirmed the README:56 invariant is unchanged and now enforced).
- **Multi-platform**: string-literal + markdown only; no platform surface touched.
- **Bug-plan specifics**: regression test present and exercising the changed path; root cause documented (plan Context + ROOT CAUSE CONFIRMED); BUG memory written with symptom → root cause → fix + regression test name, plus the dating note.

Round-2 verdict: both findings fixed as specified, no new issues — the plan is ready to finish.

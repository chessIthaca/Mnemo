# Review policy

These reviewer mechanics are enforced by the app's reviewer spawn path.

- **Re-review rounds are delta-scoped (backlog 85313a7e).** Every reviewer
  spawn carries a harness-rendered preamble: the verdict contract, the checks
  above as one line each, and the `.coding/**` rule (an accuracy check in one
  line — never a line review; generated/lockfile noise is out of scope). The
  app stamps each round's base revision on the plan frame (`## Reviews` in
  `.coding/plans/<id>.md`) and, from round 2 on, the preamble names that base,
  the delta scope and the mechanically-derived changed file set —
  so a re-review verifies the DELTA, not the whole tree. It narrows only when
  verified work is committed: **commit code at step boundaries on `wt/*`** (the
  closing sequence still commits last, with the report) so `git diff <base>` is
  exactly the fix. Never hand-write the preamble or the scope — the spawn path
  renders it, and re-writing it by hand only bloats the prompt.
- **Reviewer tool surface + failed-reviewer protocol.** A `role:"reviewer"`
  subagent is read-only by construction: reads + `git_read` (op `diff`/`log`/
  `show`/`status`), `web_fetch`, the graph tools, memory/backlog **query** tools
  + `write_review_report`. It cannot ask questions (`ask_user`), mutate memory
  or backlog, run plan tools, or `finish`. **Reviewer-only authorship:** a
  review report can only ever be authored by a spawned `role:"reviewer"`
  agent — `write_review_report` is visible under no other filter, and
  `.coding/reviews/` is protected from the file tools (the one residual is an
  approval-gated `shell` call — the standing residual, as with
  `.coding/plans/`), so the main agent can never write a review itself. If a reviewer fails **without writing a
  report**, it is NOT done: never respawn it blindly — `ask_user` and let
  the user choose (retry on another model / abandon the review).
  `spawn_agent` refuses a blind reviewer respawn while a reviewer failure is
  pending. An un-completable review is escaped via `abandon_plan` (returns
  to Planning) — the main agent is never stuck in Reviewing.

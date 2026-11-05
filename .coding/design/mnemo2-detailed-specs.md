# Detailed specs for cheap workers — proposal

Status: proposal, 2026-11-02. User decisions the same day:

- **The lead writes the specs.** That means Claude, the lead itself, not a subagent or worker. A spec-writing worker (part 2) is only for really complicated work, and the lead decides when.
- **Longer specs are preferred:** every detail needed to implement faithfully. No length target to stay under.
- **First plan, then implement:** the whole plan, with every step's spec written, is finished before the first dispatch.

## Why

Mnemo exists so that cheap models (deepseek-flash at $0.3/$1.2 per M tokens) do most of the implementing while a strong lead plans and reviews. That only works if the plan carries the design. Today it does not.

What happened on 2026-11-02:

| Measure | Value |
|---|---|
| Plan size | 700–2 400 words |
| Words per step | 40–60 |
| Where the "how" lived | the lead's head, the dispatch brief, later review messages |
| Live model switch (one feature) | 3 audit rounds, 6 + 1 + 4 findings |
| Compact-then-switch fix | 2 audit rounds, 6 + 1 findings |
| Workers on deepseek-flash | needed the design spelled out in fix messages |

Root causes:

1. **Steps were tasks, not specs.** "Add a loading state" leaves the worker to design it. A strong model designs well enough; a cheap one guesses, and review becomes the design phase, at the most expensive point.
2. **The brief is not durable.** The dispatch `brief` is not stored in the plan. A worker that is resumed, compacted or switched to another model (which also summarizes its context) loses it.
3. **A board gate punished precise designs.** The evidence rule refused context lines naming functions that don't exist yet, so new code was described vaguely. The user decided to remove it, and that removal is in progress.
4. **Small models have small windows.** deepseek-flash is switched to with summarization; a long spec in the conversation can be summarized away.
5. **Nobody checks a spec before the code.** Ambiguities surface as review findings after the work is done.

## Goals

- A cheap model can implement a step without making a design decision.
- The spec survives resumes, compaction and model switches.
- Ambiguities surface before coding, not in review.
- Writing specs costs less than the review rounds it saves.
- No gate that punishes precision. Checks guide; they don't block good plans.

## Proposal

### 1. A step is a spec (done: mnemo-plan skill, plugin 0.1.3)

Every step carries:
- the files, and the symbols to add or change with signatures (new code marked new);
- the data shapes;
- the behaviour rules and edge cases;
- the order of edits;
- every test by name with its setup and assertions;
- a done checklist;
- the files not to touch.

Typical size is 150–400 words. This works without code changes. It is the lead's discipline, now written down.

### 2. Spec writers, only for really complicated work

Decided: the lead writes every spec. Only when a design is really complicated, and the lead can't write the spec without research, (unfamiliar subsystem, performance question, several viable designs), the plan starts with a **spec step** on the difficult tier (glm-5.3 or Kimi). Its output is not code but `.coding/design/<plan>-<step>.md`: the spec for each implementation step in the format above, written against the real code it read.

The lead reviews that spec (cheap: it's a document) and then dispatches the implementation steps on the simple or average tier. This makes the expensive model do the expensive part once, and the cheap model do the volume.

### 3. Spec files instead of step text (Mnemo2 change)

Step text in `.coding/plans/<id>.md` gets long and the plan becomes hard to read. Instead:

- Each step may point at a spec file: `spec: .coding/design/<plan>-<step>.md`.
- `team_dispatch` puts the full spec file into the worker's first message. The plan page shows a one-line summary with a link (in the Plans tab, the spec opens like any other path, using the hand-over merged today).
- The spec is **pinned**: compaction and a summarize-for-smaller-model never drop it. The worker's summary keeps a pointer, and the spec is re-injected after every compaction or model switch. This fixes root causes 2 and 4.

### 4. Read-back before coding (Mnemo2 change, optional per step)

A worker's first turn on a spec step is a read-back: list ambiguities, contradictions with the code it finds, and anything the spec doesn't decide. Workers can't ask the user, but they can message the lead. The lead answers by amending the spec, not in chat, so the answer is durable. Only then does the worker code. It costs one short turn and replaces a review round.

### 5. Advisory spec check, not a gate (Mnemo2 change)

`plan_create`/`plan_update` report, as a note and never a refusal, when an implementation step has no signatures, no test names or no done checklist. The note lists what's missing. The lead can ignore it for a trivial step. Removing the evidence gate stays decided; nothing new blocks.

### 6. Measure whether it works

Per merged step, record the review rounds, the audit findings and the worker model in the plan's progress line. They are already visible in the review reports, so collect them into one number per plan. If cheap-tier steps with specs need no more fix rounds than glm steps without, the approach is working.

## What this does not do

- It does not make every task big: a one-line mechanical change stays a one-line step with an exact edit.
- It does not replace review or the independent audit for high-risk work.
- It does not let workers plan: they implement the spec and flag gaps.

## Rollout

| Part | Change | Effort |
|---|---|---|
| 1 | skill only | done |
| 2 | lead practice + spec format | none in code |
| 3 | spec files, dispatch injection, pin through compaction and switch | medium (kernel + Mnemo2) |
| 4 | read-back first turn | small (dispatch prompt + lead message flow) |
| 5 | advisory spec note | small (plan.rs) |
| 6 | per-plan rounds/findings summary | small (review report) |

Suggested order: 1–2 now, then 3 (the biggest win: durable, pinned specs), then 4–6.

## Open questions

1. Spec files per step, or one spec file per plan with a section per step?
2. Should the read-back (4) be on by default, or only for steps the lead marks?
3. Should the spec writer (2) use Kimi or glm-5.3 by default?

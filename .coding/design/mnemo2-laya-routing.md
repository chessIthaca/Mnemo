# Design: Laya routes each worker once, at dispatch (after marcreichel/laya-router)

Status: proposal, not on the board, not dispatched. Written 2026-11-03 by the
lead (Claude) against wt/mnemo at a6984816. Each claim about Mnemo code cites
the file and line it was read from. Code that does not exist yet is marked
**new**.

---

## 0. Summary

[marcreichel/laya-router](https://github.com/marcreichel/laya-router) is a
small Claude Code plugin (about 450 lines of TypeScript) that picks a
session's model from its first prompt. A local Laya classifier answers one
question: "How demanding is this request for a coding assistant?". There are
four neutral labels, each with a keyword description. The router acts only
when the answer clears a confidence gate calibrated on a labelled prompt set.
It routes **once**, because a later switch drops the prompt cache. After
that it never switches by itself: it *suggests* a bigger model when tools
keep failing or the work drifts harder. A manual model choice always wins,
and it is logged. Every route, nudge, override and session end goes to a log
that is used for calibration.

Mnemo2 has the parts but uses them badly:

- **Who decides today.** Model choice is the lead's judgement: the lead
  passes a `tier` on every `team_dispatch` (src-tauri/src/mnemo2/team.rs:1660,
  `lead_model_choice` at :930-958), and the tier's first available model runs
  the worker.
- **Laya already runs, but on the wrong thing.** With `[general.laya]
  routing` on, Mnemo1's per-turn gate (src/agent/turn.rs:1243-1282) runs on
  **every worker turn**. Workers are top-level sessions on the lead's engine,
  not subagents, so the `is_subagent()` guard at turn.rs:1249 does not skip
  them. The gate classifies the last user message, which was the plan header
  until today and is now the fixed kickoff text (plan 40eef714 step 2).
- **The result is useless.** It asks a two-label question tuned for Mnemo1
  (`trivial` vs `architectural`, model_routing.rs:109-131) against a 0.80
  gate. The real log `~/.mnemo/laya/training/routing.jsonl` holds 94
  decisions: **94/94 are `architectural`, the mean confidence is 0.42 (range
  0.01–0.68), and not one cleared the gate.**
- **A real hazard.** If anyone turned `enforce` on, this gate would switch a
  worker's model *per turn*, overriding the lead's tier mid-session and
  dropping the cache. That is exactly what laya-router avoids.
- **Nothing learns.** No outcome feeds back into routing. A user's manual
  model change in mission control, or the lead's `team_resume` onto another
  tier, is lost as a signal. The user asked for exactly this on 2026-11-02
  ("can me changing things by hand also feed into the learnings for laya?"),
  and it was deferred.

**Recommendation.** Adopt laya-router's architecture, moved from the session
to the *dispatch*:

1. Route once, at `team_dispatch`, on the worker's step spec plus the brief.
   This is the "first prompt", and since plan 40eef714 it is exactly the
   pinned task.
2. Use three neutral labels (`trivial`, `routine`, `hard`) mapped to Mnemo2's
   three tiers, with keyword descriptions tuned by an eval.
3. Calibrate the confidence gate on an eval set built partly from this
   repository's own plan history.
4. Ship shadow-first. The lead still chooses, and Laya's choice is logged
   beside it. Then a `suggest` mode, where Laya's tier is shown and used when
   the lead passes `tier: "auto"`. `auto` mode, where Laya decides unless the
   lead overrides, comes only after the log shows it agrees with outcomes.
5. Never switch a running worker by itself. Raise a one-time **nudge** notice
   to the lead (strikes, drift, failed review rounds). The lead or the user
   decides, using the live-switch machinery that already exists.
6. Log every route, override (by lead or user), nudge and outcome, joined to
   the Effort counters from plan 40eef714 step 4 (review rounds, fix rounds,
   high findings, audits, models used). That outcome-labelled corpus is what a
   later fine-tune trains on.
7. Opt Mnemo2's sessions out of the per-turn gate, so routing happens in one
   place.

This is better than today on four counts:
- It classifies the right text, the full step spec rather than a constant
  kickoff.
- It asks a question whose labels match Mnemo2's tiers.
- Its gate is calibrated instead of set at a guessed 0.80 that nothing clears.
- Every human or lead correction becomes training data.

It saves the lead's judgement only where Laya is reliably right, and it can
never silently override a choice.

---

## 1. What laya-router does (read from its source, v0.3.1)

Files: `hooks/tiers.ts` (1.8 KB), `hooks/register.ts` (13 KB),
`eval/route.ts` (5.3 KB), `types/index.d.ts`, `.claude-plugin/plugin.json`,
`compose.yaml`.

### 1.1 The question (`hooks/tiers.ts`)

```ts
haiku:  { label: 'trivial',  effort: 'low',    describe: 'quick question, translation, rename, typo, version bump' }
sonnet: { label: 'routine',  effort: 'medium', describe: 'new feature, refactor, tests, script, query, email, summary' }
opus:   { label: 'hard',     effort: 'high',   describe: 'subtle bug, flaky test, memory leak, performance regression, architecture, migration plan, security audit' }
fable:  { label: 'research', effort: 'xhigh',  describe: 'new theory, hypothesis, formal proof, mathematical bound' }
instructions: 'How demanding is this request for a coding assistant?'
```

Design rules stated in the source:

- "Laya knows tasks, not models: ask how demanding the prompt is."
- Neutral labels with **keyword-list descriptions**, because "model names as
  labels skew it".
- When a tier is disabled, its keywords fold into the next tier up. Without
  Fable, Opus's description gains ", new theory, formal proof".
- Each tier has a default effort, user-overridable per tier.

### 1.2 The gate

- `MIN_CONFIDENCE = { withoutFable: 0.475, withFable: 0.375 }`.
  `answer_confidence` is the top probability, so its floor is 1/labels, and
  each label count gets its own threshold: "the lowest that keeps about 90%
  of routed prompts on the expected tier".
- Below the gate, an unknown label, or laya-serve down: pass through
  unchanged. The status line says `laya: unsure (0.39), using /model` or
  `laya: offline, using /model`.
- A prompt under three words is never classified ("Laya reads 'hi' as a
  confident trivial task").
- A skill invocation is classified on the typed text plus the first 2000
  characters of the skill body.

### 1.3 Route once, then never switch (`register.ts`)

- `prompt.submit`: the first prompt is classified. Later prompts only run
  `checkDrift` in the background ("not awaited, so the prompt never waits on
  laya-serve").
- `turn.step`: the main thread only (`e.agentId === undefined`) runs on the
  routed `model` and `effort`. Subagents keep their own model.
- The comment says why: "every switch re-reads the whole conversation
  uncached."

### 1.4 Manual choice wins

- A `/model` before the first prompt disables routing for the session. The
  start model is snapshotted at `session.start` and compared at classify time.
- A `/model` after routing turns routing off for the rest of the session and
  logs `reroute {from, to, nudged}`. Even `/model opus` while already on Opus
  counts as a choice, so it is caught at `command.run`, not by comparing
  snapshots.
- `/effort` alone does not count as a model choice.

### 1.5 Nudges, once per session

- **Strikes.** `tool.call` counts consecutive failed calls *or identical
  repeated calls* (the JSON of the call minus `tool_use_id`). At 3
  (`STRIKES_TO_NUDGE`) it suggests the tier above: "tools keep failing" or
  "going in circles". A success resets the counter.
- **Drift.** A later prompt classified as a harder tier, above the gate,
  produces "this got harder, consider /model opus".
- A nudge is shown once (the `nudge` atom). If the session ends without a
  `/model`, it logs `nudge-ignored`.

### 1.6 The calibration log and the eval

- `log(event, data)` keeps the last 1000 entries in the plugin store. Events:
  `route {tier|null, confidence, offline?, manual?}`, `nudge {tier, reason}`,
  `reroute {from, to, nudged}`, `nudge-ignored {tier}`, `end {reason}`.
  `/laya-log` prints them.
- `eval/route.ts`: 41 hand-labelled prompts (9 trivial, 13 routine, 10 hard,
  6 research, 2 `null` "too vague"). They are classified one at a time
  ("laya-serve rejects bursts"). For thresholds 0.25…0.80 it prints the share
  routed and the accuracy on routed prompts, marks `MIN_CONFIDENCE`, and lists
  the misses.
- There is no automatic learning. The log is for a human to retune the
  descriptions and the threshold.

### 1.7 What is deliberately thin

- There is no outcome signal beyond the nudge and reroute events.
- The log is a read-modify-write of one array; the author's own comment says
  two simultaneous writes can lose one.
- The health check is a 2-second race; a slow but alive server counts as down
  for the first prompt.

Mnemo2 can do better on the first point, because a worker's real outcome is
known (merge, review rounds, findings).

---

## 2. What Mnemo2 does today

### 2.1 The lead picks a tier per dispatch

- **Tier lists.** `[tiers]` in `endpoints.toml` holds `difficult`, `average`
  and `simple`, each an ordered list of `endpoint/model` specs
  (src/config/endpoints.rs:32-75).
- **Tier descriptions.** The purposes the lead reads are in `TIER_PURPOSES`
  (team.rs:775-785): difficult is "design, debugging, bug_fixing plans,
  cross-cutting work, steps touching shared contracts", average is "typical
  implementation steps", and simple covers single-file mechanical edits,
  checks, lookups and scaffolding.
- **Resolution.** `tier_models(tier)` (team.rs:899-912) returns the tier's
  available models in order. `lead_model_choice(tier, model)` (team.rs:930-958)
  takes the first one, or validates an explicit model against the tiers.
- **A tier is required.** `team_dispatch` refuses a dispatch without one while
  tiers are configured (`dispatch_required.push("tier")`, team.rs:1660).
- **Moving a worker.** The lead can change a worker's model with
  `team_resume {tier|model}`. The user can change it in mission control
  (`change_model`, team.rs:3268, via ipc.rs:944), live from the next request
  (plan 1586c6c7).

### 2.2 The per-turn Laya gate also runs, on the wrong text

The relevant code:

- `RoutingGate` (src/agent/model_routing.rs:333-348) is wired into every
  agent by the factory (src/agent/factory.rs:1338-1341).
- `route_turn_start` (src/agent/turn.rs:1243-1282) runs once per turn. It
  skips only subagents (turn.rs:1249). It classifies the **last user
  message** with the two-label question (model_routing.rs:109-131) at the
  `[general.routing] threshold` (default 0.80), then logs a
  `RouteDecisionRow` (model_routing.rs:201-260). With `enforce` on, it
  switches that turn's model to `cheap` or `capable`.

Mnemo2 workers run as sessions on the lead's engine (team.rs `engine.spawn`,
see .coding/design/mnemo2-laya.md §0), so they are not subagents. Each worker
turn is therefore classified:

- **Text.** It reads whatever the last user message was. Before 40eef714 that
  was the opening prompt, which starts with the plan header (`# Plan: …`, the
  same for every step of a plan), capped at 2000 characters. That often never
  reaches the step at all. Since 40eef714 it is `KICKOFF`, a constant
  sentence, or a `team_message` / route-fix text on later turns.
- **Data.** 94 decisions and 94 outcomes so far. All 94 are `architectural`
  and all are shadow. Mean confidence 0.419, min 0.011, max 0.677, so zero
  cleared 0.80. The last four rows classified a fix-finding message, an audit
  brief and two plan headers.
- **Risk.** With `enforce = true` it would override the lead's tier on a
  worker, per turn, through the `cheap`/`capable` lanes. Those are configured
  separately from Mnemo2's tiers in Settings > Decisions
  (src-tauri/src/mnemo2/laya_settings.rs:210-238).

### 2.3 What exists to build on

- **Laya is attached** once per engine, and every worker shares the
  classifier slot. `Setup::classifier_slot()` (src/headless/setup.rs:833) and
  `Setup::live_config()` (:844) are public. The `Classifier` trait has
  `classify` and a batched `classify_many` (src/memory/classifier.rs:198).
  `Question::Choice { instructions, criteria }` is the wire shape
  (classifier.rs:41-55).
- **The pinned task.** Since 40eef714 every worker's full task (plan header,
  step spec and brief) is `WorkerRecord.opening_prompt`, pinned in the system
  head. The step spec alone is available from the board at dispatch.
- **Outcome counters** (40eef714 step 4): `review_rounds`, `fix_rounds`,
  `audits` and `models_used` on `WorkerRecord`, plus findings with severity.
  Mnemo2 merges, conflicts and failures are recorded as statuses.
- **Live model switch** (plan 1586c6c7) applies from the next request. The
  deferred summarize path is fixed (plan 1238e0de).
- **Lead notices** reach the lead through the queue and the waiter.
- **Eval data in the repository:**
  - .coding/plans/ has hundreds of plan files with step texts;
  - 63 team review reports (.coding/reviews/team-*.md) list each task's model
    and status;
  - from now on the Effort table has rounds per task.

---

## 3. Idea by idea: adopt, adapt, reject

| laya-router idea | Mnemo2 verdict | Why |
| --- | --- | --- |
| Ask how demanding the task is, not which model | **Adopt** | The same reason holds, and the current Mnemo1 question (design vs trivial) is exactly what produced 94/94 `architectural`. |
| Neutral labels + keyword descriptions | **Adopt** | `trivial`/`routine`/`hard` map onto simple/average/difficult. Never use tier names or model names as labels. |
| Fold a disabled tier's keywords into the next tier up | **Adopt** | When a Mnemo2 tier list is empty, its label is dropped and its keywords move up. |
| Gate calibrated per label count from an eval, floor 1/labels | **Adopt** | Replaces the guessed 0.80. The threshold is per label count and stored in config, with the eval's value as default. |
| Route only the first prompt | **Adapt** | Route once per worker at **dispatch**, on the step spec + brief, not on a chat message. |
| Never auto-switch; nudge once | **Adopt** | A switch drops the cache and summarizes on smaller windows. The lead or the user decides. |
| Strikes: 3 failed or identical tool calls → suggest tier up | **Adopt** | It is cheap and deterministic, from the worker's tool events in team.rs's pump. |
| Drift: a later prompt reads harder | **Adapt** | Classify the lead's `team_message` / `team_route_fix` text and the review findings, not user chat. |
| Manual `/model` wins and is logged | **Adopt** | An explicit lead tier at dispatch that disagrees with Laya, a `team_resume` onto another tier, and a user's mission-control `change_model` are all overrides, all logged. This delivers the deferred "manual changes feed Laya" request. |
| Under-3-words skip | **Reject** | Specs are long; there is no short first prompt. Replaced by "classify only when the step text is at least 20 words". |
| Skill text capped at 2000 chars | **Adapt** | Cap the classified text at 2000 chars, built **step header first**: step text, then brief, never the plan boilerplate. |
| Per-tier effort | **Adopt (optional)** | A tier can name a default `reasoning_effort` (new optional field), used only when the lead passes none. |
| Status line `laya → opus 0.87` | **Adapt** | A worker card chip in mission control (`Laya: average 0.62`, `lead chose difficult`), and the same text in the dispatch result for the lead. |
| `/laya-log` | **Adapt** | A JSONL log on disk plus a Settings > Decisions "Routing log" view with agreement and outcome stats. |
| Log for humans only | **Extend** | Join outcomes (merge, rounds, findings) and derive a training label per worker. A later fine-tune uses it; the existing Laya fine-tune machinery only trains failure triage today (src-tauri/src/ipc/finetune.rs:69-73). |
| One process-wide store array (racy) | **Reject** | Use append-only JSONL with one line per event (`append_row`, model_routing.rs:288-307 is the pattern). |

---

## 4. The design

### 4.1 One routing point: `team_dispatch`

At dispatch, before the worker is provisioned, Mnemo2 asks Laya one question
about the worker's task. Nothing else in Mnemo2 routes:

- **Opt the sessions out.** Mnemo2 opts every session it spawns (workers,
  audits, the Mnemo2 lead) out of the per-turn gate with a new
  `SpawnOptions.skip_turn_routing: bool`. The default `false` keeps Mnemo1 and
  every other host unchanged.
- **Keep the shared config.** `[general.laya] routing` and `[general.routing]`
  keep meaning what they mean for Mnemo1. Mnemo2 gets its own settings block,
  `[mnemo2.routing]` in Mnemo2's settings file (§4.9), so turning Mnemo2
  routing on never arms Mnemo1's per-turn lanes and vice versa.

### 4.2 The classified text

The new `routing_text(step_text: &str, brief: &str) -> String` builds, in
order:

1. the step's text as stored on the board (header plus spec), with the
   `- [ ] N.` checkbox prefix stripped;
2. a blank line, then the lead's brief.

It is capped at 2000 characters (`ROUTING_TEXT_MAX_CHARS`,
model_routing.rs:64). The plan title, goal, context, interfaces and
acceptance are **excluded**: they are shared by every step of the plan and
carry no per-step signal. This is the lesson of §2.2, where the plan header
crowded out the step.

- A step under 20 words is not classified (`skip: "too_short"`); the lead's
  tier stands.
- An audit dispatch is never classified. Audits use the configured audit
  model or tier.

### 4.3 The question

The new `dispatch_routing_question(enabled: &EnabledTiers) -> Question`.
`EnabledTiers` is **new**: which of simple, average and difficult have at
least one available model.

```text
instructions: "How demanding is this task for a coding agent that has the full spec in front of it?"
criteria (only for enabled tiers; label -> description):
  trivial -> "rename, typo, version bump, config value, copy change, add a log line, delete dead code, update a doc comment, single-file mechanical edit"
  routine -> "implement a specified function, add tests for existing code, wire an existing API into a new caller, UI component from a spec, schema or DTO field, follow an existing pattern in a new place, refactor inside one module"
  hard    -> "subtle bug, race or deadlock, flaky test, data loss, memory or performance regression, security, persistence format, cross-module or shared-contract change, concurrency, migration, design decision the spec leaves open"
label -> tier: trivial -> simple, routine -> average, hard -> difficult
```

Rules:

- **Neutral labels only.** Never `simple`/`average`/`difficult` (they are our
  tier names, and laya-router found that model or tier-like names skew the
  checkpoint), and never yes/no-ish words (classifier.rs:44-47).
- **A disabled tier's label is dropped** and its keywords are appended to the
  next tier *up*. A disabled `difficult` folds into `routine`, so the top
  enabled tier absorbs everything above it. A disabled `simple` folds
  `trivial` into `routine`.
- **One enabled tier means no classification** (`skip: "one_tier"`).
- **Descriptions are constants** in one module, so the eval (§4.8) and
  production use byte-identical text. Changing a description requires
  re-running the eval and updating the stored threshold (§4.4).

### 4.4 The gate

The new `DispatchRoutingDecision`:

```rust
pub struct DispatchRoutingDecision {
    pub label: String,
    pub tier: Option<&'static str>,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub cleared: bool,
}
```

- `tier` is `None` for an unknown label.
- `cleared` is `tier.is_some() && confidence >= threshold`.
- The threshold is per label count: `[mnemo2.routing] min_confidence_3`
  (default **0.475**) and `min_confidence_2` (default **0.60**, until the eval
  sets it).
- Both defaults are **provisional**, copied from laya-router's 3-label value
  and a 2-label guess. Step 1's eval run replaces them with this checkpoint's
  measured values before any mode above `shadow` is offered. The settings UI
  shows the eval date next to the value.
- No answer (slot empty, server down, timeout, a non-choice answer) is
  `None`. Nothing is routed, and the log records `offline`.

### 4.5 Modes: `off`, `shadow`, `suggest`, `auto`

`[mnemo2.routing] mode` (new, default `off`):

| mode | Laya asked? | lead must pass `tier`? | what runs the worker |
| --- | --- | --- | --- |
| `off` | no | yes (today) | the lead's tier, as today |
| `shadow` | yes | yes (today) | the lead's tier; Laya's pick is only logged, plus one line in the dispatch result: `Laya would pick average (0.62).` |
| `suggest` | yes | no: `tier` may be `"auto"` | `auto` → Laya's tier if cleared, else **refused** with "Laya is unsure (0.41); pass a tier"; an explicit tier → that tier, logged as agree/override |
| `auto` | yes | no: `tier` is optional | omitted or `auto` → Laya's tier if cleared, else the `fallback_tier` (default `average`); an explicit tier → that tier (override) |

- The dispatch schema's `tier` enum gains `"auto"` when mode is `suggest` or
  `auto`. With mode `auto`, `tier` leaves `dispatch_required` (team.rs:1660).
- **An explicit `model` always wins** and counts as an override when Laya
  cleared on a different tier.
- **Laya picks the tier, never the model inside it.** The tier's first
  available model runs, exactly as `lead_model_choice` does today.
- **Shadow is not optional on the way up.** The settings UI enables `suggest`
  only after the log holds at least 30 shadow decisions, and `auto` only after
  at least 30 decisions with outcomes and an agreement-with-outcome rate the
  UI shows (§4.7). The user can still force it with a confirmation.

### 4.6 Overrides, reroutes and nudges

**Override events** are logged, never refused:

- `lead_override`: at dispatch, Laya cleared on tier X and the lead passed
  tier or model Y ≠ X.
- `lead_reroute`: `team_resume` with a `tier` or `model` that changes the
  worker's tier, or a lead `change_model` (team.rs:3268, called from the lead
  tool path).
- `user_reroute`: `change_model` from mission control (ipc.rs:944). This
  needs a new `source: ChangeSource { Lead, User }` parameter on
  `change_model`.

Each records `from_tier`, `to_tier`, `from_model`, `to_model`, the worker's
status, `review_rounds` and `fix_rounds` so far, and whether a nudge preceded
it.

**Nudges** are new. They go to the lead as a notice, at most **one per
worker**, and are never automatic:

- **Strikes.**
  - Counting: in the worker event pump (team.rs, where `ToolResult` /
    `ToolError` events are handled), count consecutive failed tool results, or
    a tool call identical to the previous one (name plus canonical JSON
    arguments, ignoring the call id).
  - Reset: any successful, non-repeated call resets the count.
  - Trigger: at **3**, when a tier above the worker's current tier exists,
    queue the notice `[TeamManager notice — not the human] worker_id=X Laya
    nudge: X looks stuck (3 failing tool calls in a row); consider team_resume
    with tier difficult.`
  - Reason text: "going in circles" for the identical-call case.
- **Drift.** When the lead sends `team_message` or `team_route_fix` text of
  at least 20 words, classify it in the background (not awaited, best-effort).
  If it clears on a tier above the current one, send the notice "this got
  harder: <reason>".
- **Review.** When a `team_review` records at least one **high** finding and
  the worker's `review_rounds` reaches 2, send a nudge to the next tier up.
  This is deterministic, needs no classifier, and works in every mode except
  `off`.
- **Mission control.** The same text appears as a chip on the worker card
  (`Laya suggests difficult — stuck`). It clears when the worker's tier
  changes or the worker finishes.
- **`nudge_ignored`.** Logged when a nudged worker reaches a terminal status
  (merged, failed or stopped) on the tier it was nudged from.

There are no automatic switches, ever. The live switch exists (plan 1586c6c7),
but laya-router's cache argument is stronger for us: a smaller target window
also triggers the summarize-first path (plan 1238e0de).

### 4.7 The dispatch routing log

The log lives at `~/.mnemo/laya/training/dispatch_routing.jsonl` (**new**),
beside `routing.jsonl`. It is append-only, one JSON object per line, and
written with `model_routing::append_row`, so writes are best-effort and never
fail a dispatch. Every row has `ts` (unix ms), `event`, `worker_id`, `plan_id`
and `step` (1-based).

| event | extra fields |
| --- | --- |
| `route` | `mode`, `label`, `tier` (Laya's), `confidence`, `probabilities`, `threshold`, `cleared`, `lead_tier` (or null for auto), `ran_tier`, `ran_model`, `text_sha256`, `text` (capped at 2000), `skip` (null, `too_short`, `one_tier`, `offline` or `audit`) |
| `lead_override` | `laya_tier`, `lead_tier`, `confidence` |
| `lead_reroute` / `user_reroute` | `from_tier`, `to_tier`, `from_model`, `to_model`, `status`, `review_rounds`, `fix_rounds`, `nudged` |
| `nudge` | `reason` (`strikes`, `circles`, `drift` or `review`), `from_tier`, `to_tier`, `detail` |
| `nudge_ignored` | `tier` |
| `outcome` | `status` (merged, failed, stopped or abandoned), `final_tier`, `models_used`, `review_rounds`, `fix_rounds`, `findings`, `findings_high`, `audits`, `cost_micros`, `label` (§4.8) |

`outcome` is written once per implementation worker, when it reaches a
terminal status (merge, abandon, failed-and-stopped). That is the same place
the ledger records a merge.

### 4.8 Outcome labels and the eval

**Derived label.** The new `fn outcome_label(rows: &WorkerRoutingHistory) ->
Option<&'static str>` names the tier a step **should** have run on:

1. Merged on tier T with `fix_rounds <= 1`, no high finding, and no reroute
   gives **T** ("sufficient"). That says nothing about whether a lower tier
   would also have worked, so a T label from a lead-chosen tier is weak. It is
   marked `weak: true` when T is not `simple`.
2. Merged after a reroute *up* to U gives **U** (strong).
3. Merged after a reroute *down* to D gives **D** (strong: the cheaper tier
   was enough).
4. Merged on T with `fix_rounds >= 2` or a high finding, and no reroute, gives
   the tier **above T** (weak; capped at difficult).
5. Failed, stopped or abandoned gives no label.

**The eval** (new: `src-tauri/src/bin/mnemo2_route_eval.rs`, or a `--eval`
subcommand of `mnemo2-mcp`). It classifies each prompt one at a time against
the attached Laya and prints the same table as laya-router's `eval/route.ts`:
for thresholds 0.25..0.80 in 0.025 steps, the share routed and the accuracy
on routed prompts, then the misses. There are two corpora:

1. **The hand set** (`src-tauri/tests/fixtures/route_eval.jsonl`, new): about
   45 coding-team step texts written for this project, 15 per tier, plus 3
   `null` vague ones. They are adapted from laya-router's prompts to step-spec
   form, for example "**Bump the version** — in Cargo.toml set version = 1.3.1"
   for trivial, and "**Fix the swap race** — set_explicit_provider can …" for
   hard.
2. **The history set.** Mined by the eval from `.coding/plans/*.md` (step
   texts) joined with `.coding/reviews/team-*.md` (task model and status) and
   the dispatch log's `outcome` labels. Only strong labels are used, plus weak
   `simple` labels.

The tool prints both tables and suggests the threshold: the lowest that keeps
at least 90% accuracy on routed prompts in the hand set. The user saves that
value in settings.

### 4.9 Configuration

Mnemo2-only settings stay out of the shared config that Mnemo1 reads. They go
into a new `[routing]` table in Mnemo2's settings (the store used by
model_settings.rs / laya_settings.rs; step 3 decides the exact file after
reading it):

```toml
[routing]
mode = "off"            # off | shadow | suggest | auto
min_confidence_3 = 0.475
min_confidence_2 = 0.60
fallback_tier = "average"  # auto mode, Laya unsure
nudges = true           # strikes / drift / review nudges
min_words = 20
[routing.effort]        # optional per-tier default effort (used only when the lead passes none)
simple = "low"
average = "medium"
difficult = "high"
```

### 4.10 Surfaces

- **Dispatch result**, which the lead reads. One line appended:
  - `Laya: average (0.62) — matches your tier.`
  - `Laya: difficult (0.71) — you chose average; logged as an override.`
  - `Laya: unsure (0.41).`
  - `Laya: offline.`
  - `Laya: routed to average (0.62).` in auto mode.
- **`team_status`.** Each worker gains `routing: {laya_tier, confidence,
  cleared, lead_tier, nudged}`, absent when mode is `off`.
- **Mission control worker card.** A chip with the same text as the result
  line, plus the nudge chip (§4.6).
- **Settings > Decisions.** The Routing block gains the mode, the thresholds
  with their eval date, fallback tier, nudges toggle and per-tier effort. It
  also gets a **Routing log** summary:
  - decisions by mode;
  - agreement rate (Laya vs lead);
  - for decided workers with an outcome label, the accuracy of Laya and of the
    lead against the label;
  - overrides by source;
  - nudges and how many were acted on.

  This is what gates `suggest` and `auto` (§4.5).
- **Review report Effort table** (ledger.rs `render_effort`). New columns
  `Laya` (tier and confidence, or `-`) and `Routed by` (`lead`, `laya`,
  `lead (override)`).

### 4.11 Failure modes

| Situation | Behaviour |
| --- | --- |
| Laya disabled, not installed, or the slot empty | mode behaves as `off` for classification; `suggest` refuses `auto` with "Laya is not running; pass a tier"; `auto` uses `fallback_tier` and logs `skip: offline` |
| Laya slow | dispatch waits at most **3 s** for the answer (`tokio::time::timeout`), then treats it as offline. A dispatch never hangs on the classifier. laya-router lets an alive server take its time, but a dispatch sits on the lead's critical path, while a pre-classification of a first prompt does not. |
| Unknown label | `tier: None`, `cleared: false`, logged |
| Tier emptied after the decision | `lead_model_choice` already refuses an empty tier with a clear error; the dispatch reports it |
| Two dispatches at once | the classifier client is shared; laya-serve "rejects bursts", so dispatch classification goes through a `tokio::sync::Semaphore(1)` owned by the team runtime |
| Log write fails | swallowed (`append_row` contract) |
| Mnemo1 running at the same time | unaffected: it keeps its per-turn gate and its own config; the logs are different files |

---

## 5. Plan (steps written as specs; not on the board yet)

Order:
- step 1 (kernel opt-out, eval scaffolding) and step 2 (question, gate, log
  types; a pure module) can run in parallel;
- step 3 (dispatch integration, shadow and suggest) waits for both;
- step 4 (overrides and outcome) waits for 3;
- step 5 (nudges) waits for 4;
- step 6 (UI) waits for 3 and 5;
- step 7 is the lead's own verification.

Proposed tiers:
- 1 average (glm-5.3);
- 2 simple;
- 3 difficult, `review_risk` high (it changes the dispatch contract);
- 4 average;
- 5 average;
- 6 average.

Verification for every step:
- root: `cargo clippy --all-targets -- -D warnings`, `cargo test --lib`;
- src-tauri: `cargo clippy --manifest-path src-tauri/Cargo.toml --features
  mnemo2 --lib --tests --bins -- -D warnings`, `cargo test --manifest-path
  src-tauri/Cargo.toml --features mnemo2 --lib --bins`, `cargo build
  --manifest-path src-tauri/Cargo.toml --bins`;
- UI: from mnemo2/, `npx svelte-check --tsconfig ./tsconfig.json`, `npx vitest
  run src/lib src/components`.

No new `#[allow(...)]`.

### Step 1: Kernel: sessions can skip per-turn routing

**Files:** src/headless/types.rs, src/headless/engine.rs, src/host/runtime.rs,
src/agent/loop_impl.rs, src/agent/turn.rs, src/agent/tests.rs, every host
spawn-struct initializer (grep `pinned_task:` in src, src-tauri/src/ipc/spawn.rs,
src-tauri/src/main.rs, src-tauri/src/mnemo2/team.rs, src-tauri/src/mnemo2/runtime.rs).

**New code:**

1. `SpawnOptions` gets `#[serde(default, skip_serializing_if = "std::ops::Not::not")] pub skip_turn_routing: bool`.
   Doc: "When true, this session's turns are never classified by the
   pre-prompt routing gate, and no routing.jsonl rows are written for them.
   Mnemo2 routes at dispatch instead."
2. The host `SessionOptions` gets `pub skip_turn_routing: bool`, passed
   through the engine beside `pinned_task` (src/headless/engine.rs, next to
   `pinned_task: opts.pinned_task.clone()`).
3. `AgentLoop` gets `skip_turn_routing: AtomicBool` (default false) and
   `pub fn set_skip_turn_routing(&self, skip: bool)`. The host spawn path calls
   it right after `set_pinned_task`. A subagent never inherits it, but
   subagents are already skipped (turn.rs:1249).
4. In `route_turn_start` (turn.rs:1243), right after the `is_subagent()`
   guard: `if self.skip_turn_routing.load(Ordering::Relaxed) { return None; }`.
5. Mnemo2 sets `skip_turn_routing: true` in `spawn_worker_session`'s
   SpawnOptions (team.rs, the `pinned_task,` line) and in the lead spawn
   (runtime.rs, beside `pinned_task: None`). Every Mnemo1 initializer passes
   `false`.

**Rules:**

- Default false, so Mnemo1 behaviour and routing.jsonl rows are unchanged.
- The step-lane ladder (src/agent/step_lanes.rs) and the reflex are not
  touched; this skips only `route_turn_start`.

**Tests (written first):**

- `src/agent/tests.rs` `skip_turn_routing_writes_no_routing_rows`: reuse the
  `routing_gate(Some(("trivial", 0.99)), calls, log_path)` helper
  (tests.rs:15950), build the loop as the existing gate tests do (~16090),
  `set_skip_turn_routing(true)`, run one turn. Assert the classifier call
  count is 0 and the log file does not exist or is empty.
- `turn_routing_still_runs_by_default`: the same set-up without the flag.
  Assert one call and one decision row, which guards Mnemo1.
- `src-tauri/src/mnemo2/runtime.rs`
  `mnemo2_worker_sessions_skip_turn_routing`: dispatch through the stub team
  (`open_stub_team`) and assert the worker's AgentLoop reports
  `skip_turn_routing() == true`. Add `pub fn skip_turn_routing(&self) -> bool`
  for the assertion.

**Done:**

- [ ] both crates' clippy clean
- [ ] root `cargo test --lib` and src-tauri tests green
- [ ] bins build

**Do not touch** src-tauri/src/mnemo2/laya_settings.rs or the new routing
module (step 2).

### Step 2: The dispatch routing module: question, gate, text, log rows

**Files:** `src-tauri/src/mnemo2/dispatch_routing.rs` (new),
`src-tauri/src/mnemo2/mod.rs` (one `mod` line), tests in the new file.

**New code** (all `pub(crate)`):

```rust
pub const LABEL_TRIVIAL: &str = "trivial";
pub const LABEL_ROUTINE: &str = "routine";
pub const LABEL_HARD: &str = "hard";
pub const ROUTING_INSTRUCTIONS: &str = "How demanding is this task for a coding agent that has the full spec in front of it?";
pub const DESCRIBE_TRIVIAL: &str = "rename, typo, version bump, config value, copy change, add a log line, delete dead code, update a doc comment, single-file mechanical edit";
pub const DESCRIBE_ROUTINE: &str = "implement a specified function, add tests for existing code, wire an existing API into a new caller, UI component from a spec, schema or DTO field, follow an existing pattern in a new place, refactor inside one module";
pub const DESCRIBE_HARD: &str = "subtle bug, race or deadlock, flaky test, data loss, memory or performance regression, security, persistence format, cross-module or shared-contract change, concurrency, migration, design decision the spec leaves open";
pub const MIN_WORDS: usize = 20;
pub const TEXT_MAX_CHARS: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnabledTiers { pub simple: bool, pub average: bool, pub difficult: bool }

pub fn label_tier(label: &str) -> Option<&'static str>; // trivial->simple, routine->average, hard->difficult
pub fn routing_question(enabled: EnabledTiers) -> Option<Question>; // None when fewer than 2 enabled
pub fn routing_text(step_text: &str, brief: &str) -> Option<String>; // None when under MIN_WORDS
pub fn threshold_for(labels: usize, cfg: &RoutingSettings) -> f64;
pub async fn classify_dispatch(classifier: &dyn Classifier, text: &str, enabled: EnabledTiers, cfg: &RoutingSettings) -> Option<DispatchRoutingDecision>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingSettings { pub mode: RoutingMode, pub min_confidence_3: f64, pub min_confidence_2: f64, pub fallback_tier: String, pub nudges: bool, pub min_words: usize, pub effort: BTreeMap<String, String> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode { #[default] Off, Shadow, Suggest, Auto }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DispatchRoutingRow { Route{..}, LeadOverride{..}, LeadReroute{..}, UserReroute{..}, Nudge{..}, NudgeIgnored{..}, Outcome{..} }  // fields per design §4.7, each row also ts, worker_id, plan_id, step
pub fn dispatch_routing_log_path() -> PathBuf; // training_log_dir().join("dispatch_routing.jsonl")
pub fn outcome_label(history: &[DispatchRoutingRow], final_status: &str, final_tier: &str, fix_rounds: u32, high_findings: usize) -> Option<(&'static str, bool /*weak*/)>;
```

**Rules:**

- **Folding.** A disabled tier's description is appended (`", "` + text) to
  the next enabled tier *up*. With difficult disabled, `hard`'s keywords go to
  routine, the highest enabled tier. With simple disabled, `trivial` is
  dropped and its keywords go to routine.
- **Text.** `routing_text` strips a leading `- [ ] N. ` or `- [x] N. `,
  joins `step + "\n\n" + brief`, and caps at `TEXT_MAX_CHARS` chars
  (char-safe, as `cap_task_text` does at model_routing.rs:189-195). Words are
  counted with `split_whitespace()` on the step plus the brief.
- **Decision.** `classify_dispatch` returns `None` when the classifier returns
  `None` or a non-choice answer. Otherwise it returns `cleared = tier.is_some()
  && confidence >= threshold_for(n_labels)`.
- **Outcome label** rules exactly as design §4.8, items 1–5.

**Tests (written first, a stub `Classifier` like model_routing.rs's tests
~440-515):**

- `question_has_three_neutral_labels_when_all_tiers_enabled`: exact labels,
  exact descriptions, and no label equals a tier name.
- `disabled_top_tier_folds_its_keywords_into_routine`.
- `disabled_simple_tier_folds_trivial_into_routine`.
- `one_enabled_tier_asks_nothing`.
- `routing_text_strips_the_checkbox_and_caps_at_2000_chars`.
- `short_step_is_not_classified`.
- `confidence_at_the_threshold_clears`: 0.475 inclusive, with 3 labels.
- `unknown_label_never_clears`.
- `no_answer_is_none`.
- `rows_round_trip_as_tagged_jsonl`: one row of each variant, serialized,
  parsed back, with `"event"` tags `route`, `lead_override` and so on.
- `outcome_label_rules`: one case per rule 1–5, including weak flags.

**Done:**

- [ ] src-tauri clippy clean
- [ ] tests green
- [ ] no `#[allow(dead_code)]` or `cfg_attr` workaround. Nothing outside the
  tests calls the module until step 3. If the dead-code lint fails clippy,
  merge step 2 only together with step 3 (dispatch step 3 on top of step 2's
  branch).

**Do not touch** team.rs (step 3).

### Step 3: Dispatch routes through Laya (shadow and suggest; auto behind the same switch)

**Files:** `src-tauri/src/mnemo2/team.rs` (dispatch path, schema, result
text, `team_status` JSON, tests), `src-tauri/src/mnemo2/model_settings.rs` or
the settings store that holds Mnemo2's own settings (read it first and put
`[routing]` there; record the choice with `plan_update decision`),
`src-tauri/src/mnemo2/runtime.rs` (stub-server tests).

**New code:**

1. `TeamRuntime` gets:
   - `routing: RwLock<RoutingSettings>`, loaded at open and refreshed by the
     same `set_catalog` path as the tiers (team.rs:880-887);
   - `classifier: Option<ClassifierSlot>`, from
     `Setup::for_engine(..).classifier_slot()` at `from_lead`
     (team.rs:861-873); the lead runtime already holds the Laya manager;
   - `routing_permit: tokio::sync::Semaphore` with 1 permit.
2. `team_dispatch` handler, where `tier` is read (team.rs:1841):
   - mode `off`: unchanged;
   - otherwise build `routing_text(step_text, brief)`. The step text comes
     from the board (`board.step_text(plan_id, step)`, new if it is missing:
     read how `brief_for` gets the step and reuse it). Then classify with
     `tokio::time::timeout(Duration::from_secs(3), …)` under the permit.
3. The tier the worker runs on:
   - `shadow`: the lead's tier;
   - `suggest`: `tier == "auto"` uses Laya's tier if cleared, else returns
     `Err("Laya is unsure ({conf:.2}); pass a tier")` or
     `Err("Laya is not running; pass a tier")`. An explicit tier is used;
   - `auto`: tier omitted or `auto` uses Laya's tier if cleared, else
     `fallback_tier`. Explicit wins.
4. Schema: when mode is `suggest` or `auto`, the `tier` property's enum gains
   `"auto"` and the description says "auto = let Laya choose from the step
   spec". With mode `auto`, `"tier"` is removed from `dispatch_required`
   (team.rs:1660). Tier guidance text (`tier_guidance`, team.rs:976) gains one
   line about `auto` in those modes.
5. `WorkerRecord` gets `#[serde(default)] pub routing: Option<WorkerRouting>`,
   where `WorkerRouting { laya_tier: Option<String>, confidence: Option<f64>,
   cleared: bool, lead_tier: Option<String>, routed_by: String /* "lead" |
   "laya" | "lead_override" */, nudged: Option<String> }`. It is set at
   dispatch with `update_worker` before `start_worker` (the read_back pattern,
   team.rs ~2276-2288), and serialized into `team_status` as `routing`.
6. Append a `Route` row, and a `LeadOverride` row when Laya cleared on a
   different tier than the lead's explicit one.
7. The dispatch result gets the one line of design §4.10.
8. The optional per-tier effort applies only when the lead passed no
   `reasoning_effort` and the chosen model supports it (validate as
   `team_dispatch` already does).
9. Audit dispatches skip classification entirely.

**Rules:**

- Idempotent re-dispatch (same `worker_id` and brief) does **not** classify
  again or log twice. It returns the stored routing.
- A dispatch never waits more than 3 s on Laya.
- `tier: "auto"` in mode `off` or `shadow` is refused with "tier auto needs
  routing mode suggest or auto (Settings > Decisions)".

**Tests (written first):**

- team.rs unit tests with a stub classifier in the slot:
  - `off_mode_never_asks_laya`: call count 0, no log file;
  - `shadow_logs_layas_pick_and_runs_the_lead_tier`: worker tier = lead's; one
    `route` row with `lead_tier`, `tier` and `cleared`; the result line says
    "Laya would pick";
  - `shadow_override_is_logged_when_laya_cleared_elsewhere`;
  - `suggest_auto_uses_a_cleared_laya_tier`;
  - `suggest_auto_refuses_when_unsure` (exact error text);
  - `suggest_auto_refuses_when_laya_is_offline` (empty slot);
  - `auto_mode_falls_back_when_unsure`;
  - `explicit_model_wins_in_auto_mode`;
  - `slow_laya_times_out_as_offline`: the stub sleeps 5 s; `tokio::time::pause`
    + advance, then assert the dispatch completed with `skip: offline`;
  - `redispatch_does_not_classify_twice`;
  - `audit_dispatch_is_never_classified`;
  - `schema_offers_auto_only_in_suggest_and_auto`;
  - `tier_is_optional_only_in_auto_mode`.
- runtime.rs stub-server:
  `dispatch_routing_survives_a_real_worker_start`. Mode `shadow`, a dispatch
  through `handle_lead_host_tool`, then the worker's first request arrives and
  `team_status` carries `routing.laya_tier`.

**Done:**

- [ ] clippy clean
- [ ] tests green
- [ ] bins build
- [ ] `plan_update decision` recording where `[routing]` lives

**Do not touch** src/agent or src/headless (step 1), or ledger.rs
(step 4).

### Step 4: Overrides, reroutes and outcomes in the log and the report

**Files:** `src-tauri/src/mnemo2/team.rs` (`change_model` source,
`resume` with tier, terminal-status hooks), `src-tauri/src/mnemo2/ipc.rs`
(the mission-control `change_model` call at :944 passes `ChangeSource::User`),
`src-tauri/src/mnemo2/ledger.rs` (Effort columns), tests.

**New code:**

1. `pub enum ChangeSource { Lead, User }`. `change_model(&self, worker_id,
   model, reasoning_effort, source: ChangeSource)`; every caller passes the
   right source. If clippy's argument count trips, group the arguments in a
   struct, as step 2 of 40eef714 did with `WorkerSessionSpec`.
2. In `change_model` and in `resume` with a `tier`/`model`, after the switch
   succeeds: when the worker's tier changes (map the old and new model spec to
   a tier with `tier_of(spec) -> Option<&str>`, new, the first tier whose list
   names the spec), append `LeadReroute` or `UserReroute` with the §4.7
   fields, and set `routing.nudged = None` after copying it into the row's
   `nudged`.
3. At the place the ledger marks a worker merged, and where a worker becomes
   failed-and-stopped or abandoned, append `Outcome` with `outcome_label(…)`
   (step 2) computed from the worker's rows. Read the worker's rows by
   scanning the log for its `worker_id`, which is cheap at this size, or keep
   them on `WorkerRecord.routing` as a `Vec` (decide in the step and record it
   with `plan_update decision`).
4. ledger.rs `render_effort`: two columns after `Models`. `Laya` shows the
   tier and confidence (`average 0.62`, `unsure 0.41`, `-`). `Routed by` shows
   `lead`, `laya` or `lead (override)`, and `+ user reroute` or
   `+ lead reroute` when one happened.

**Rules:**

- A reroute that keeps the tier (a different model inside the same tier) is
  logged with `from_tier == to_tier` and never counts as an override in the
  stats.
- Mnemo1 is untouched.

**Tests (written first):**

- `user_model_change_logs_a_user_reroute`;
- `lead_resume_onto_another_tier_logs_a_lead_reroute`;
- `same_tier_switch_logs_without_counting_as_override`;
- `merge_appends_an_outcome_with_the_derived_label` (strong label after a
  reroute up);
- `failed_worker_outcome_has_no_label`;
- ledger `effort_table_shows_laya_and_routed_by`;
- ledger `old_records_render_dashes_for_laya`.

**Done:**

- [ ] clippy clean
- [ ] tests green
- [ ] bins build

### Step 5: Nudges

**Files:** `src-tauri/src/mnemo2/team.rs` (the worker event pump, the
`team_message` / `route_fix` paths, the review path), tests in team.rs and
runtime.rs.

**New code:**

1. `WorkerRecord` gets `#[serde(default)] strikes: u32` and
   `#[serde(default)] last_call: Option<String>` (the canonical call
   signature).
2. The new `fn call_signature(name: &str, args: &serde_json::Value) -> String`
   serializes `{name, args}` with sorted keys, dropping any `id` /
   `tool_use_id` key.
3. In the pump, on each worker tool result:
   - update `last_call`;
   - if failed or identical to `last_call`, `strikes += 1`, else `strikes = 0`;
   - at `strikes == 3`, when a tier above the current one exists and
     `routing.nudged` is None, queue the nudge notice (§4.6 text), set
     `routing.nudged = Some(tier)` and append a `Nudge` row.
4. Drift: in `team_message` and `team_route_fix`, when mode is not `off` and
   the text has at least `min_words` words, spawn a detached task. It
   classifies the text (same question, same 3 s timeout, same permit) and, if
   it clears on a tier above the worker's, sends the nudge with reason
   `drift`. It never blocks or fails the message.
5. Review: in `review(…)`, after storing, when the new findings contain a
   `high` and `review_rounds >= 2`, send the nudge with reason `review`.
6. On a terminal status with `routing.nudged.is_some()` and the tier
   unchanged, append `NudgeIgnored`.

**Rules:**

- At most one nudge per worker. A reroute clears `nudged`, which allows one
  more on the new tier.
- No nudge when the worker is already on the top enabled tier.
- `nudges = false` disables all three kinds.
- The notice is lead-facing data, prefixed like every TeamManager notice.

**Tests (written first):**

- `three_failing_tool_calls_nudge_once`;
- `identical_calls_count_as_strikes`;
- `a_success_resets_the_strikes`;
- `no_nudge_on_the_top_tier`;
- `drift_message_nudges_when_laya_reads_it_harder`;
- `drift_never_blocks_the_message` (the stub sleeps; the message returns
  immediately);
- `second_high_finding_review_nudges`;
- `ignored_nudge_is_logged_at_merge`;
- `nudges_off_disables_all_three`.

**Done:**

- [ ] clippy clean
- [ ] tests green
- [ ] bins build

### Step 6: Settings, mission control and the eval tool

**Files:**

- `src-tauri/src/mnemo2/laya_settings.rs` (status and save of `[routing]`,
  plus a `routing_log_summary` command);
- `src-tauri/src/mnemo2/mod.rs` (invoke_handler);
- `mnemo2/src/components/LayaPage.svelte`, `mnemo2/src/lib/layaSettings.ts`;
- the worker card component in mission control (grep
  `WorkerModel.svelte` / the worker card that shows the model chip);
- `mnemo2/src/lib/types.ts` (`Worker.routing`);
- the eval tool `src-tauri/src/bin/mnemo2_route_eval.rs` (new) plus
  `src-tauri/tests/fixtures/route_eval.jsonl` (new, the hand set of design
  §4.8);
- vitest tests beside the components.

**New code:**

1. `routing_log_summary() -> RoutingLogSummary { decisions_by_mode,
   agreement_rate, laya_accuracy, lead_accuracy, labelled, overrides_by_source,
   nudges, nudges_acted_on, eval_date }`, computed by reading
   dispatch_routing.jsonl.
2. The LayaPage Routing block:
   - mode radio, with `suggest` and `auto` disabled until the thresholds in
     §4.5 are met (a "force" link with a confirm);
   - thresholds with the eval date;
   - fallback tier, nudges toggle and per-tier effort;
   - the summary numbers.
   - Keep Mnemo1's `[general.routing]` lanes block, relabelled "Mnemo1
     per-turn lanes (not used by Mnemo2 workers)".
3. The worker card: a routing chip from `worker.routing` and a nudge chip.
4. The eval tool:
   - `mnemo2_route_eval [--history] [--laya-url URL]`. It attaches to the
     running Mnemo2's Laya (or a URL) and classifies the hand set (plus the
     history set with `--history`) one prompt at a time;
   - it prints the threshold table and the misses as in design §4.8, and the
     suggested `min_confidence_<n>`;
   - its pure parts (corpus parsing and table math) are unit-tested; the
     network part is not run in tests.

**Tests:**

- vitest:
  - `LayaPage` renders each mode;
  - auto is disabled with fewer than 30 labelled decisions;
  - save sends `[routing]`;
  - the worker card shows `Laya: average 0.62` and the nudge chip.
- rust:
  - `summary_counts_agreement_and_accuracy` over a fixture log;
  - `eval_table_picks_the_lowest_threshold_at_90_percent` over fixed scores;
  - `history_corpus_parses_plan_steps_and_review_tasks` over two fixture
    files.

**Done:**

- [ ] svelte-check and vitest clean
- [ ] clippy clean
- [ ] tests green
- [ ] bins build

### Step 7: Verification (lead only)

1. Rebuild Mnemo2 and set mode `shadow`.
2. Run the eval tool against the live Laya and record the measured thresholds
   in settings. If accuracy at any threshold that routes at least 50% of the
   hand set is below 80%, **stop**: tune the descriptions (step 2 constants)
   and rerun before going on.
3. Run one real plan in shadow. Check the `route` rows, the dispatch result
   lines, `team_status.routing`, the worker card chip, and the Effort table's
   Laya column.
4. Change one worker's model by hand in mission control and check the
   `user_reroute` row.
5. After about 30 dispatches, read the summary and decide with the user
   whether to enable `suggest`.

### Acceptance scenarios

- With mode `off`, dispatch behaves exactly as today: no classifier call, no
  log row, `tier` required.
- With mode `shadow`, every implementation dispatch appends one `route` row
  with Laya's tier, confidence and the lead's tier. The worker runs on the
  lead's tier, and the result says what Laya would pick.
- With mode `suggest`, `tier: "auto"` runs on Laya's tier when it clears, and
  is refused with "Laya is unsure (x.xx); pass a tier" when it does not.
- No Mnemo2 worker, audit or lead session writes rows to routing.jsonl or is
  routed per turn. Mnemo1 sessions still are.
- A user's model change in mission control appends a `user_reroute` row. A
  lead `team_resume` onto another tier appends a `lead_reroute` row.
- Three failing tool calls in a row produce exactly one nudge notice to the
  lead and never change the worker's model.
- A merged worker's `outcome` row carries a derived label. The plan's review
  report shows Laya and Routed-by columns.
- A dispatch never waits more than 3 s on Laya.

---

## 6. Open questions for the user

1. **The default mode after this ships.** `shadow` is recommended, because it
   costs about a 50–300 ms classifier call per dispatch and nothing else, and
   starts building the corpus at once. Or should it stay `off` until you turn
   it on?
2. **The lead in `auto` mode.** I lean towards stopping at `suggest`: the
   lead (Claude) is a strong router, and Laya's job is to make cheap routing
   possible when the lead is Mnemo2's own model. `auto` is mainly for the
   Mnemo2-internal lead. Should `auto` be offered at all for an external
   Claude lead?
3. **Laya-routed effort.** Should the per-tier effort apply, or should only
   the tier be routed?
4. **The fine-tune.** It is out of scope here. When the dispatch log has
   about 200 strong labels, a later plan can add a `dispatch_routing` corpus
   to the startup fine-tune (src-tauri/src/ipc/finetune.rs, which today
   exports only failure triage). Do you want that in this plan as a step 8
   instead?
5. **Mnemo1's per-turn gate.** It keeps its two-label question and 0.80
   gate. Its 94 logged rows show the same weakness. Should a follow-up give
   Mnemo1 the same three-label question and calibrated gate?

# Unified tiers: one model ladder for Mnemo1 and Mnemo2

Status: draft for review (2026-11-04). Not on the plan board yet.

## 1. Why

Mnemo has two separate ways of saying "this kind of work runs on that model":

| | Mnemo2 **Tiers** | Mnemo1 **lanes** |
|---|---|---|
| Where | `endpoints.toml` `[tiers]` | `config.toml` `[general.routing]` |
| Shape | 3 fixed lists of models: `difficult`, `average`, `simple` | 6 single model slots: `cheap`, `routine`, `capable`, `lane_medium`, `lane_high`, `escalate` |
| Purpose text | one line each, hard-coded (`team.rs` `TIER_PURPOSES`) | a blurb each, hard-coded (`mnemo2/src/lib/layaSettings.ts` `ROUTING_LANES`) |
| Who picks | the lead (`team_dispatch tier`), Laya in suggest/auto mode | Laya at the first prompt; the reflex at each plan step |
| Escalation | none (a human switches the model) | an `escalate` rung after a failed cycle |
| Settings | Settings > Tiers | Settings > Laya, a block marked "Mnemo1 … not used by Mnemo2 workers" |

The user's verdict: the lanes are better defined (more levels, an escalation target) and Mnemo2's three tiers are naive. Two lists for the same idea also means setting models up twice, and the routing log, the kNN overlay and the eval all speak a 3-tier dialect.

On this machine today: `[tiers]` = difficult Kimi k3.1-pro, average GLM 5.3, simple DeepSeek v4.1 flash; every Mnemo1 lane is unset.

## 2. Goal

One ordered **tier ladder**, defined once (Settings > Tiers), read by:

- the Mnemo2 lead (`team_dispatch tier`, the tool description, the plugin skills);
- Mnemo2 dispatch routing (Laya + kNN), nudges and reroutes;
- Mnemo1's first-prompt routing and its step-lane ladder;
- the routing log, the kNN overlay, the eval, team_status, mission control.

Each tier carries a written definition, its models and its default effort. Escalation becomes a first-class part of the ladder for both apps.

## 3. The ladder

### 3.1 Default tiers

Five tiers, cheapest first. Ids are stable keys; names and definitions are editable.

| # | id | Default definition (what work belongs here) | Maps from Mnemo2 | Maps from Mnemo1 |
|---|---|---|---|---|
| 1 | `trivial` | Rename, typo, version bump, config value, copy change, a log line, dead code, doc comment, a single-file mechanical edit, a lookup or a check. | `simple` | `cheap` |
| 2 | `routine` | Implement a specified function, add tests for existing code, wire an existing API into a new caller, a UI component from a spec, a schema or DTO field, follow an existing pattern, refactor inside one module. | `average` | `routine` |
| 3 | `medium` | A normal multi-file change with a clear spec: a feature across a few modules, an endpoint with its tests and UI, a migration of call sites, a non-trivial refactor. | — (new) | `lane_medium` |
| 4 | `high` | Subtle bugs, races, flaky tests, data loss, performance regressions, security, persistence formats, cross-module or shared-contract changes, concurrency, design decisions the spec leaves open, bug_fixing plans. | `difficult` | `capable`, `lane_high` |
| 5 | `escalate` | Work a lower tier visibly failed at: repeated review failures, a stuck worker, an audit that keeps finding real problems. Not chosen for fresh work. | — (new) | `escalate` |

The definitions for `trivial`, `routine` and `high` are the existing routing-question descriptions (`route_question.rs` `DESCRIBE_*`) plus the old tier purposes, so nothing the lead or Laya relied on is lost.

### 3.2 Rules

- **Order matters.** The ladder is ordered cheapest to strongest. "One tier up" and folding (below) use that order.
- **The lowest tier must have a model** (decided 2026-11-04). It is the default model: Settings refuses to save a ladder whose `trivial` tier is empty, and the migration fills it from the old `simple` tier or, if that is empty too, from the configured default model.
- **Empty tiers fold down** (decided 2026-11-04). A tier with no model runs on the nearest tier **below** it that has one; because the lowest tier always has a model, every tier resolves. With today's config, `medium` runs on `routine` until the user gives it a model, and `escalate` runs on `high`. Folding is shown in Settings ("uses Routine's model") and in the dispatch result ("medium has no model; ran on routine").
- **`escalate` is never a first pick.** It is offered only to the escalation paths (§6), never to Laya or the kNN for fresh work. The lead can still name it explicitly in `team_dispatch` / `team_resume`.
- **Models per tier stay an ordered list.** The first available model runs, as in Mnemo2 today. A Mnemo1 lane's single model becomes a one-entry list.
- **Effort per tier** moves here too (today `mnemo2.toml [routing.effort]`). A tier may name a default reasoning effort; an explicit `reasoning_effort` on a dispatch still wins.
- **Fixed five tiers** (decided 2026-11-04). Names, definitions, models and effort are editable; the set and order are not. Custom tiers are deferred and may never be needed.

## 4. Storage

`endpoints.toml` keeps the ladder (it already holds `[tiers]` and is shared by both apps):

```toml
[[tier]]
id = "trivial"
name = "Trivial"
definition = "Rename, typo, version bump, …"
models = ["Ollama Cloud/deepseek-v4.1-flash:1017"]
effort = "low"            # optional

[[tier]]
id = "routine"
…

[[tier]]
id = "escalate"
role = "escalate"         # never a first pick
models = []
```

- **Reading the old shape.** `Tiers` (src/config/endpoints.rs) parses the legacy `[tiers] difficult/average/simple` table once and turns it into the ladder: simple→`trivial`, average→`routine`, difficult→`high`, `medium` and `escalate` empty, default definitions. The first save writes `[[tier]]` and drops the legacy table.
- **Old names go away** (decided 2026-11-04). `simple`, `average` and `difficult` are not accepted by `team_dispatch` or any setting after the change. The one migration table is used only to convert stored data once: `endpoints.toml`, each project's `team.json` worker records (`tier`, reroutes, routing), and on read in the routing log and `routing.jsonl` (old rows keep their bytes; readers map them).
- **Mnemo1 lanes migrate once.** On first load with the new code, each set lane in `[general.routing]` is added to the mapped tier's list (front of the list for an otherwise empty tier, end of the list otherwise, never a duplicate): cheap→trivial, routine→routine, lane_medium→medium, capable and lane_high→high, escalate→escalate. The lane fields are then removed from `config.toml` (a `.bak` copy is written first). `threshold`, `first_prompt_min_confidence` and `enforce` stay in `[general.routing]`: they are Mnemo1 routing switches, not models.
- **One loader.** `Tiers` becomes `TierLadder { tiers: Vec<Tier> }` with `get(id)`, `ordered()`, `offered()` (tiers with a model, excluding `escalate`), `fold(id)`, `next_up(id)`, `escalation()` and `first_model(id)`. Every consumer goes through it; nothing hard-codes a tier id again except the migration table.

## 5. Routing

### 5.1 The Laya question: four labels, one per pickable tier

Decided 2026-11-04: Laya chooses among **all tiers except `escalate`**, and is retrained for it.

- **Four neutral labels**, one per pickable tier: `trivial`→`trivial`, `routine`→`routine`, `involved`→`medium`, `hard`→`high`. Labels stay neutral words, never tier names (laya-router: tier-shaped names skew the checkpoint). `involved` gets a new byte-stable description built from the `medium` definition in §3.1.
- **The question text is fixed in code** (`route_question.rs`), not built from the user-editable definitions, so editing a definition never invalidates the eval or the training data. The defaults in §3.1 and the label descriptions start out identical; the Settings preview shows both.
- **Gates for four labels.** The 4-label top probability has a floor of 0.25, so it needs its own gate: `min_confidence_4` (provisional default 0.40) next to the existing 3- and 2-label gates, keyed by how many labels a question offered. Folded-away labels (a tier with no model is still offered: it folds down at run time, §3.2) do not change the label count; only an explicitly offered set does.
- **Re-baseline with the eval.** The eval corpus (`route_eval.rs`) is relabelled into the four labels (the existing `routine`/`hard` cases split where `medium` fits) and re-run; Apply sets `min_confidence_4`.
- **Retraining.** The installed laya 0.3.20 has no `laya.finetune`, so training runs outside the app until it does: Mnemo exports a labelled dataset (`~/.mnemo/laya/training/route-dataset.jsonl`: text + tier label from lead picks, reroutes, escalations-excluded outcomes, the eval corpus) in the shape `src-tauri/src/ipc/finetune.rs` already uses for failure triage, and the startup fine-tune hook picks it up automatically once a laya version with a training surface is installed. Until then the **kNN overlay** is the online learner: it already uses tier ids as labels, so it learns all four pickable tiers from what actually happened.
- The old 3-label question constants are removed with the old names.

### 5.2 Mnemo2 dispatch

- `team_dispatch` `tier` becomes an enum of ladder ids (offered tiers plus `escalate`), and the tool description lists each tier's name, definition, models and price, generated from the ladder (replacing `TIER_PURPOSES`).
- A dispatch naming an empty tier folds down (§3.2) and says so in the result ("medium has no model; ran on routine").
- Old ids are refused with the list of new ones (no aliases).
- The routing log's tier fields (`tier`, `lead_tier`, `ran_tier`, `from_tier`, `to_tier`, `final_tier`, `label`) hold ladder ids from now on.

#### Audits run on a tier (user request, 2026-11-04)

Today an audit with no `model` silently runs on the global default model (`[general] default_model`, Kimi on this machine), its model shows as empty in mission control until its session reports, and the UI and `change_model` refuse to switch it (team.rs ~4391). Instead:
- `team_audit_dispatch` takes an optional `tier` (any ladder id, folded down like a dispatch); without `tier` or `model` it runs on **`high`**. The tool description lists the tiers like `team_dispatch`'s.
- The audit record stores its resolved model and tier **at dispatch**, so team_status and mission control show them from the first moment (no more `model: null`).
- Before an audit has produced its report, the user may switch its model or tier in mission control (and the lead via `team_resume` + `tier`); once it has reported (the finished-audit rule of plan 5779641d) it stays locked.
- Audits are never routed by Laya or the kNN (they verify, they are not fresh work), and their switches are not training labels.

### 5.3 Mnemo1

- **First prompt** (route-once, `model_routing.rs`): the decision's tier comes from §5.1 (kNN first once Mnemo1 has its own labelled rows, else Laya), and the session runs on that tier's first model. `RouteTarget::{Cheap, Routine, Capable}` become ladder ids.
- **Step lanes** (`step_lanes.rs` / reflex): the reflex's complexity answer maps onto the ladder: `small`→`trivial`, `medium`→`medium`, `high`→`high`, `escalate`→`escalate` (each folded if empty). `LaneRung` maps to ladder ids instead of lane fields.
- `enforce` and the thresholds are unchanged: shadow by default.

## 6. Escalation

Both apps get the same escalation rule, built on the existing nudge machinery (Mnemo2 `nudges`, Mnemo1 `Strikes`):

- **When:** a worker's second failed review round on the same finding, a route-fix round that did not fix its finding, a strike streak (repeated or failing tool calls), or an audit that finds a high-severity problem after a fix round.
- **What:** a nudge "escalate to <next tier up>" for the lead (Mnemo2) or the turn loop (Mnemo1). In shadow and suggest modes it is a suggestion the lead accepts with `team_resume` + `tier`; in auto mode Mnemo2 switches the idle worker itself without asking (decided 2026-11-04) and tells the lead in a notice. A worker already on the highest non-escalate tier is offered `escalate`.
- **Logged** as a reroute row with `source: "escalation"`, so the kNN learns from it.

### 6.1 Fix rounds may step down (suggested by the user, 2026-11-04)

The opposite case: a strong-tier worker whose review left only small, exact fixes (a rename, a missing test, a wording change, a one-line guard) does not need its expensive model for the fix round.

- **`team_route_fix` takes an optional `tier`.** The fix round runs on that tier's model in the **same session** (transcript, worktree and branch kept, as `team_resume` + `tier` does today), so the cheaper model starts with full context.
- **Who picks:** the lead, explicitly; or, when the lead passes `tier: "auto"`, the routing question (Laya + kNN, §5.1) classifies the findings' text (title, detail, file) — the same question as for dispatch, so no new calibration. In shadow mode the pick is only logged; in suggest/auto it is used when it clears the gate.
- **Never below what the finding needs:** a finding marked `high` severity, or touching a file the plan marks as a shared contract, never steps down automatically (the lead can still choose).
- **Back up when it fails:** if the stepped-down fix round does not close its finding (the re-review keeps it open), the next round returns to the worker's previous tier — the escalation rule above applies from there.
- **Logged** as a reroute row with `source: "fix_downshift"` plus the outcome of that fix round (closed or not), so the kNN learns which findings a cheap model can fix.
- **Audits:** a high-risk worker still needs a fresh audit after any fix commit, whatever tier made it.

## 7. Settings > Tiers (the one place)

Decided with the user (2026-11-04): the Tiers page and the routing half of the Decisions (Laya) page become **one Tiers page**. Everything about "which tier, and who picks it" lives there. The Mnemo1 lane block disappears.

### 7.1 Top of the page: who picks the tier

The first and most prominent control answers one question: **Does Laya help pick the tier?**

| Choice | Meaning (Mnemo2 dispatch / Mnemo1 sessions) |
|---|---|
| **No: the lead picks** (default) | Laya is never asked. `team_dispatch` needs a tier. Mnemo1 keeps the session's model. Model switches and outcomes are still logged as labels (so the kNN can learn for later). |
| **Watch only** (shadow) | Laya and the kNN are asked and logged; the lead's tier always runs. Mnemo1: logged, not switched (`enforce = false`). |
| **Suggest** | A dispatch with tier `auto` runs on the routed tier when it clears the gate and is refused when it does not. Mnemo1: unchanged (shadow). |
| **Auto** | Like suggest, with a fallback tier when the gate is not cleared. Mnemo1: unchanged (shadow), unless its own settings turn enforcement on. |

This one control drives both apps: it writes `mnemo2.toml [routing] mode` and `[general.laya] routing` (off only for "No"); `[general.routing] enforce` is set only in Mnemo1's own Classifier settings. Suggest and Auto keep their existing unlock rule (30 labelled decisions) with the "force" link.

**When Laya is not ready the control says so, in place:**
- *Not installed*: the three Laya choices are disabled with "Laya is not installed — Install Laya (≈ 1.2 GB)" and an install button that runs the existing installer with its progress bar. "No: the lead picks" stays selected.
- *Installed but off* (`[general.laya] enabled = false`): "Laya is turned off — Turn on" inline.
- *Installing / starting / failed*: the existing status words (`layaStatusHint`) with the matching action (retry, view log).
- *Ready*: a green "Laya ready" line with the checkpoint name and, when the kNN has learned something, "kNN: N labelled workers (answers from 30)".
A mode that is saved but cannot run (e.g. Shadow saved, Laya later uninstalled) shows a warning on the control instead of silently doing nothing.

Picking a choice saves at once (as the mode radio does since plan 50eadde4).

### 7.2 The ladder

Per tier, in ladder order:
- name, definition (multi-line), models (ordered, drag to reorder, from the configured endpoints, with price), default effort;
- a "uses <lower tier>'s model" hint when it has no model; the lowest tier shows "required: the default model" and cannot be emptied;
- `escalate` shown with its role ("only after a lower tier failed");
- When Laya helps pick: each pickable tier shows the Laya label it answers to (`trivial`, `routine`, `involved`, `hard`).

Below the ladder:
- **Preview**: the generated `team_dispatch` tier text, exactly as the lead will see it.

### 7.3 Routing details (collapsed by default, only when Laya helps pick)

- Gates: `min_confidence_3`, `min_confidence_2`, `min_words`, fallback tier (auto mode).
- kNN: `knn_min_rows`, `knn_min_share`.
- Nudges and escalation on/off (§6).
- Mnemo1: first-prompt gate and the step-lane threshold.
- **Eval**: run, last result, apply suggested gates (moved from the Laya page).
- The routing log summary (decisions, labelled rows, by mode).

### 7.4 What stays elsewhere

Decided 2026-11-04: **the Decisions tab goes away.** Laya itself and its non-routing uses move to a "Laya" section at the bottom of the Tiers page: install / status / checkpoint, and the flags for memory auto-typing, failure triage (+ kNN), tool-choice steering and auto fine-tune. The top control's install / turn-on actions (§7.1) and this section share one Laya status source, so they never disagree. Anything that linked to the Decisions page (status-bar Laya chip, notices, the docs) links to Settings > Tiers, Laya section.

## 8. Everything that names a tier today

Found by grep for the three ids and the lane fields; each is in scope.

- Config: `src/config/endpoints.rs` (`Tiers`), `src/config/general.rs` (`RoutingConfig` lanes), `src/config/settings_dto.rs`, `src/config/patch.rs`, `src/headless/setup/endpoint_detail.rs` (`TiersDto`).
- Kernel routing: `src/agent/route_question.rs` (label→position mapping), `src/agent/model_routing.rs` (`RouteTarget`), `src/agent/step_lanes.rs` (`LaneRung`), `src/agent/route_knn.rs` (`TIER_LADDER`), `src/agent/reflex.rs` (complexity mapping only).
- Mnemo2: `src-tauri/src/mnemo2/team.rs` (`TIER_PURPOSES`, tier validation, `tier_of`, reroutes, `final_tier`, outcome labels, nudges), `dispatch_routing.rs` (`label_tier`, `TIER_LADDER`, effort), `route_eval.rs`, `laya_settings.rs`, `model_settings.rs`, `ledger.rs`, `runtime.rs` (tool description test), the MCP tool schema.
- UI: `mnemo2/src/lib/settingsEngine.ts`, `settingsState.ts`, `layaSettings.ts` (`ROUTING_LANES`), `workerRouting.ts`, the Tiers settings component, `LayaPage.svelte`, `WorkerRow`/`WorkerDetail` chips.
- Plugin text: `mnemo2/plugin/skills/mnemo-lead` (Tiers section), the MCP server instructions, the SessionStart tier summary.

## 9. Steps (proposed)

1. **Ladder model and migration** (kernel config): `TierLadder` with the legacy `[tiers]` reader, the required lowest tier, fold-down / next-up / escalation helpers, the Mnemo1 lane migration (+ `.bak`), the one-time old-id conversion table, round-trip tests. — difficult
2. **Four-label routing question** (kernel): `involved` label + description, `min_confidence_4`, label→tier mapping, removal of the 3-label constants, eval corpus relabelled into four labels, the training-dataset export (§5.1). — difficult
3. **Mnemo2 on the ladder**: team.rs (tool schema + generated description, validation, fold-down result note, reroutes, outcomes, `team.json` conversion), dispatch_routing.rs, route_knn.rs (tier ids, old-row mapping), route_eval.rs, ledger. — difficult
4. **Mnemo1 on the ladder**: model_routing.rs, step_lanes.rs, the reflex mapping, settings DTO/patch, `routing.jsonl` old-row mapping. — average
5. **One Tiers page** (§7): the "who picks the tier" control with Laya readiness states and inline install/turn-on, the ladder editor, the collapsed routing details and eval moved from the Laya page; the Decisions page shrinks to Laya's non-routing uses. — average (UI) after a small backend step for the combined save (mode + Mnemo1 enforce/laya.routing in one call).
6. **Escalation and fix-round step-down** (§6, §6.1) for both apps on the nudge machinery: auto-switch up in auto mode, and `team_route_fix` with an optional `tier` (explicit or `auto`) that runs the fix round on a cheaper tier and returns to the previous tier if the finding stays open. — difficult
7. **Plugin text**: mnemo-lead skill, server instructions, SessionStart summary. — simple
8. **Live check (lead)**: migration on the real config, a dispatch per tier, a folded `medium` dispatch, the four-label eval and Apply, an escalation, Mnemo1 first prompt, the Tiers page with Laya ready / not installed.

Order: 1 → 2 → (3 ∥ 4) → (5 ∥ 6) → 7 → 8.

## 10. Decisions and open questions

Decided by the user, 2026-11-04:
1. Five tiers: trivial, routine, medium, high, escalate. Old names (simple, average, difficult) go away; no aliases.
2. Laya chooses among all tiers but escalate (four labels) and is retrained for it.
3. Empty tiers use the tier below; the lowest tier must have a model (the default model).
4. Auto mode may escalate an idle worker without asking.
5. No custom tiers for now (maybe never).
6. One Tiers page; "does Laya help pick the tier" is the prominent top setting and shows Laya's readiness.
7. Laya's non-routing settings move to the bottom of the Tiers page; the Decisions tab is removed.

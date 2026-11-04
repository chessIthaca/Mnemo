# OpenSpec support: current-state specs and schemas on Mnemo's plan board

Status: spec, in the backlog (2026-11-04). Not planned yet. Decided with the user: adopt OpenSpec's **file formats and ideas**, keep Mnemo's own workflow; support OpenSpec **schemas** (templates) and give them a settings page.

## 1. Why

Mnemo already has most of what OpenSpec offers, under other names. What it lacks is one place that says **what the system does now**.

| OpenSpec | Mnemo today |
|---|---|
| *(nothing kept; `/opsx:explore` is a chat)* | backlog (`.coding/backlog.jsonl`, "plans-to-be") |
| `/opsx:propose`: change folder with proposal, design, tasks, spec deltas | `plan_create`: goal, context, steps, interfaces, acceptance scenarios |
| `/opsx:apply` | `team_dispatch` → review / audit → `team_merge`, with gates OpenSpec lacks |
| `/opsx:archive`: deltas merged into `openspec/specs/`, change moved to `archive/` | plan close + team review report under `.coding/reviews/` |
| **current-state specs** (`openspec/specs/<capability>/spec.md`) | **missing** |

The gap shows in practice. `SPEC:`/`DECISION:` memory records are an append-only pile: a rule change supersedes records one by one and nothing gathers "what dispatch routing does today". On 2026-11-04 the rule "routing off writes no rows" was spread over four records that a worker had to supersede individually. Plans' acceptance scenarios, which are exactly current-state requirements, vanish when the plan closes.

## 2. Decisions

1. **Formats, not the tool.** Mnemo writes and reads OpenSpec-compatible files. It does not require the OpenSpec CLI (Node 22+, telemetry on by default) and does not install `/opsx:*` slash commands; those would compete with the plan board, and Mnemo's workers (Kimi, GLM, DeepSeek) are driven by Mnemo's tools, not slash commands.
2. **The plan board stays the only home of in-flight work.** OpenSpec's `openspec/changes/<name>/` is not used as live state. On close, Mnemo writes an OpenSpec-style archive folder so the history is readable by OpenSpec tools.
3. **Optional compatibility.** If the `openspec` CLI is on PATH, Mnemo offers `openspec validate` as an extra check. Nothing depends on it.
4. **Schemas are supported.** A project picks an OpenSpec schema (default `spec-driven`, any community or own schema); the schema shapes what a plan must contain.
5. **One settings page** for specs and schemas (§7).

## 3. Layout

```
openspec/
  config.yaml                     # schema, context, rules (OpenSpec format)
  specs/<capability>/spec.md      # current-state specs
  schemas/<name>/schema.yaml      # project schemas (+ templates/)
  changes/archive/<date>-<plan-slug>/   # written by Mnemo on plan close
```

- `openspec/` at the repo root, as OpenSpec expects, so its tools work unchanged.
- The existing `.coding/specs/` (free-form older notes: `headless-free-agent*.md`, `job1-host-extraction.md`) moves to `.coding/design/` to avoid two "specs" folders.
- User-level schemas in `~/.local/share/openspec/schemas/` are read too (OpenSpec's own resolution order).

## 4. Current-state specs

Format (OpenSpec): one `spec.md` per capability.

```markdown
# Dispatch routing

## Purpose
How Mnemo2 picks a worker's tier at dispatch.

## Requirements

### Requirement: Model changes are training labels
The system SHALL append a reroute row to dispatch_routing.jsonl for every user or lead model change, in every routing mode.

#### Scenario: Routing off
- **WHEN** routing mode is off and the user switches a worker's model
- **THEN** one user reroute row is appended
- **AND** team_status shows the destination tier
```

- A **capability** is a stable area name (kebab-case folder): `dispatch-routing`, `tier-ladder`, `rebuild`, `worker-lifecycle`, …
- `memory_search` and the code graph learn the spec files (indexed like knowledge records), so the lead and workers find requirements the same way they find decisions today.
- `SPEC:` memory records become pointers into the spec files instead of a second source of truth. `DECISION:`, `BUG:` and `HOW:` records are unchanged.

## 5. Plans carry spec deltas

`plan_create` / `plan_update` gain `spec_deltas`:

```json
{ "capability": "dispatch-routing",
  "added":    [{ "requirement": "…", "text": "The system SHALL …", "scenarios": [{ "name": "…", "when": ["…"], "then": ["…"] }] }],
  "modified": [{ "requirement": "<existing title>", "text": "…", "scenarios": [ … ] }],
  "removed":  [{ "requirement": "<existing title>", "reason": "…" }],
  "renamed":  [{ "from": "…", "to": "…" }] }
```

- The board renders them into the plan file as OpenSpec delta sections (`## ADDED Requirements` etc.) under a `## Spec changes` heading.
- Workers receive them in the plan context, next to the acceptance scenarios. A delta's scenarios count as acceptance scenarios (high-risk dispatch needs at least one; a delta satisfies that).
- Validation at `plan_create`/`plan_update`: MODIFIED/REMOVED/RENAMED must name a requirement that exists in the current spec (or in an earlier delta of the same plan); ADDED must not collide; every requirement has SHALL/MUST text and at least one scenario with WHEN and THEN.
- The reviewer and the audit worker get the deltas as the checklist: each requirement's scenarios are what "matches the spec" means.

## 6. Closing a plan = archive

When a plan's last step merges and the team review passes, before the plan closes:

1. **Merge the deltas** into `openspec/specs/<capability>/spec.md` (OpenSpec's archive semantics: RENAMED, then REMOVED, MODIFIED, ADDED; a missing capability file is created).
2. **Write the archive folder** `openspec/changes/archive/<YYYY-MM-DD>-<plan-slug>/` with the files the project's schema generates (for `spec-driven`: `proposal.md` from goal + context "why", `design.md` from context + decisions + interfaces, `tasks.md` from the steps with their done marks, `specs/<capability>/spec.md` deltas), plus `.openspec.yaml` naming the schema and the plan id.
3. **Gate:** the board refuses to close a plan whose deltas cannot be applied (a MODIFIED requirement no longer exists, a conflict with a delta another plan merged meanwhile). The notice names the conflict; the lead fixes the delta with `plan_update` and the close is retried. This mirrors the existing "record a memory before close" gate.
4. Commit the spec and archive changes on the lead branch with the review report (the merge already allows `.coding/` changes; `openspec/` gets the same allowance).

## 7. Schemas

### 7.1 What an OpenSpec schema is

`openspec/schemas/<name>/schema.yaml` + `templates/*.md`:
- `artifacts[]`: `id`, `generates` (file or glob), `template` (file in `templates/`), `instruction` (guidance for writing it), `requires` (artifact ids).
- `apply`: `requires` (artifacts that must exist before implementation) and `tracks` (the progress file).
- `openspec/config.yaml`: `schema` (default), `context` (project info added to every artifact), `rules` (per-artifact guidance by artifact id).
- Resolution order: a change's `.openspec.yaml` → `config.yaml schema` → `spec-driven`.

### 7.2 How Mnemo uses one

| Schema part | Mnemo behaviour |
|---|---|
| `artifacts[]` | Each artifact becomes a **plan section** the lead fills in `plan_create` (`sections: { <artifact id>: "<markdown>" }`). The built-in fields map to `spec-driven` artifacts: proposal ↔ goal + why, design ↔ context/decisions/interfaces, tasks ↔ steps, specs ↔ `spec_deltas`. Extra artifacts of other schemas (e.g. an epic, a research note, a runbook) are free-form sections. |
| `template` | The section skeleton: `plan_create` returns it when a required section is missing; the board refuses a section that is still the unchanged template. |
| `instruction` + `config.yaml rules[artifact]` | Shown to the lead in the `plan_create` refusal/help text and given to workers in the plan context for that section. |
| `requires` | Section order: a section whose prerequisites are empty is refused (with the reason). |
| `apply.requires` | `team_dispatch` refuses until those sections exist. |
| `apply.tracks` | Mapped to the plan's steps (the board already tracks them); written as that file in the archive folder. |
| `config.yaml context` | Added to every worker brief (after the plan, before the step). |
| `generates` | The archive folder layout on close (§6). |

A plan records its schema (`schema: <name>@<version>`) when created; a later schema change never rewrites open plans.

### 7.3 Compatibility limits

- Mnemo supports schema fields listed above. Unknown fields are kept and ignored, with a warning in Settings.
- A schema without a specs-like artifact still works; such plans carry no spec deltas and the close step skips merging.
- Community schemas are copied in (like OpenSpec: no install command); Mnemo never runs code from a schema.

## 8. Settings > Specs page

1. **Schema**
   - Active schema picker: `spec-driven`, project schemas, user-level schemas; shows where each resolves (like `openspec schema which`).
   - **Add schema** from a folder or a git URL (community schemas such as `epic-driven`, `intent-driven`): validated (schema.yaml parses, every template exists, `requires` has no cycle, ids unique) before it is copied into `openspec/schemas/<name>/`.
   - **Fork** the current schema under a new name (like `openspec schema fork`).
2. **Artifacts**: the schema's artifacts in order, each with the file it generates, its prerequisites (cycles flagged), whether it gates dispatch; edit its template and instruction side by side with a rendered preview; changes are validated before saving.
3. **Project context and rules**: edit `config.yaml` `context` and per-artifact `rules`.
4. **Plan preview**: a dry run showing which sections a new plan must have and what a worker's brief would contain under this schema.
5. **Specs status**: capabilities and requirement counts; plans with unapplied deltas; format check results; "Run openspec validate" when the CLI is installed.

All edits write OpenSpec's own files, so hand edits and the OpenSpec CLI keep working.

## 9. Backlog link ("Plan this")

The backlog stays the inbox before propose. A **Plan this** button on a backlog item (the backlog module already reserves it) hands the item to the lead: the lead writes the plan (with schema sections and spec deltas), and when `plan_create` succeeds the item is marked done with the plan id. This is Mnemo's `/opsx:propose`.

## 10. Rollout

- Start with **one capability**: `dispatch-routing` (its rules changed on 2026-11-04 and live in four superseded records), then `tier-ladder` from plan 87932c46.
- Grow specs as plans touch other areas; no big backfill.
- A one-off helper turns a capability's current `SPEC:` records into a draft `spec.md` for the lead to edit.

## 11. Steps (proposed)

1. **Spec files and parser** (kernel): read/write OpenSpec `spec.md` (requirements, scenarios), delta apply with OpenSpec's archive semantics, validation; move `.coding/specs/` to `.coding/design/`; index spec files for memory_search and the graph. — difficult
2. **Plans carry deltas** (plan board): `spec_deltas` in plan_create/plan_update, validation against current specs, rendering, worker context, deltas as acceptance scenarios. — difficult
3. **Archive on close**: merge deltas, archive folder per schema, close gate, commit allowance for `openspec/`. — difficult
4. **Schema support** (plan board): load and validate schemas and `config.yaml`, resolution order, sections from artifacts, template/instruction/rules in refusals and briefs, `apply.requires` dispatch gate, plan records its schema. — difficult
5. **Settings > Specs page** + backend commands (schema list/add/fork/validate, artifact and template editing, context/rules, preview, status, optional `openspec validate`). — average
6. **Backlog "Plan this"**. — average
7. **Plugin text**: mnemo-plan / mnemo-lead skills explain deltas, schemas and the archive. — simple
8. **First specs (lead)**: `dispatch-routing` and `tier-ladder` drafted from their records and reviewed; live close of one plan with deltas; `openspec validate` on the result if available.

Order: 1 → (2 ∥ 4) → 3 → (5 ∥ 6 ∥ 7) → 8.

## 12. Open questions (answer when this is planned)

1. Folder: `openspec/` at the repo root (OpenSpec-compatible, visible) or under `.coding/openspec/` (hidden, needs a config flag for OpenSpec tools)?
2. Should a plan be **required** to carry spec deltas once a capability has a spec (refuse plans touching files of a specced capability without deltas), or only encouraged?
3. Which schema first besides `spec-driven`: one you already use, or a community one?

# Mnemo — configuration reference

How to configure Mnemo2: the global files in `~/.mnemo/` (or `$MNEMO_HOME`), endpoints, models and tiers, MCP servers, and the per-project state. The app is described in [mnemo2/README.md](../mnemo2/README.md), the engine features in [FEATURES.md](./FEATURES.md), the quickstart in the [README](../README.md).

## Files at a glance

| File | What it holds |
| --- | --- |
| `config.toml` | shared settings: safety mode, `[memory]`, `[context]`, `[general.optimizer]`, `[general.laya]`, `[shell_filter]`, `[claude_lead]` |
| `endpoints.toml` | endpoints, their models and pricing, and the five tiers |
| `keys.toml` | API keys and OAuth tokens, user-only permissions |
| `mcp.toml` | MCP servers |
| `projects.toml` | registered projects |
| `mnemo2.toml` | Mnemo2-only settings: `[routing]` and `[window]` |
| `state-owner-id` | this machine's id, which stamps each project's `.coding/mnemo2/` |
| `bin/mnemo2-mcp` | the Claude Code plugin's helper, installed by the build |
| `run/` | one `mnemo2-<pid>.json` per running Mnemo2 (port, token, open project), read by the helper |
| `index-staleness.jsonl` | the index-repair log (last section) |
| `laya/training/` | Laya training logs |

The system prompts (preamble, workflow lifecycle, app rules, tool strategy, memory records, per-state workflow guidance) and the tools-array descriptions are compiled constants — not configurable per install. Installs predating the rename are migrated from `~/.myharness` automatically on first launch.

## config.toml

### Memory and context

`config.toml` holds the defaults, safety mode, the `[memory]` retrieval knobs — recency-decay half-life, per-query cap, derived per-class cap, digest budgets — and the `[context]` compaction knobs.

`summarize_at_fill_rate` (fraction of window triggering summarization) plus `preflight_compact` / `compact_headroom_tokens` (defaults on / 32 000) enabling aggressive keep_recent=3 compaction once a session exceeds its remaining headroom before each provider call).

The automatic compaction-limit fallback (no config key of its own): a compaction pass that still leaves the conversation at/over the effective threshold temporarily RAISES the effective limit, +10% per failed pass and cumulative (x1.1, x1.2, ..., at most 10 steps), never at or above the true window (`max_tokens - max(compact_headroom_tokens, 1)` - the one-token floor holds even for a degenerate `compact_headroom_tokens = 0`); every raise is announced in the transcript and logged, and the limit resets to the configured dial once a compaction fits at the un-raised threshold (also on `/new`, or when the model is swapped), so a boundary session continues instead of re-compacting until the bounded abort while the provider's hard window stays hard (a genuine provider length error is still an error).

### Context-economy levers

The **context-economy levers** are ON by default (2027-01-25 — saving tokens is the expected behaviour; uncheck one in Settings → Savings to opt out) — `[general.optimizer]` in `config.toml`, every flag `true` by default and omitted while default: `delta_reads` (a whole read of a source file over 500 lines serves its signature outline, so the agent reads the region it needs with `start_line`/`max_lines` instead of up to 500 lines it mostly won't use; smaller files are served whole), `compress_output` (collapse known command families — `cargo`, `npm`/`yarn`/`pnpm`, `pytest`, `go` — to their signal lines, dedup repeats, and redact credentials on every line the compactor serves — output it never sees passes through verbatim), `archive` (archive tool results at/above `archive_min_chars`, default 20 000 chars, keep a preview in context, and retrieve the full text later with the always-advertised `expand_result` tool by id or keyword), `compaction_survival` (archive the region a summary is about to drop as a checkpoint, hand the summarizer the decisions seen so far as a must-preserve block, and leave a post-compaction digest note naming the checkpoint), `quality_score` (grade the context S–F from how full the window is, shown in the ctx hover popup) and `lean_output_nudge` (past `lean_output_fill_pct` fill — default 25 — ride one lean-output steering line on the volatile tail so the cached prefix is never touched) and `recall_delta` (a repeat auto-recall of a memory already injected in this session renders a compact reference — tier, title, stable id, score, `unchanged since injection (turn N)` — instead of re-sending its 160-char snippet, and an ALL-elided set — every row already injected, the set unchanged since its last sighting — collapses into ONE sentinel line, `N recalled memories unchanged since injection (first at turn M); re-read with memory_search if needed.`, instead of N references; the reference names the memory for `memory_search`, because the volatile tail is popped right after every request, and a new, changed, or post-compaction recall still arrives in full).

Further knobs: `compress_min_chars` (default 2 000), `nudge_cooldown_requests` (default 10, shared by both nudges) and `compress_extra_commands` (extra command prefixes to compress). Every lever writes a row to the `savings_events` ledger so its cost/benefit is auditable; the Savings card on the Cost tab reads that ledger back per project. With a flag off its code path is byte-identical to the pre-lever behaviour. **Settings → Savings** edits all seven flags and the four knobs from inside the app — the same keys, saved to `config.toml` (the section stays omitted while every value is at its default), with a change taking effect on the next tool call and no restart.

### Laya

The **Laya classifier** is opt-in — `[general.laya]` in `config.toml`, edited in Settings → Tiers (the Laya settings at the bottom of the page). Laya is managed-only: Mnemo downloads a self-contained runtime (uv + virtualenv + `laya[serve]` + the English checkpoint, ~0.8–1 GB) and starts, monitors, and stops the local `laya-serve` sidecar on `127.0.0.1` itself — there is no endpoint and no checkpoint to choose. Disabled by default — the section is omitted from `config.toml` while untouched, and an absent or disabled section builds no classifier client, makes no call, and adds no startup cost; enabled without the runtime installed logs a hint and stays disabled (the Settings → Tiers download completes it, then `laya_setup` starts the server and swaps the client in). Settings → Tiers edits `enabled` plus the consumer opt-ins — `auto_type_memories` (memory prefix correction), `steer_tool_choice` (a confident classifier picks the search/search_read delegation class), `failure_triage` (classify failures to steer retries), `failure_triage_knn` (the kNN overlay that learns from the failure-triage training log via the local memory embedder — needs no `laya-serve`), all default `false` and omitted while default — and shows the live status (`disabled` / `installing` / `downloading` / `starting` / `ready` / `failed`). No auth is sent yet: the service's optional `LAYA_API_KEY` bearer is not part of this surface.

### Per-turn routing (kernel only)

`[general.laya] routing` and `[general.routing]` drive the kernel's per-turn routing for applications that embed the engine. Mnemo2 does not use them: its lead and worker sessions skip per-turn routing and the per-step reflex lane, and Mnemo2 routes each worker once, at dispatch (see `mnemo2.toml` below).

### [claude_lead]

`agents_without_prompt` lists the Claude Code agent types that run without asking while Claude Code leads a Mnemo2 project (default `["Explore", "claude-code-guide"]`). See [the plugin README](../mnemo2/plugin/README.md).

### Retired keys

Keys the retired Mnemo1 app wrote are ignored on load: `[general] auto_compact_on_plan_complete`, `[general.laya] auto_finetune`, the `[ui]` chat display toggles, `[ui.steering_notes]` and `[ui].show_delegation_notes`.

## endpoints.toml

### Endpoints and models

`endpoints.toml` holds the endpoints, their models and pricing — the Settings → Savings model field is an editable combobox listing the models the configured endpoints serve, annotated with their endpoint names, plus free typing for anything else. Each endpoint declares a kind — `openai`, `local`, or `anthropic` for the native Messages API, which also accepts an optional `workspace_id` sent as the `anthropic-workspace-id` header on every request so usage is attributed to a specific workspace.

Models inside an endpoint are plain ids (`models = ["gpt-4o"]`) or per-model tables (`[[endpoint.models]]`) with their own `max_context`, `max_output_tokens`, `reasoning_effort` default, and `reasoning_efforts` allow-list — a single endpoint can host models with different token budgets and reasoning surfaces. The per-model `reasoning_effort` (settable on each model row in Settings → Endpoints) wins over the endpoint's value when set; unset inherits the endpoint's (then the app default `max`).

`[models.planning|executing|bug_fixing|reviewing|complete|subagent|skill]` are per-task model slots for applications that embed the engine's plan-first workflow. Mnemo2 does not read them: its lead uses the model chosen in the composer, and each worker uses its tier's model.

The `summarize` slot is a dedicated consumer outside that chain: it routes compaction summaries (auto-compaction) to its model; unset (or dangling) rides the turn's model. Caveat: the summarize model's own context window is never consulted — the whole to-summarize region is sent to it, so pick a cheap model whose window covers your largest contexts (a too-small window fails the compaction; the turn continues — compaction is never a blocker).

Base URLs are forgiving: a missing trailing `/` is auto-appended on save.

Models whose tokenizer emits its stop boundaries as raw text (GLM-5.3's role tags + newline cascades) are fixed by config, not code: `stop_boundary_strings` on the endpoint or a model entry (model-level wins) lists the boundary strings — they ride the request `stop` list ahead of your own `stop` sequences and also arm the client-side stream guard that cuts a runaway stream at the first boundary; the same strings are escaped to a visible `⟦raw:\uXXXX…⟧` marker in tool-result messages (serving layers may strip them from request input too — the model then can't see them in tool results) and restored to the raw token by the file tools on write, so token-containing files round-trip read → write unchanged; pair with `stop_token_ids` for servers that honor token ids (the GLM-5.3 example: the four role-tag boundaries plus the `\n\n\n\n`/`\n\n\n` cascades, and ids `[151329, 151330, 151336]`).

The effort dropdown's `off` choice has the same escape hatch: `reasoning_effort_off_wire` on the endpoint or a model entry (model-level wins) sets the wire value `off` encodes as — unset, the built-in default applies (DeepSeek-family model names → `none`, the enum's explicit thinking-off variant; everyone else omits the field), so a renamed or fine-tuned DeepSeek model that needs the mapping the name-based default can't see is fixed by config. Aliases and fine-tunes are covered — resolution is by config, never by model-name prefix.

Strict tool schemas are capability-gated per endpoint: `supports_strict_schema` on an endpoint entry overrides the kind default (`openai` kinds enforce, `local`/`anthropic` don't) — set it `false` for an OpenAI-kind endpoint behind a litellm/vertex-class proxy that rejects the field, or `true` for a local endpoint that enforces it. When the effective value is true, the mutation/plan tools (`file_edit`, `file_write`, `file_append`, `convert_line_endings`, `create_plan`, `update_plan`, `complete_step`, `abandon_plan`, `finish`) are advertised with `"strict": true` and their schemas normalized to strict-legal form (every property required, optional ones nullable, `additionalProperties: false`) — the tools' forgiving argument forms keep working unchanged, and read-only tools stay unnormalized.

### Tiers

`endpoints.toml` holds five `[[tier]]` entries in fixed order: `trivial`, `routine`, `medium`, `high`, `escalate`. Each has an id, a name, a definition and an ordered list of models. A worker runs on its tier's first available model; an empty tier falls back to the nearest cheaper tier that has one; `escalate` is only for moving a worker that keeps failing, never a first pick. Edit them in Settings → Tiers.

## mnemo2.toml

Mnemo2's own settings, kept apart from the shared `config.toml`.

```toml
[routing]
mode = "off" # off | shadow | suggest | auto (Settings → Tiers)
cost_notice_usd = 2.0 # tell the lead when one worker passes this cost, then at each doubling; 0 = off
fallback_tier = "routine" # the tier auto mode uses when Laya is not confident enough
nudges = true # send escalation nudges to the lead

[window]
close = "auto" # auto | tray | quit (Settings → Appearance)
```

The `mode` values:

- `off` ("No: the lead picks") — the lead's tier stands and nothing is classified.
- `shadow` ("Watch only") — Laya classifies each dispatch and the answer is logged to `dispatch_routing.jsonl`, but the lead's tier stands.
- `suggest` — the lead's tier stands, and Laya's answer and escalation nudges reach the lead as advice.
- `auto` — a dispatch without a tier runs on Laya's tier when the answer clears the confidence gate, otherwise on `fallback_tier`, and a worker that keeps failing is moved up the ladder automatically.

`[window] close`: `auto` hides to the tray when Claude Code started Mnemo2 and quits otherwise; `tray` always hides; `quit` always quits.

## MCP servers

MCP servers live in `mcp.toml` — `[[server]]` entries with a name and exactly one transport: `command` + `args` (stdio, e.g. `npx -y @modelcontextprotocol/server-filesystem`) or `url` (+ `headers_env`) for remote servers. Secrets never enter the file: `env` and `headers_env` carry environment variable NAMES whose values are resolved from the environment at connect time. Each enabled server exposes its tools as a deferred `mcp.<name>` group (one index-line until the agent calls `load_tools`), and every MCP tool call is approval-gated. Settings → MCP edits the set and has a Test button (connect + list tools); saves apply to agents built afterwards — a restart always picks them up. Two per-server knobs: `trusted = true` skips the per-call approval prompt for that server's tools (auto-approve for servers you trust), and `idle_timeout_secs` drops a cached connection that has been idle that long, re-establishing it lazily on the next call (omit it to keep connections until they fail).

MCP config is two-scope: the installation-wide `~/.mnemo/mcp.toml` is union-merged with an optional per-project `.coding/mcp.toml` — the project file wins by name (same philosophy as skills: global baseline, project overrides). The Settings → MCP list tags each server with its source, and a save splits entries back into their files. Remote servers can authenticate with OAuth 2.1 (`auth = "oauth"` + a pre-registered `client_id`; optional `scopes`): Connect opens the browser login (PKCE S256, loopback redirect), tokens live in `keys.toml`, and expired access tokens refresh automatically on 401. Servers that advertise prompts and resources also expose `mcp__<server>__get_prompt` / `mcp__<server>__read_resource` agent tools (capability-gated, registered at reveal); server-initiated `sampling/createMessage` requests are declined with a standard JSON-RPC error so servers never hang.

## Per-project state

Per-project state lives in `<project>/.coding/` — knowledge files, plans, review reports, the backlog (`.coding/backlog.jsonl`, git-union-merged across worktrees), the memory + knowledge-graph databases (both git-ignored, rebuildable caches), and the skills (`.coding/skills/`, seeded at init from the app's embedded baseline — write-if-missing, so project-specific edits are preserved and deleted files self-heal on the next init). Creating a project also MERGES the app-owned `.gitignore`/`.gitattributes` entries — the cache/scratch ignores, `.worktrees/`, and the backlog's `merge=union` attribute — into whatever the project already has: only lines not already covered are appended, existing user lines are never reordered or overwritten, and a re-run is a byte-identical no-op.

Mnemo2 keeps its own per-project state in `.coding/mnemo2/`: `lead.json` (the lead's model, effort and transcript), `team.json` (the worker ledger and the live-worker cap), `preferences.json` and `transcripts/`. The folder carries its own `.gitignore` (`*`) and is trusted only when its `owner` stamp matches this machine's `state-owner-id`.

## Index-staleness log

The **index-staleness log** (`~/.mnemo/index-staleness.jsonl`) records every stale-index repair: one JSON line per stale file, written by `search` / `search_read` (the FTS content index) and `graph_search` (the symbol index). Only staleness writes records — a clean search leaves the file untouched, and the file does not exist until the first record. Each line carries `ts` (unix millis), `tool`, `action` (`inline-reindex` when the file was repaired in the same call, `surfaced-only` when the repair was refused — staleness above the inline-repair ceiling), `path` (project-relative), the evidence `mtime_index` / `mtime_disk` / `size_disk` (omitted when unknown), and `cause`: `content-edit` (the bytes changed), `touch` (mtime moved, hash identical — a save-all, a checkout, a formatter), `line-ending-flip` (CRLF↔LF), `unindexed` (no index row yet), `vanished-or-unreadable`, or `mtime-drift` (no hash evidence — above the inline-repair ceiling only; the classification hash is the indexer's and is skipped above the ceiling, so the log never out-costs the repair it explains). The log is append-only, capped at 512 KB, and rotates to `index-staleness.jsonl.1` once past the cap (one backup kept). When records were written the tool note names the first three paths and appends `; full list: index-staleness.jsonl` — e.g. `jq -r 'select(.cause=="touch") | .path' ~/.mnemo/index-staleness.jsonl` lists every file that was only touched, and the same read answers "when" (`ts`) and "was it repaired" (`action`).

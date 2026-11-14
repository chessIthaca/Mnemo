# Mnemo

> **An agentic coding harness that remembers.**

Named after Mnemosyne, the Greek titaness of memory — because the point is that your agent gets *better* at your codebase the longer it works on it, instead of starting from zero every session.

![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-blue)
![Rust](https://img.shields.io/badge/rust-stable-orange)
![Node](https://img.shields.io/badge/node-LTS-green)

Mnemo is a **Rust + Tauri desktop app** for coding with a team of LLM agents. A **lead** plans the work on a plan board and hands each step to a **worker**. Every worker runs on a model tier you choose, in its own git branch and checkout. The lead reviews each worker's diff, records evidence and checks, and merges it into its working branch. Memory, a tree-sitter code graph and approval gates come with every session.

The app is called **Mnemo2**. Mnemo1, the original single-agent app, retired in 2.3.0 (see the [CHANGELOG](CHANGELOG.md)).

You can lead from Mnemo2's own lead chat, or let **Claude Code** lead through the [`mnemo` plugin](mnemo2/plugin/README.md).

---

## Why Mnemo exists

If you've iterated with coding agents, you know the pattern. The pain points Mnemo was built to eliminate:

- **Plans evaporate.** Context resets, sessions restart, and the agent re-derives what it already knew — or worse, re-decides it differently.
- **The tools array bloats.** Every capability is advertised all the time, so the model calls the wrong one, at the wrong moment, or not at all.
- **Big models become a crutch.** You pay flagship prices for the privilege of the agent losing the plot halfway through.
- **Memory doesn't survive.** Everything your agent learned about your project last week is gone today.
- **The harness trusts the model.** Mutations go out unchecked; one confident wrong turn costs an hour of cleanup.

**Not a model problem — an allocation problem.** The same model is brilliant when the plan is right and hopeless when it's reconstructing intent from a stale context. So Mnemo allocates: the lead plans and reviews, cheaper workers implement on the tier each step needs, and every handoff (plan, review, memory record) is a file on disk.

The loop, enforced end to end: **plan → dispatch → checks → review → merge → pull request**, with every artifact persisted — not remembered.

---

## Highlights

| | |
|---|---|
| **Lead and team** | The lead plans on a plan board and dispatches each step to a worker in its own `wt/agent-*` branch and checkout; several workers run at once (Team tab, worker slots). |
| **Tiers** | Five model tiers (trivial, routine, medium, high, escalate) set in Settings → Tiers. Optional Laya routing can watch, suggest or pick each worker's tier. |
| **Review gate** | No merge without the lead's review evidence, checks recorded on the exact commit and every finding closed; high-risk work also needs an independent read-only audit. Merges are local `--no-ff`; nothing pushes to `main`. |
| **Four-tier memory** | Working → episodic → semantic → procedural, persisted in SQLite with recall ranked by relevance, recency and staleness (records not re-accessed within the decay half-life sink by up to −0.25); sessions auto-consolidate into durable knowledge at turn boundaries, not only via the manual tool. |
| **Knowledge graph** | Tree-sitter parses 12 languages (Rust, TS/TSX, JS, Python, Go, Java, C/C++, C#, Ruby, PHP, HTML) into a per-project SQLite graph: callers, callees, imports, blast radius, shortest paths. |
| **Safety first** | Approval gates on mutations, git merge and push always gated, a path sandbox, and `.coding/` state protected from the file tools. |
| **Token-optimizer levers** | Seven context-economy levers, on by default (Settings → Savings): outline-first reads, command-output compression with credential redaction, archive and expand, compaction survival, a context-quality score, lean-output nudges and recall delta. The Savings card on the Cost tab shows what they saved. |
| **Desktop shell** | Tauri 2 app for Windows and macOS: lead chat, Team (mission control), Cost, Graph and Plans tabs, a side panel (Browser, Diff, Plan, Files, Git, Trace, Graph, Memory) and MCP servers. The Browser pane is WebView2 on Windows. |

The [feature reference](docs/FEATURES.md) has the exhaustive detail; this is the tour.

## Embedding the kernel

The engine under the app is a library first. Cargo features slice it:
`kernel` (the core — always compiled), `headless` (the public engine
surface: `Engine`, `AgentSession`, `Setup`, the `*Api`s; implies
`kernel`), `app` (the plan-first workflow module graph), plus the
optional `embeddings` / `browser` heavies. Defaults give library
consumers the full set; a second consumer builds the lean surface:

```sh
cargo build -p mnemo --no-default-features --features kernel,headless
```

`examples/second_consumer.rs` is the ~280-line proof — a NoPolicy
agent with a non-coding identity, tools narrowed to `[Read, Host]`,
one host tool the host answers itself, turn limits, every session
event printed as JSON. It runs two ways: `MNEMO_HOME` set resolves a
configured endpoint like a real deployment; unset, an in-file scripted
provider (the public `Engine::builder(..).provider(..)` override)
drives the whole host-tool round trip offline — the shape CI's
`examples` job runs and asserts. The app crate is held to the same public
surface: `tests/boundary.rs` fails the build if `src-tauri` reaches
into `mnemo::agent::` / `mnemo::runtime::` / `mnemo::host::`
internals.

---

## How a task flows

1. **Plan.** The lead searches project memory, then writes a plan on the board: goal, context, acceptance scenarios and a detailed spec for each step.
2. **Dispatch.** Each step goes to a worker on the cheapest tier that fits (Laya can choose in Auto mode). The worker receives the plan and its step, works in its own branch and checkout, runs the checks and commits.
3. **Review.** The lead reads the diff before the worker's summary, runs the checks on the worker's exact commit and records a review with evidence: intent, execution, integration, test quality and residual risk. Every finding is fixed or closed with a written reason; high-risk work also needs an independent read-only audit.
4. **Merge.** The reviewed worker merges `--no-ff` into the lead branch, and Mnemo2 removes its branch and checkout. The plan closes with a review report in `.coding/reviews/`.
5. **Land.** The lead opens a pull request into `main`; CI must pass before it merges.

Workers cannot plan, merge, push or ask the user, and the lead cannot merge work that has not passed review. Those rails are what make cheaper worker models viable. The full policy is in [docs/REVIEW-POLICY.md](docs/REVIEW-POLICY.md).

---

## How the repository works

```
src/                  the `mnemo` library — the engine (no Tauri)
  agent/                turn loop, approval gates, context construction
  app/                  the plan-first workflow, the plan board, backlog
  headless/             the public engine surface: Engine, AgentSession, Setup
  host/  runtime/       session hosting and the multi-agent runtime
  memory/               four-tier store: consolidation, embedder, knowledge files, strength decay
  codegraph/            tree-sitter extraction → SQLite graph: query, walk, watcher
  tool/                 tool implementations: files, shell, git, memory, browser, agent
  safety_rules/         the constitution the agent cannot ignore
  provider/             model endpoints: OpenAI/Anthropic-compatible + local
  model_resolver.rs     model and effort resolution
  mcp/                  Model Context Protocol client (stdio + remote, OAuth)
mnemo2/               the Mnemo2 UI (Svelte 5, Vite)
  plugin/               the `mnemo` Claude Code plugin (Claude Code as the lead)
src-tauri/            the Mnemo2 host library (`mnemo-app`)
  src/mnemo2/           lead, team, plan board, review and merge gate, MCP server
  mnemo2-app/           the app package: the `mnemo2` and `mnemo2-mcp` binaries
  tests/                source-contract tests guarding the vendored wry patches
tests/integration/    cross-boundary integration tests
examples/             second_consumer.rs, the lean-kernel proof
scripts/              maintenance scripts
vendor/               patched wry — with a PATCHES.md, wired via [patch.crates-io]
.github/workflows/    ci.yml (every pull request), build.yml (release installers), codeql.yml
.claude-plugin/       the plugin marketplace entry for Claude Code
rust-toolchain.toml   the pinned Rust toolchain
agent.md              the project constitution — read by the lead and every worker
PLAN.md               the technical-decisions log
.coding/              the agents' own state, versioned with the repo (see below)
```

**The lib/app split.** `mnemo` is a plain Rust library: memory, graph, tools and the engine all work headless. `mnemo-app` (`src-tauri/`) hosts the Tauri 2 Mnemo2 app, which the `mnemo2-app` package builds; it always selects the heavier optional features (embedded browser, embeddings).

**`.coding/` is the memory the repo carries.** Plans, review reports, knowledge files, and the backlog travel in git and merge across machines; the SQLite caches (`memory.db`, `codegraph.db`) are rebuildable and gitignored. A fresh clone of Mnemo's own repo arrives with its development history already loaded. Creating a project seeds those ignore rules (and the backlog's union-merge attribute) into the new project's `.gitignore`/`.gitattributes`, appending only what is missing — existing user lines are never touched. Mnemo2's own per-project state (the lead transcript, the worker ledger, preferences) lives in `.coding/mnemo2/`, which ignores itself and never enters git.

**Vendored patches.** `vendor/` carries small, documented patches to `wry` (OS-account SSO + hard-reload for the embedded browser), guarded by source-contract tests in `src-tauri/tests/`; the lock pin itself is guarded by `cargo_lock_keeps_the_vendored_patches` (`tests/integration/ci_workflow.rs`), which fails if a `[[patch.unused]]` entry appears or `wry` stops resolving from the vendored path. (`tao` used to be vendored for its Windows IME self-deadlock fix; tauri 2.12 moved the app to tao 0.37.x, which carries that upstream fix natively — the backport and its guard were dropped with it.)

---

## Quickstart

### Install a release

Download the installer for your platform from the [releases page](https://github.com/chessIthaca/Mnemo/releases): the `.msi` or `-setup.exe` for Windows, the `.dmg` for macOS on Apple silicon.

### First run

1. **Settings → Endpoints**: add an OpenAI-compatible, Anthropic-compatible or local endpoint (Ollama, LM Studio) and its models. API keys are stored with user-only permissions.
2. **Settings → Tiers**: put a model on each tier you want to use. An empty tier falls back to the nearest cheaper tier that has a model.
3. **Settings → Models**: download the local embedding model, so memory search is semantic.
4. Open a project folder from the start screen, choose the lead's model in the composer, and describe what you want.

### Build from source

**Prerequisites — Windows 10/11:**

- [Rust](https://rustup.rs) — rustup installs the version pinned in rust-toolchain.toml on the first build
- [Node.js LTS](https://nodejs.org)
- Visual Studio Build Tools with the **Desktop development with C++** workload
- WebView2 Runtime (preinstalled on Windows 10/11)

**Prerequisites — macOS 11+:**

- Xcode command-line tools: `xcode-select --install`
- [Rust](https://rustup.rs) — rustup installs the version pinned in rust-toolchain.toml on the first build
- [Node.js LTS](https://nodejs.org)

```bash
git clone https://github.com/chessIthaca/Mnemo.git
cd Mnemo
npm install
npm run rebuild:mnemo2   # release build (asks running instances to leave first)
```

Development:

```bash
npm run dev:mnemo2            # Vite dev server (browser-only preview)
npm run dev:mnemo2:native    # Tauri host + real lead and team (run dev:mnemo2 first)
```

To lead from Claude Code instead of Mnemo2's lead chat, install the plugin as described in [mnemo2/plugin/README.md](mnemo2/plugin/README.md).

---

## Development & tests

Run what CI runs (`.github/workflows/ci.yml`) before you open a pull request:

```bash
npm ci
npm run build:mnemo2
npm test --workspace mnemo2
cargo clippy --workspace --all-targets
cargo clippy -p mnemo-app --all-targets --features mnemo2
cargo test --workspace --no-fail-fast
cargo test -p mnemo-app --features mnemo2 --lib
```

Other scripts:

```bash
npm run dev:mnemo2          # http://127.0.0.1:5180 — browser preview, no engine behind it
npm run dev:mnemo2:native   # Tauri host + real lead and team (run dev:mnemo2 first)
npm run check:mnemo2        # svelte-check
npm run build:mnemo2:native
```

Two heavyweight optional cargo features exist on the library — `browser` (headless-browser tooling) and `embeddings` (local ONNX memory search) — plus a dev-only `test-support` feature for cross-crate test factories. The two heavyweight ones are off in a plain `cargo build -p mnemo`; the app always selects both, so `cargo test --workspace` compiles them.

## Settings worth checking

- Download the local embedding model in **Settings → Models**, then rebuild the memory index (Memory tab → Diagnostics → Memory upkeep → **Rebuild index**) so recall is semantic.
- For Laya tier routing, open **Settings → Tiers** and choose whether Laya watches, suggests or picks each worker's tier. Laya is off by default; turning it on downloads a self-contained runtime (roughly 0.8–1 GB plus the checkpoint), and Mnemo starts and stops the local `laya-serve` on 127.0.0.1 itself.
- In **Settings → Appearance**, choose what closing the window does: quit, hide to the tray, or automatic.

### Context economy — the `[general.optimizer]` levers

Seven independent context-economy levers, all **on by default** (2027-01-25 —
saving tokens is the expected behaviour, not an opt-in); a flag turned off is
byte-identical to pre-lever behaviour. Uncheck a lever in **Settings → Savings**
(each toggle takes effect on the next tool call — no restart; the Savings card on the Cost tab shows what they save, per project). A `config.toml` that never wrote the
`[general.optimizer]` section runs every lever — the section appears only once
you opt out of one (or move a knob), and an explicit `false` there is honoured
as written:

```toml
[general.optimizer]
delta_reads = false          # opt out: big source files are served whole instead of as an outline
compress_output = false      # opt out: command output is served verbatim
archive = false              # opt out: large tool results are not archived
compaction_survival = false  # opt out: no checkpoint or preserved decisions
quality_score = false        # opt out: no S–F grade in the ctx hover popup
lean_output_nudge = false    # opt out: no lean-output steering line
recall_delta = false         # opt out: repeat recalls re-send the full snippet

# knobs (defaults shown)
archive_min_chars = 20000    # only results at least this large are archived
compress_min_chars = 2000    # only outputs at least this long are compressed
lean_output_fill_pct = 25    # fill % at which the lean-output nudge starts
nudge_cooldown_requests = 10 # requests between two nudges
compress_extra_commands = [] # extra command prefixes to compress
```

Every lever writes a row to the `savings_events` ledger, so its cost/benefit is
auditable. The always-advertised `expand_result` tool retrieves any archived
result by id or keyword. The **Savings** card on the **Cost** tab reads the ledger back per project: metered tokens saved, per-kind and
per-day breakdowns, the recent events, and a prompt-cache section whose dollar
figures are labelled ESTIMATED.

Have fun and let me know where we can improve things.
Pull requests gratefully considered.

---

## Where the detail lives

- [mnemo2/README.md](mnemo2/README.md) — the Mnemo2 app: lead, team, workbench, review and merge
- [mnemo2/plugin/README.md](mnemo2/plugin/README.md) — Claude Code as the lead
- [docs/REVIEW-POLICY.md](docs/REVIEW-POLICY.md) — the review and merge gate
- [docs/FEATURES.md](docs/FEATURES.md) — the engine feature reference (memory, graph, tools, providers)
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md) — config files, endpoints, tiers, MCP servers, per-project state
- [CHANGELOG.md](CHANGELOG.md) — releases
- [PLAN.md](PLAN.md) — the technical-decisions log
- [agent.md](agent.md) — the project constitution the lead and workers load
- [vendor/wry/PATCHES.md](vendor/wry/PATCHES.md) — what is patched upstream and why
- `.coding/` — the agents' own working state (plans, reviews, knowledge, backlog)

---

## License

MIT — see [LICENSE](LICENSE). Copyright © 2026 Carsten Hess.

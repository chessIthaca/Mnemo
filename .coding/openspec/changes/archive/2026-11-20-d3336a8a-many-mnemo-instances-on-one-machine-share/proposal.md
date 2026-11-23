Several Mnemo2 instances run side by side without duplicating models, overloading the machine or tripping provider limits. The user sees them all in one place and settings reach every instance. Memory search is fully semantic, and code can be found by meaning.

WHY: the user runs many Mnemo2 instances at once (one per project, e.g. C:\Mnemo and C:\FactoryGame). Today each is an isolated process.
- Duplicated models: each instance runs its own Laya server (Python `laya-serve`, ~1 GB+ RAM, src/host/laya.rs LayaManager) and its own fastembed embedder (bge-base-en-v1.5, ~0.5 GB, src/memory/embedder.rs BundledEmbedder).
- Unshared limits: each runs heavy cargo/npm builds with no machine-wide limit (we saw load-flaky tests today), and hits provider rate limits independently.
- No shared view: nobody shows all instances, and a hand edit of ~/.mnemo/*.toml reaches no running instance.
- Search gaps: memory search is semantic only inside an FTS pre-filter (src/memory/mod.rs recall_peek :1679; FTS_CANDIDATE_POOL 50 :1691, FULL_SCAN_FALLBACK_CAP 200 :1699), so meaning-only matches are missed. Code search is FTS (cg_content) plus symbol-name lookup only (memory HOW a146c39d: graph_search is lexical).

DECISIONS (user, 2026-11-19: "I want them all"):
- SHARED SERVICES use one generic mechanism: a lease directory `~/.mnemo/run/<service>/`.
  - A `service.json` holds {pid, port, token, version, started_ms, models}, plus one `lease-<instance pid>.json` per attached instance.
  - The first instance starts the service detached (NOT inside its own kill-on-close job, or the service would die with the first instance) under a spawn lock (reuse the `SpawnLock` pattern in src-tauri/mnemo2-app/src/mnemo2_mcp/instance.rs:721).
  - Others attach after a health check. Leases of dead pids are stale.
  - The service exits when it has had no live lease for 60 s, so a crash of every instance never leaves it running forever.
  - Every attach falls back to the in-process path (today's behaviour) when the service is unreachable. A shared service is an optimisation, never a new single point of failure.
- Laya stays a Python server. The embedder host is a new small Rust binary `mnemo-models` (same workspace) serving loopback HTTP with a bearer token. It serves the memory model (bge-base-en-v1.5) and the code model (jina-embeddings-v2-base-code, which fastembed 7.7.1 already supports: EmbeddingModel::JinaEmbeddingsV2BaseCode, dim 768).
- MACHINE LIMIT for heavy commands: a user setting, `[general.machine] heavy_commands` (default 2, range 1-16), editable on a new Settings page "Machine" (All projects group).
  - Slots are OS file locks on `~/.mnemo/run/heavy/slot-<i>.lock` (std::fs::File::try_lock, stable since Rust 1.89; the toolchain is 1.99). A crash releases the lock.
  - Heavy = the session's check commands, plus commands whose first word is cargo with build/test/clippy/check/doc/bench, or npm/npx/pnpm/yarn with test/run build/run check/vitest/tsc.
  - Waiting for a slot never counts toward the silence timeout, and the wait is reported in the result.
- ENDPOINT LIMITS are machine-wide too.
  - endpoints.toml gains an optional per-endpoint `max_concurrent` (file-lock slots under `~/.mnemo/run/endpoint-<hash of name>/`), editable on the Endpoints page.
  - A 429 with Retry-After (or a default 30 s) writes `~/.mnemo/run/endpoint-<hash>/cooldown` (an until-ms timestamp), which every instance honours before sending.
- SHARED BUILD CACHE: when `sccache` is on PATH, worker commands get RUSTC_WRAPPER=sccache automatically. There is no toggle (user preference: automate over settings knobs). The Machine page shows whether sccache was found, with the install hint `cargo install sccache`.
- INSTANCES OVERVIEW comes from the run files (`~/.mnemo/run/mnemo2-<pid>.json`, src-tauri/src/mnemo2/run_file.rs list :148). Each instance already serves an authenticated HTTP endpoint (mcp_server.rs).
- LIVE SETTINGS: a notify watcher on ~/.mnemo/{config,endpoints,keys,mcp}.toml reloads through the same path as a Settings save (src/headless/setup.rs patch_settings :551 and rewire). An instance's own save does not trigger a second reload (compare content hashes).
- SHARED LEARNING: the routing and failure-triage kNN logs are already user-level (~/.mnemo/laya/training/*.jsonl). The new part is a machine knowledge tier `~/.mnemo/knowledge/` (how/ and decision/ records that are not project-specific), indexed into each project's memory store as a second, read-only source and labelled [machine] in results. memory_write gains `scope: "machine"`.
- SEMANTIC CODE SEARCH: the code graph stores each symbol's signature and doc comment. A vector table embeds one chunk per symbol, incrementally, by text hash, through the code model in `mnemo-models`. A new tool `find_code {query}` ranks by meaning blended with FTS and name matches. An eval on this repo decides whether it stays on.
- MEMORY SEARCH: recall unions the FTS candidates with the top 50 by cosine over ALL live rows, so meaning-only matches are found.
- SETTINGS TEXT: the Models page section "Code indexing" is renamed and rewritten to say what the embedding model is used for (memory search over specs, decisions, bugs and HOW records, plus plan/review/backlog summaries; routing and failure-triage examples; the memory hints in search). A second entry describes the code-search model. Both show the shared host's status.

CONSTRAINTS: Windows and Unix both (cfg! branches; the macOS CI leg runs tests). No #[allow]. Reuse process_tree.rs KillOnCloseJob / TreeKillGuard where a process must die with its owner. Every new file under ~/.mnemo/run is written atomically (temp file + rename). Tests use a temp MNEMO_HOME and never touch the real ~/.mnemo.

VERIFY (each alone on its line): `cargo test --workspace`; `cargo clippy --workspace --all-targets`; `npm test --workspace mnemo2`; `npm run check --workspace mnemo2`.

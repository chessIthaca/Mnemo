+++
title = "content-index staleness root causes — .coding indexed but never watched, plus the 800 ms debounce"
created = "2027-01-11"
+++

ROOT CAUSE of content-index staleness (diagnosed 2027-01-25, asked as "nobody else edits files, so why stale?"). TWO mechanisms, both from Mnemo's own writes:

(1) TRANSIENT, by design: the watcher debounce is 800 ms — src-tauri/src/main.rs:2066 spawns GraphWatcher::spawn(graph, project.root, Duration::from_millis(800)). src/codegraph/watcher.rs debounce_loop (:201-213) sleeps that quiet period after the last event of a burst, then runs a full index() pass on spawn_blocking; only then are the FTS rows fresh. Window = 800 ms + pass time. The design assumes human-paced editing; with agent tool calls (seconds apart) an edit->query inside the window is the NORMAL case, so the F10 stale trip is routine, not exceptional.

(2) PERMANENT, a genuine coverage bug: .coding/ is INDEXED but never WATCHED. Index side: walk_searchable/visit (src/codegraph/walk.rs:154-199) skips a directory only via is_ignored_component, and .coding is NOT in that list, so every .coding/knowledge/*.md, .coding/plans/*.md, .coding/reviews/*.md and .coding/backlog.jsonl gets FTS content rows + a cg_files mtime row (only the .db family is kept out of RESULTS by a separate name guard, src/tool/agent/search.rs:2031-2046). Watch side: is_indexable_path (watcher.rs:52-67) rejects ANY path with a .coding component (line 61) — blanket; its doc (:40-46) admits "Those files' content rows simply refresh on the next pass". So Mnemo's own memory/plan/review/backlog writes fire NO watcher event and stay stale until an unrelated non-.coding edit triggers a pass, or startup/manual rebuild. In an artifact-heavy session (plans, memories, reviews) those files are stale by construction.

Why NINE at once: the F10 sweep (src/tool/agent/search.rs:914-938, fts_page :865) checks EVERY glob-passing hit file on the fetched page — up to MAX_MATCHES*5 = 500 hits, not the displayed 100 — so one common token trips staleness across the whole matching set.

FIX DIRECTION (root cause, queued as the follow-up plan AFTER backlog 9201704f): narrow the blanket .coding watch exclusion to what it was built to avoid — the self-trigger loop comes from the index PASS writing .coding/codegraph.db (+ -wal/-shm) mid-pass, and those files are excluded from search results anyway. So watch .coding/** except the DB family, and optionally except the per-step bookkeeping (stack.json / instance.json / backlog.jsonl) whose churn would add a pass per bookkeeping write. USER DECISION 2027-01-25: finish the adaptive-budget plan (backlog 9201704f) first, then fix the watcher root cause as its own plan.

# System Prompt Review — Efficiency, Accuracy & Context Optimization

**Date:** 2026-11-23
**Scope:** Every prompt the model sees — stable head, volatile tail, tool schemas, one-shot (summarization/consolidation), skill prompts.
**Method:** Read `src/agent/prompt.rs`, `src/agent/context.rs`, `src/memory/consolidation.rs`, `agent.md`, `src/project/agent_md.rs`, `src/agent/turn.rs` (assembly site), all `src/tool/agent/*.rs` + `src/tool/workflow/*.rs` schemas, `.coding/skills/merge_to_main.toml`. No source changes — analysis only.

---

## 1. Inventory — what the model sees each turn

### Stable head (`messages[0]`, byte-stable across turns → cached)
| Block | Source | Approx chars | Notes |
|---|---|---|---|
| `CODING_SYSTEM_PREAMBLE` | prompt.rs:13-52 | ~1900 | Core principles + tool-usage tutorials |
| `WORKFLOW_LIFECYCLE` | prompt.rs:64-98 | ~1700 | State-machine map |
| Global constitution | `~/.myharness/agent.md` | (unreadable) | Cross-project hard rules |
| Project constitution | `agent.md` | ~5200 | 128 lines of hard rules + closing sequence |
| Session primer | `format_primer` | ~2100 | 8 memories × ~260 chars, once/session |
| **Stable head total** | | **~11k+ chars** | Paid on cache miss; cached after |

### Volatile tail (trailing system msg, changes per step/turn → re-processed each change)
| Block | Source | Approx chars | Notes |
|---|---|---|---|
| `# RECALLED MEMORIES` | `format_recall_context` | ~1000 | 5 memories × ~200 chars |
| `workflow_section` | prompt.rs:270-415 | ~1200 | Per-state text + CURRENT STEP + CAPABILITIES |
| `CONTEXT_FOOTER` | prompt.rs:113 | 60 | Fixed sentinel (cache-stable last msg) |

### Tool schemas (`tools` array, sorted alphabetically → cache-stable)
~30 tools. Each carries a top-level `description` (50-600 chars) + per-parameter `description`s. Total ~12-15k chars. Re-sent every turn but cache-stable (sorted, deterministic — `src/tool/mod.rs:364`).

### One-shot prompts (not per-turn)
- **Summarization** — `build_summary_prompt` (context.rs:329-392), two ~90% identical branches.
- **Consolidation** — 3 prompts (consolidation.rs:162, 207, 261): synthesize episodic, extract semantic, extract procedural.
- **Skill** — `merge_to_main.toml` prompt (injected only when skill active).

---

## 2. Redundancy findings (ranked by wasted tokens)

### R1 — Research-planning guidance appears in 4 places (HIGH)
"Plan even for research / `kind:research` skips review" is stated in:
- `CODING_SYSTEM_PREAMBLE` lines 23-26 (~280 chars)
- `WORKFLOW_LIFECYCLE` lines 70-73 + 90-97 (~600 chars)
- `agent.md` lines 113-120 (~450 chars)
- `create_plan` tool schema lines 93-97 (~400 chars)

**~1730 chars of repetition.** The preamble and lifecycle overlap most heavily — the preamble's bullets 3-4 are a condensed restatement of the lifecycle's PLANNING/EXECUTING/PLAN KIND sections.

### R2 — Sub-plan guidance appears in 4 places (HIGH)
"Create sub-plans AT ANY POINT during EXECUTING; parent auto-resumes" is in:
- `CODING_SYSTEM_PREAMBLE` lines 27-30 (~230 chars)
- `WORKFLOW_LIFECYCLE` lines 74-79 (~280 chars)
- `create_plan` tool schema lines 89-92 (~300 chars)
- `workflow_section` Executing arm lines 333-340 (~400 chars)

**~1210 chars of repetition.** The `workflow_section` copy is the most expensive because it lives in the **volatile tail** — re-processed on every `complete_step` bump.

### R3 — Review/closing sequence appears in 3 places (HIGH)
The test→review→commit closing sequence is in:
- `WORKFLOW_LIFECYCLE` lines 80-85 (~280 chars)
- `agent.md` "Standard plan closing sequence" lines 66-120 (~2200 chars — authoritative)
- `workflow_section` Reviewing arm lines 351-365 (~600 chars)

**~880 chars of repetition** beyond the authoritative `agent.md` version. The lifecycle + workflow_section copies are condensed restatements of the constitution.

### R4 — Tool-gating explanation duplicated (MEDIUM)
"Workflow gates tools, enforced not suggestion" is in:
- `CODING_SYSTEM_PREAMBLE` lines 19-22 (~240 chars)
- `WORKFLOW_LIFECYCLE` lines 65-66 (~100 chars)
- `workflow_section` Planning arm lines 280-283 (~200 chars)

### R5 — file_edit parameter tutorial duplicates the tool schema (MEDIUM)
The preamble (lines 33-42, ~600 chars) teaches `use_regex`/`count`/`start_line`+`end_line`/`fuzzy_whitespace`. The `file_edit` schema (file_edit.rs:497-505) carries the same parameter descriptions. The model sees both every turn. The preamble adds *strategy* (when to use each mode) but also re-states the *spec* (what each param does) the schema already carries.

### R6 — read_files / file_append guidance (LOW)
Preamble lines 43-50 teach `read_files` shape (~150 chars, matches schema) and `file_append` chunking (~250 chars, **not** in schema). The `read_files` part is redundant with the schema; the `file_append` part is preamble-only strategy.

---

## 3. Accuracy findings (prompt vs. tool behavior)

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| A1 | Preamble file_edit modes (regex/count/line-range/fuzzy) | ✅ accurate | file_edit.rs:497-505 matches |
| A2 | `read_files {path, start_line?, max_lines?}` | ✅ accurate | read_files.rs:104-110 matches |
| A3 | "Keep file_append calls under ~5000 chars" | ⚠️ preamble-only | file_append.rs:46-62 schema has no size limit — guidance lives only in preamble |
| A4 | `CONTEXT_FOOTER` "ignore" | ✅ accurate | Fixed 60-char sentinel (prompt.rs:113) |
| A5 | Reviewing arm: spawn `role:"reviewer"` read-only | ✅ accurate | spawn_agent.rs:134 confirms |
| A6 | "In PLANNING and COMPLETE only read tools + create_plan are visible" | ⚠️ imprecise | COMPLETE also exposes `skill_start`, `ask_user`, `current_plan`; PLANNING exposes `ask_user`, `current_plan`. The preamble oversimplifies — the per-state `workflow_section` is precise, so this is a minor conceptual-map simplification, not a bug. |

No inaccurate claims found — only two imprecisions (A3, A6), both low severity.

---

## 4. Efficiency recommendations (ranked by impact)

### E1 — Merge `CODING_SYSTEM_PREAMBLE` + `WORKFLOW_LIFECYCLE` (HIGH, ~1000 chars saved)
The two stable-head consts overlap ~40%. The preamble's bullets 3-4 (research planning, sub-plans) are a condensed restatement of the lifecycle's PLANNING/EXECUTING/PLAN KIND sections. **Consolidate into one block:** keep the lifecycle map (the conceptual arc) and trim the preamble to only the principles NOT in the lifecycle — read-before-write, be-precise, explain-briefly, file-edit strategy. Drop the duplicated research/sub-plan/tool-gating bullets from the preamble.
- **Savings:** ~800-1000 chars from the stable head.
- **Cache impact:** one-time cache miss on next turn, then re-cached. Safe.

### E2 — Trim the preamble's tool-usage tutorials (HIGH, ~700 chars saved)
The preamble teaches `file_edit` modes (~600 chars), `read_files` shape (~150 chars), `file_append` chunking (~250 chars). Since the tool schemas are **always present** in the `tools` array and are the authoritative spec, the preamble's parameter-level tutorials are largely redundant. Keep only the **strategy** ("prefer `file_edit` targeted edits over `file_write` full rewrites", "use `read_files` to cut round-trips", "chunk large writes with `file_append`") and drop the parameter semantics the schema already carries. Move the `file_append` ~5000-char guidance (A3) into the `file_append` tool schema so it survives the trim.
- **Savings:** ~700 chars from the stable head.
- **Cache impact:** one-time miss, then re-cached. Safe.

### E3 — De-duplicate the summarization prompt (MEDIUM, ~700 chars/call saved)
`build_summary_prompt` (context.rs:329-392) has two branches (running-update vs fresh) that share ~90% identical text: the 5-section format (Current Task / Key Decisions / Files & Identifiers / Errors & Blockers / Open Items) + the compression instructions (~700 chars duplicated). Extract the shared section-format + compression instructions into a `const` or helper, and branch only on the intro + payload placement.
- **Savings:** ~700 chars per summarization call.
- **Cache impact:** none (one-shot prompt, not cached).
- **Priority:** lower (one-shot), but it's a clean DRY fix.

### E4 — Trim `workflow_section` Executing arm (MEDIUM, ~400 chars/step saved)
The Executing arm (prompt.rs:329-347) has a ~400-char instruction block repeating sub-plan/update_plan/abandon_plan guidance already in the lifecycle + tool schemas. Since this is in the **volatile tail** (re-processed on every `complete_step` bump), trimming it saves tokens *per step*, not just per turn. Keep CURRENT STEP + PROGRESS + the "← resume here" pointer; drop the repeated sub-plan/update_plan lecture.
- **Savings:** ~400 chars per step bump.
- **Cache impact:** none (volatile tail changes anyway; `CONTEXT_FOOTER` keeps the prefix stable).

### E5 — Tighten consolidation prompts (LOW)
The three consolidation prompts (consolidation.rs:162, 207, 261) are one-shot and already minimal. The synthesize prompt could more forcefully specify "return ONLY JSON, no prose preamble" to reduce fallback-to-raw-text cases. Minor.

### E6 — `merge_to_main` skill prompt (LOW / no change)
Concise and clear. No change needed.

---

## 5. Cache-stability constraints (which changes are safe)

| Change | Location | Cache-safe? | Rationale |
|---|---|---|---|
| E1 (merge preamble+lifecycle) | stable head | ✅ one-time miss | All in `messages[0]`; change → one cache miss, then re-cached |
| E2 (trim tool tutorials) | stable head | ✅ one-time miss | Same as E1 |
| E3 (DRY summarization) | one-shot | ✅ n/a | Not cached |
| E4 (trim Executing arm) | volatile tail | ✅ no impact | Tail changes per step anyway; `CONTEXT_FOOTER` keeps prefix stable |
| Tool schema trims | `tools` array | ✅ one-time miss | Schemas sorted alphabetically (mod.rs:364), deterministic → cache-stable |

**Key invariant:** any change to the stable head (preamble, lifecycle, constitution) causes exactly **one** cache miss on the next turn, then re-caches. The `CONTEXT_FOOTER` sentinel ensures volatile-tail changes never invalidate the prefix. So all recommendations are cache-safe.

---

## 6. Summary

The prompt system is well-architected (stable-head/volatile-tail split, cache-stable footer, sorted tool schemas). The main inefficiency is **redundancy across the stable-head blocks** — the preamble, lifecycle map, and constitution each restate the research-planning, sub-plan, and closing-sequence guidance. Consolidating the preamble + lifecycle (E1) and trimming the preamble's tool tutorials (E2) would remove ~1700 chars from the stable head with no loss of information (the lifecycle + tool schemas + constitution cover everything). The volatile-tail Executing arm (E4) is the highest *per-step* cost since it's re-processed on every `complete_step`. The summarization DRY fix (E3) is clean but lower priority. No accuracy bugs found — only two low-severity imprecisions.

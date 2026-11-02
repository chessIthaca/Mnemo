+++
title = "chat tool cards — one human headline per run, raw tool names only on expand; mixed-tool bundling supersedes the never-merge rule (plan 8813e7ab)"
created = "2027-01-11"
+++

Plan 8813e7ab, branch wt/mnemo, review rounds 1-3 (PASS at .coding/reviews/2026-10-21-8813e7ab-toolcard-labels-review-round3.md). Commits: 36e7a74 (feature + round-1 fixes), e2729c8 (round-2 comment), 6219ec9 (round-3 PASS).

COLLAPSED CARD: one human sentence per RUN — Message.tsx header renders `groupPhrase(calls) ?? `Using ${prettyToolName(name)}``. The raw tool name NEVER shows while collapsed; it renders (font-mono) in the expanded per-call meta row (`#N · <name> · duration · ts`), which also carries that call's chips: file paths (read → Files tab at the line; file_edit/multi_edit → Diff tab via chip.opensDiff), web_fetch URL (opens in the app's Browser tab, ctrl-click = OS browser), backlog_add title (inline markdown), plus browser/search result summaries and the search literal-fallback note.

BUNDLING: agentEventReducer merges consecutive calls of DIFFERENT tools into ONE card (ToolInvocation now carries its own required `name`; entry.name = run's FIRST call). This SUPERSEDES the shell/search/browser never-merge directives (backlog daa38cbe + 24ddb845). Breaks: intervening text, a failed last call, MAX_CALLS_PER_TOOL_CARD. The shell live-preview fixed-height reservation keys off the first RUNNING SHELL call (`c.name === "shell"`), not the first running call.

HELPERS (frontend/src/lib/toolCardPaths.ts): actionPhrase (per-tool sentence, null for unknown/malformed), groupPhrase (joins legs with ", " + " and ", consecutive duplicates → ×N, ~120-char clamp, unknown legs join as "Using <pretty>"), prettyToolName (MCP prefix dropped), buildPathChips(calls) per-call names + opensDiff. displayName was REMOVED (dead).

OUT-OF-DIFF CONSUMERS the review caught (all fixed): lastDiff/planDiff snapshot keys off the completed call's name; countGitMutations reads each call's name; isKnowledgeActivityEntry scans every call for graph_*; InputBar /load normalizes legacy saves (calls' name ?? entry.name) and the helpers degrade instead of throwing on an unnamed call. Regression pins: useAgentStore mixed-card lastDiff, gitLanes mixed-card mutation, agentState/Conversation bundled graph classification, toolCardPaths legacy-unnamed + actionPhrase family table + source contracts. Gate: tsc clean, vitest 94/94 files, vite build OK, cargo test --workspace OK (Rust untouched).

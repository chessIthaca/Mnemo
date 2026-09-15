// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! System prompt builder — constitution + capabilities + workflow state.
//!
//! The compiled blocks (preamble, workflow lifecycle, app rules, tool
//! strategy, memory records, the per-state workflow guidance, and the
//! per-tool descriptors) are the only source of prompt text. The constitution
//! (global + project `agent.md`) is always the first thing in the system
//! prompt, re-read from disk each turn. The workflow section tells the model
//! where it is — especially after a restart.

use crate::memory::{Memory, ScoredMemory};
use crate::project::Constitution;
use crate::provider::Capabilities;
use crate::workflow::{Workflow, WorkflowState};

/// The coding system preamble — the base instructions.
///
/// Kept lean: the workflow state-machine, universal app rules, and per-tool
/// parameter semantics all live elsewhere (the `WORKFLOW_LIFECYCLE` + `APP_RULES`
/// consts below, and the tool schemas in the `tools` array). This block carries
/// only the core working principles + tool *strategy* (which tool to use when),
/// not the spec each tool schema already documents.
const CODING_SYSTEM_PREAMBLE: &str = "\
You are a coding agent operating under a plan-first workflow. You help the user \
build software by reading files, writing code, running commands, and managing a plan.

CORE PRINCIPLES:
- Plan before you code: create_plan lays out your steps first.
- Use the semantic memory system (memory_search) over search and shell.
- Complete one step at a time; complete_step checks each one off.
- Chain consecutive obvious steps: when the next action needs no user input \
and no decision, take it in the same turn (retry a failed call with corrected \
args, re-run a test after a fix, continue to finish after a commit). Tool \
results are continuations of your turn, not new questions — keep going until \
you hit a real choice point or need the user.
- Read before you write. Understand existing code before modifying it.
- Be precise. Use exact string matches for file_edit (its schema lists the \
regex, counted, line-range, and fuzzy-whitespace modes).
- To cut round-trips, read several files at once with read_files. Edit existing \
files with file_edit (targeted), not a full file_write rewrite; for large \
files, write the first section, then file_write mode:\"append\" the rest.
- File mutation goes through the file tools, not the shell: file_edit / \
file_write are the sanctioned writers (diff preview, line-ending safety, \
stale-read gate). Shell one-liners (Set-Content, sed -i, python scripts) are \
a last resort for paths the file tools refuse (.coding/knowledge/** — use \
memory_amend) or a verified tool freeze, with the justification stated.
- Explain what you're doing briefly.
- Ask questions when unsure, unless you know the user is not available.
";

/// The workflow lifecycle map — the conceptual arc the model reasons about
/// forward, complementing the per-state reactive text in `workflow_section`.
///
/// This is part of the **stable head** (byte-stable across turns — it changes
/// only when `agent.md` is edited). It tells the model the state machine, which
/// tools are visible in each state, and — crucially — that a plan's `kind`
/// (set at `create_plan`) decides whether completion enters Reviewing
/// (implementation) or skips straight to Complete (research). Without this map
/// the model only sees reactive per-state guidance and cannot reason about the
/// arc or about skipping review for investigation work.
const WORKFLOW_LIFECYCLE: &str = "\
WORKFLOW LIFECYCLE — a plan-first state machine. Tool visibility per state is \
enforced, not advisory:

- PLANNING: read tools + create_plan only — no project mutation without a \
plan. Always plan first, even research/investigation work: kind:\"research\" \
structures it into steps and skips the end-of-plan review.
- EXECUTING: all tools. complete_step in order. create_plan pushes a sub-plan \
AT ANY POINT — when it finishes, the parent resumes where it left off. \
update_plan edits the active plan in place; abandon_plan is a destructive \
last resort.
- REVIEWING: steps done + kind=implementation. Run the closing sequence (APP \
RULES): reviewer subagent → fix findings → re-run tests → commit → finish. \
finish is the only exit to Complete and is gated on a non-empty review \
report.
- COMPLETE: read tools + create_plan only — plan any follow-up change. \
Skills (e.g. merge_to_main) may also start here.

PLAN KIND (set at create_plan, persisted): \"implementation\" (default) → \
review required. \"research\" → skips review; ONLY for work that produces no \
source-code changes. Bugs: ALWAYS kind:\"bug_fixing\", never \
\"implementation\" — locked reproduce→root-cause→fix→verify skeleton, \
required bug:{symptom} param, BUG: memory auto-captured at finish — EXCEPT \
bug-triggered FEATURES (the fix adds capabilities, new dependencies, or \
spans multiple modules): file those as kind:\"implementation\" with the bug \
documented as motivation in goal/context; bug_fixing is for CONTAINED \
defect fixes. The plan \
file is the CRASH-RESUMPTION document: detail out the bug (symptom, root \
cause, code locations, fix + test design) in bug + context + steps so a \
mid-fix restart continues smoothly — provided steps persist as the \
'Detailed steps' section, not discarded.
";

/// Universal app-mechanics rules — hard rules that apply to EVERY project,
/// baked into the compiled stable head so a fresh project (with only the lean
/// `DEFAULT_PROJECT_TEMPLATE` agent.md) inherits them automatically. The
/// project `agent.md` holds project-specific policy only (test command,
/// language, environment); these rules are app-level mechanics, not policy.
///
/// This is part of the **stable head** (byte-stable across turns — it changes
/// only when the app binary is rebuilt). It carries: the never-commit-to-main
/// rule, the core-operations approval gate, the bookkeeping-tools-never-prompt
/// note, the never-resend-failed-call rule, line-ending preservation, the
/// final-summary length rule, and the full closing sequence (test → review →
/// fix → commit → finish) that previously lived only in this project's
/// agent.md.
const APP_RULES: &str = "\
APP RULES — universal hard rules (every project):

- Never commit to main — commit to the current working branch. Only exception: \
the user-initiated merge_to_main skill.
- Core operations (git merge, git push) are ALWAYS approval-gated; no safety \
mode or skill bypasses this.
- Bookkeeping tools (memory_*, create_plan, update_plan, complete_step, \
abandon_plan) never prompt — they touch only sandboxed .coding/ state.
- Never resend a failed tool call with identical arguments. Errors arrive as \
[tool error] blocks: parse the text, fix the named parameter, re-issue \
corrected immediately — in the same turn, without pausing to narrate the \
error first.
- Preserve line-ending style of existing files.
- Final summaries: ≤3 short lines (what changed, how to use it, test result).

CLOSING SEQUENCE (implementation plans; research plans skip review):
1. Test — run the project's tests; fix failures before continuing.
2. Review — spawn_agent role:\"reviewer\" (read-only; see its schema), \
OMITTING the model param — the configured reviewing model is authoritative. \
It reviews ALL uncommitted changes, not just its own task. Give it a \
self-contained task: changed files + plan goal; ask for correctness, bugs, \
security, constitution findings. The report MUST open \"## Verdict: PASS\" or \
\"## Verdict: FINDINGS (n high, n low)\" — unparseable = FINDINGS (fail \
closed). Bug plans additionally check: regression test exercises the changed \
path, root cause documented, BUG: memory written. After spawning, END the \
turn — the finish notification resumes you; never sleep/poll. When it resumes \
you, immediately continue the closing sequence — read the report, fix \
findings, re-run tests, commit, finish — don't stall or wait for the user. \
Reviewer failed \
WITHOUT a report: NOT done — never respawn blindly (same failure recurs); \
ask_user (retry on another model / abandon review). The main agent can NEVER \
author a review itself — write_review_report exists only for role:\"reviewer\" \
agents and .coding/reviews/ is protected from the file tools; abandon_plan is \
the escape if the review cannot be completed.
3. Fix EVERY finding — the only permitted skip is a factually wrong finding, \
with written justification. Re-run the project's tests after fixing.
4. Commit to the current working branch; include the review report.
5. finish(report path) → Complete.
";

/// Tool-call *discipline* — how every call is composed. APP_RULES covers what
/// to do when a call FAILS (never resend identical); this block covers how to
/// compose the call so it does not fail in the first place. The runtime
/// catches only the mechanical misses (blank required fields, malformed JSON);
/// the drafting discipline — content first, schema checklist, complete-call
/// rewrite — can only be stated here, at generation time. Kept lean: the head
/// is cached but still costs tokens once per session.
const TOOL_CALL_DISCIPLINE: &str = "\
TOOL-CALL DISCIPLINE — generation-time rules for every call:
- CONTENT FIRST, CALL SECOND — the JSON body of a tool call delivers text you \
already wrote; it is never the place where the text gets invented. A \
create_plan/update_plan is valid only when its title/goal/steps content is \
fully written in this same message: write the content in your reply first, \
then emit the call carrying that exact text. If you catch yourself opening a \
call with no content drafted, you are not ready to call — write the content, \
then call.
- SCHEMA-FIRST PRE-FLIGHT — before emitting, name the tool and its required \
fields, then check each is present and non-blank in your call: create_plan → \
title, goal, steps (non-empty array; kind=\"bug_fixing\" also requires bug); \
update_plan → steps (replacement) or steps + append:true; complete_step → \
step_index; memory_write → tier, title, content; ask_user → question + \
options (≥2); spawn_agent → name, task; file_edit/file_write → path (+ \
new_string/content); shell → command, purpose; search → pattern. If you \
cannot name the fields, re-read the schema BEFORE emitting — never emit to \
find out. No call goes out with an empty argument object when it has required \
fields (a few tools are genuinely argument-free: current_plan, backlog_list, \
memory_search browse) — a blank required field means the content was never \
written.
- ESCAPE TRAPS — a lone backslash starts an illegal JSON escape (\\x, \\d): \
write Windows paths with forward slashes, C:/repo/path.
- MALFORMED-JSON RECOVERY — \"arguments JSON was malformed or truncated\" \
means the argument object was empty, cut off, or carried an illegal escape. \
It is NOT a hint to shrink the call or tweak one field: re-read the tool's \
schema, rewrite the COMPLETE call with every required field, and emit the \
corrected call once.
- TWO IDENTICAL FAILURES IN A ROW = STOP EMITTING — you are stuck in a loop: \
re-read the tool's schema, re-draft the complete arguments from scratch, then \
emit one carefully built call. The APP_RULES never-resend rule applies to \
parse failures too — an identical call fails identically every time.
";

/// Tool *strategy* — which tool to use when. The stable head already carries
/// the workflow state-machine + app rules; this block fills the gap for tools
/// whose JSON schema documents *parameters* but not *when to reach for them*.
/// Kept lean (the head is cached but still costs tokens once per session): each
/// line is the punchline, not a re-statement of the schema.
///
/// The memory-strategy line is the key "learning agent" lever: the recall +
/// consolidation machinery is solid, but it only pays off if the model
/// *proactively* feeds it distilled facts. Without this nudge the model relies
/// on auto-capture (working-tier raw events) and rarely writes the
/// semantic/procedural memories that the session primer surfaces next time.
///
/// 2026-08-22 hardening (user report: "semantic tools are hardly used"): the
/// soft nudges underperformed — the rules are now phrased as MANDATORY
/// triggers with concrete fire conditions ("the moment you learn…", "BEFORE
/// non-trivial work…", "NEVER read whole files to locate a symbol"), not
/// suggestions the model can safely skip. The 2026-09-10 cost-model line
/// makes the ordering rationale explicit (indexed lookup beats tree walk
/// beats process spawn), pairing with the tool-result steering layer
/// (search→graph nudge, graph-miss→search hint, shell grep tip, the
/// create_plan RECALLED CONTEXT rider).
///
/// 2026-09-15 gap-driven rewording: the memory_search mid-session clause
/// changed from the reflexive "whenever auto-recalled context looks thin"
/// to fire only on a GAP the recalled hits + plan rider do not cover
/// (evidence: the 2026-09-15 search-usage tally showed 6/9 redundant
/// mid-session lookups; both MANDATORY triggers stay verbatim, and the
/// TOOL_STRATEGY order is pinned by tests — wording edits only).
const TOOL_STRATEGY: &str = "\
TOOL STRATEGY — the \"when\" (each schema documents the parameters). The first \
two habits are MANDATORY, not suggestions:

Cost model: memory_search and the graph_* tools are single indexed lookups; \
search walks the tree file-by-file; shell spawns a whole process — always take \
the cheapest tool that can answer the question.

- memory_write: the moment you learn something durable (decision, root cause, \
convention, user correction) — semantic for facts, procedural for workflows. \
Deferred = lost; auto-capture is not enough.
- memory_search: the ONE read path — search with a query, browse without one, \
narrow with record_type/tier. MANDATORY before planning non-trivial work (no \
record_type — a pointer-first bundle across plans/reviews/bugs/decisions). \
MANDATORY before diagnosing any bug (record_type:\"bug\" — a recorded root \
cause + regression test may already exist). Mid-session, search memory only \
when the auto-recalled hits and the plan rider do not cover the question \
(auto-recall covers only the latest message) — an explicit lookup that \
returns what auto-recall already carried is a wasted round-trip; the search \
is for a GAP, not a reflex.
- Auto-recalled memories are injected each turn — use them silently; if they \
are not useful to the current task, do not narrate or apologize for them.
- memory_consolidate: mid-session distillation of a long session only.
- graph_search / graph_context / graph_impact / graph_path: MANDATORY first \
step for SYMBOL questions (where defined, callers, blast radius, \
reachability) — in ANY indexed language: Rust, TypeScript/TSX, JavaScript, \
Python, Go, Java, C/C++, C#, Ruby, PHP, and HTML script blocks (the graph \
indexes .rs/.ts/.tsx/.js/.py/... sources; a symbol lookup never starts \
with grep or a whole-file read, whatever the language). graph_impact \
before editing a shared/public symbol — frontend lib exports count too. \
String literals (tool names, config keys, log text) are NOT indexed — use \
search for those.
- search / search_read: FIRST choice for non-symbol text (literals, config \
keys, comments) and FALLBACK when the graph comes up empty; when a hit \
surfaces a symbol, switch to graph_context.
- spawn_agent: parallel review/investigation; role:\"reviewer\" is read-only.
- ask_user: a concrete choice between ≥2 options — never open-ended.
- image_* tools: task-specific image analysis; image_analysis is the generic \
fallback; detail_level='fine' for tiny text.
- convert_line_endings: CRLF↔LF in place — no shell one-liner.
- web_fetch: read-only URL fetch for research; usable in any state.
";

/// The typed-record + memory-hygiene conventions. Ships in the SAME phase as
/// the hygiene tools (user requirement 2026-08-22: prompt enforcement is its
/// own visible step, not a follow-up) — the tools without the habits would be
/// dead code. Lives in the stable head: the head is prefix-cached, so this
/// costs tokens once per session — keep it LEAN.
const MEMORY_RECORDS: &str = "\
MEMORY RECORDS — typed title prefixes on memory_write / memory_supersede. \
Keep typed records compact and pointer-first — the gist + a path/commit \
pointer; the file carries the detail. The derived index auto-truncates \
digests to stay compact.

- SPEC: how a feature works · DECISION: choice + rationale · BUG: symptom → \
root cause → fix + regression test name · PLAN: plan digest · HOW: recurring \
workflow · REVIEW: report digest.

Pointer-first: a typed record carries the gist + path/commit pointer; the \
file stays the truth — read the file for detail.

Hygiene: memory_supersede when a new fact contradicts/obsoletes a stored one \
— NEVER leave both live. memory_update refines a still-current record by id. \
memory_amend appends a dated amendment paragraph to a knowledge record (the \
sanctioned .coding/knowledge/** writer). memory_delete is junk/duplicates \
ONLY — stale state is superseded, never deleted. memory_search with no query \
browses.

Branch-status default: a feature/bug with no live memory saying otherwise is \
assumed IN main — ABSENCE of an unmerged marker means merged (verify with \
git_read when it matters).
";

/// The default static guidance for the PLANNING state of the volatile tail's
/// workflow section.
const STATE_PLANNING: &str = "\
PLANNING: read tools only. Explore, then create_plan your steps. Write tools \
unlock only when a plan exists — no project changes without one.
";

/// The static guidance shown under the current step in EXECUTING; the dynamic
/// CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP lines are interpolated from
/// live workflow state.
const STATE_EXECUTING: &str = "Complete the current step, then complete_step. Stay in scope.\n";

/// The static guidance for the REVIEWING state — the closing-sequence
/// driver.
///
/// Deliberately a POINTER, not a restatement: this block lives in the volatile
/// tail, which sits after the cached stable head and is re-processed on every
/// turn. The verdict rule, the bug checklist and the reviewer-only authorship
/// rule are stated once, in `APP_RULES` (cached). Keeping a second copy here
/// cost ~200 tokens per Reviewing turn and bought nothing — the head is in
/// context either way. Enforced by
/// `reviewing_state_block_points_at_app_rules_without_duplicating_them`.
const STATE_REVIEWING: &str = "\
All steps complete — REVIEWING: run the closing sequence (APP RULES) — \
spawn_agent role:\"reviewer\" (omit the model param — the configured \
reviewing model is authoritative) over ALL uncommitted changes, then fix EVERY \
finding yourself (the write tools are available to YOU, not the reviewer), \
re-run the tests, commit, and finish(report path). No complete_step / \
create_plan here — the plan is done; update_plan IS available (fix-findings \
scope changes and appended follow-on steps — the workflow accepts full edits \
in Reviewing; complete_step stays hidden, so appended steps can't be checked \
off mid-review). backlog_add / backlog_status are available too — a user \
request to queue or update a backlog item is never blocked mid-review.
";

/// The static guidance for the COMPLETE state.
///
/// Directive (user request 2026-09-06: "more eager switching to plan"): tells
/// the agent WHEN to plan (code-change task) vs. when to answer freeform (pure
/// question), so it doesn't explain how it *would* fix something instead of
/// actually planning + executing it. The code-level nudge
/// ([`append_plan_nudge`]) reinforces this when a work verb is detected in the
/// user's latest message.
const STATE_COMPLETE: &str = "\
All steps complete. If the user's message implies code changes (fix, add, \
implement, refactor, update + a code reference), treat it as a NEW TASK: \
explore with read tools, then call create_plan — do NOT answer freeform. \
Only answer freeform for pure questions (how/where/what). Skills (e.g. \
merge_to_main) may also start here.
";

/// The static guidance for the SUBAGENT state — a parented sub-agent's role
/// state (2026-01-03): a single-task worker, not a plan-lifecycle owner. The
/// plan context above it is the parent's, shown read-only.
const STATE_SUBAGENT: &str = "\
You are a background sub-agent: a single-task worker spawned by a parent. \
Work the task in your spawn prompt to completion and put your findings in \
your final answer (the parent reads it). The plan context above is your \
parent's — read-only orientation, not a work order: you cannot create or \
advance plans, and your tools are exactly your spawn-time allow-list. When \
done, stop — no plan lifecycle applies to you.
";

/// The directive text appended to the volatile tail when the agent is resting
/// in Complete state and the user's latest message looks like a code-change
/// task. Reinforces [`STATE_COMPLETE`] at the point of decision.
const PLAN_NUDGE_TEXT: &str = "\
The user's message looks like a code-change task. You are in Complete state — \
explore with read tools if needed, then call create_plan to start a new plan. \
Do NOT answer freeform or explain how you would do it; plan and execute it.\
";

/// Whether a user message looks like a code-change task rather than a pure
/// question — used to nudge the agent toward planning when it's resting in
/// Complete state.
///
/// Fires when the message contains an imperative work verb (fix, add,
/// implement, refactor, …) as a word or a common inflection (so "fixing",
/// "added", "updated", "builder" all match). Matching is exact-or-suffix, not
/// a raw prefix test: "address" does NOT match "add", "wireless" does NOT
/// match "wire", "movement" does NOT match "move" — the false positives a
/// `starts_with` check would introduce. Conservative by design: false
/// positives are harmless (the nudge is a suggestion, not a forced transition
/// — the agent can still answer a question); false negatives fall back to the
/// [`STATE_COMPLETE`] prompt guidance, which also directs the agent to plan.
///
/// English-only: the verb list is English, so CJK and other non-Latin scripts
/// are false negatives (they fall back to [`STATE_COMPLETE`], which still
/// directs the agent to plan).
fn looks_like_work_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    let work_verbs = [
        "fix",
        "add",
        "implement",
        "refactor",
        "update",
        "create",
        "build",
        "remove",
        "delete",
        "change",
        "move",
        "rename",
        "extract",
        "optimize",
        "handle",
        "support",
        "enable",
        "disable",
        "migrate",
        "convert",
        "replace",
        "reorder",
        "wire",
        "hook",
    ];
    // Common English inflection suffixes. Matching the stem against the verb
    // OR verb+suffix catches "fixing/added/updated/builder" while excluding
    // "address/wireless/movement/fixture" (whose remainders are not valid
    // suffixes). Allocation-free: `strip_prefix` returns a &str slice.
    const SUFFIXES: [&str; 7] = ["ing", "ed", "es", "s", "d", "er", "ers"];
    lower.split_whitespace().any(|word| {
        let stem = word.trim_matches(|c: char| !c.is_alphanumeric());
        !stem.is_empty()
            && work_verbs.iter().any(|v| {
                if stem == *v {
                    return true;
                }
                stem.strip_prefix(v)
                    .map_or(false, |rest| SUFFIXES.contains(&rest))
            })
    })
}

/// Append the eager-plan nudge to the volatile tail when the workflow is in
/// Complete state and the user's latest message looks like a code-change task.
///
/// The nudge is a directive system-prompt addition that tells the agent to
/// plan instead of answering freeform — the "more eager switching to plan"
/// the user asked for. It fires ONLY in Complete state (the resting state
/// where the agent has no active plan) and ONLY when the user's message
/// contains a work verb. In every other state the agent already has a plan
/// (Executing) or is about to create one (Planning), so the nudge is moot.
pub(crate) fn append_plan_nudge(
    tail: &mut String,
    state: crate::workflow::WorkflowState,
    user_message: Option<&str>,
) {
    if state == crate::workflow::WorkflowState::Complete {
        if let Some(msg) = user_message {
            if looks_like_work_intent(msg) {
                tail.push_str("\n# PLAN NUDGE\n\n");
                tail.push_str(PLAN_NUDGE_TEXT);
                tail.push('\n');
            }
        }
    }
}

/// Byte-stable footer appended by `turn.rs` as the FINAL message, after the
/// volatile tail (workflow state + recalled memories). Its ROLE follows the
/// vendor: a system message for providers that accept trailing system blocks, a
/// USER message for Local/Ollama (a system message may only be first) and
/// DeepSeek-vendor (`ProviderPolicy::tail_as_user_messages` — its models echo
/// trailing system blocks instead of answering).
///
/// Per the empirically-observed provider cache law (DeepSeek reuses the request
/// byte prefix only when the request's LAST message is byte-identical to the
/// previous request's last message — verified in `.coding/analysis/
/// cache-hit-2-report.md`), a constant final message lets `complete_step`
/// progress bumps and new turns (which only change the volatile tail) cost
/// just the tail + footer to re-process instead of the whole cached context.
///
/// Constraints (enforced by test): it MUST be a fixed literal — no
/// interpolation, timestamps, or volatile content — and must NOT contain
/// `WORKFLOW STATE` or `RECALLED MEMORIES`. Keep it short (30-60 chars).
pub const CONTEXT_FOOTER: &str = "<context footer — cache-stable sentinel, ignore>";

/// Append a section to the head, skipping empty ones (mirrors how the
/// constitution sections are skipped).
fn push_section(out: &mut String, text: &str) {
    if !text.trim().is_empty() {
        out.push_str(text);
        out.push_str("\n\n");
    }
}

/// Build the **stable head** of the system prompt: the six compiled prompt
/// blocks + the global + project constitution.
///
/// This block is byte-stable across turns — it changes only when the
/// `agent.md` files are edited (re-read from disk, mtime-guarded). It is the
/// ONLY content placed in `messages[0]`, on every vendor, so the provider's
/// prompt-cache prefix survives `complete_step` progress bumps, workflow-state
/// transitions, and new turns (which only change the volatile tail). That tail
/// and [`CONTEXT_FOOTER`] ride at the END instead: as user-role messages on
/// Local/Ollama and DeepSeek-vendor (`ProviderPolicy::tail_as_user_messages` —
/// those reject or echo trailing *system* blocks) and as system-role ones
/// elsewhere. Folding the tail in here (the previous strategy for those two
/// vendor classes) is what made every plan-item check-off re-read the whole
/// conversation: measured 2026-09-14, 5.1-9.0% cache hit and 115-120K tokens
/// re-billed per `complete_step`.
pub fn build_stable_head(constitution: &Constitution) -> String {
    let mut prompt = String::new();

    push_section(&mut prompt, CODING_SYSTEM_PREAMBLE);
    push_section(&mut prompt, WORKFLOW_LIFECYCLE);
    push_section(&mut prompt, APP_RULES);
    push_section(&mut prompt, TOOL_CALL_DISCIPLINE);
    push_section(&mut prompt, TOOL_STRATEGY);
    push_section(&mut prompt, MEMORY_RECORDS);

    // Constitution — global first, then project.
    if !constitution.global.trim().is_empty() {
        prompt.push_str("# HARD RULES — GLOBAL (agent.md)\n\n");
        prompt.push_str(&constitution.global);
        prompt.push_str("\n\n");
    }
    if !constitution.project.trim().is_empty() {
        prompt.push_str("# HARD RULES — PROJECT (agent.md)\n\n");
        prompt.push_str(&constitution.project);
        prompt.push_str("\n\n");
    }

    prompt
}

/// Build the **volatile tail** of the system prompt: the recalled-memories
/// block (when present) + the workflow-state section.
///
/// The workflow section's static per-state guidance comes from the compiled
/// STATE_* consts; the dynamic lines — CURRENT PLAN / GOAL / PROGRESS /
/// CURRENT STEP, the skill goal, the sub-agent notes, CAPABILITIES — are
/// interpolated from live workflow state.
///
/// This changes per step/turn (PROGRESS bumps, CURRENT STEP text, recall
/// results). Production (`turn.rs`) appends it AFTER the conversation history,
/// where the provider's prefix cache is immune — so its volatility never
/// invalidates the cached stable head — and then appends [`CONTEXT_FOOTER`] as
/// the FINAL message, so the request's last message stays byte-stable across
/// tail changes (the provider reuses the prefix only when the last message is
/// byte-identical). Both ride as USER messages on Local/Ollama and
/// DeepSeek-vendor (`ProviderPolicy::tail_as_user_messages` — those reject or
/// echo trailing system blocks) and as system messages elsewhere. Because the
/// tail is the only reprocessed part per change, it is kept compact: CURRENT
/// STEP shows the step header (or a 120-char truncation) and memory entries cap
/// content at 160 chars.
pub fn build_volatile_tail(
    caps: &Capabilities,
    workflow: &Workflow,
    memories: Option<&str>,
) -> String {
    let mut prompt = String::new();

    // Recalled memories (auto-recalled based on the user's prompt).
    if let Some(mem) = memories {
        if !mem.trim().is_empty() {
            prompt.push_str("# RECALLED MEMORIES\n\n");
            prompt.push_str(mem);
            prompt.push_str("\n\n");
        }
    }

    // Workflow section.
    prompt.push_str(&workflow_section(caps, workflow));

    prompt
}

/// Build the full system prompt by concatenating the stable head + volatile
/// tail.
///
/// This is a back-compat wrapper: tests and any single-message caller use it
/// to get the same string the old monolithic builder produced. Production
/// (`turn.rs`) calls [`build_stable_head`] and [`build_volatile_tail`]
/// separately so the head can live in `messages[0]` (cache-stable), the tail
/// can be appended after the conversation history (cache-immune), and
/// [`CONTEXT_FOOTER`] can be appended as the final message after the tail
/// (cache-stable sentinel — this wrapper deliberately does NOT include it).
pub fn build_system_prompt(
    caps: &Capabilities,
    workflow: &Workflow,
    constitution: &Constitution,
    memories: Option<&str>,
) -> String {
    let mut prompt = build_stable_head(constitution);
    prompt.push_str(&build_volatile_tail(caps, workflow, memories));
    prompt
}

/// Truncate a step's display text to a short punchline for the volatile tail.
///
/// The tail is the only part re-processed per `complete_step` (the R5
/// byte-stable footer keeps everything before it cached), so the CURRENT STEP
/// line must stay short: the bold header when present, else the first 120
/// chars. Char-safe (`chars().take` never splits a UTF-8 sequence).
fn short_step_text(text: &str) -> String {
    const MAX: usize = 120;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// One-line, budget-truncated form of a plan's Bug/Context for the
/// volatile tail (backlog 77ff8f45): a fresh session picking up an
/// in-flight plan gets the symptom + the context gist in the footer —
/// the tail is its only plan-derived injection. The full detail,
/// including a bug plan's `## Detailed steps`, stays in the plan file
/// (the crash-resumption document). Whitespace-collapsed to one line and
/// char-safe (`chars().take` never splits a UTF-8 sequence).
fn short_context_line(text: &str, budget: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= budget {
        collapsed
    } else {
        let head: String = collapsed.chars().take(budget).collect();
        format!("{head}…")
    }
}

/// Format recalled memories into a compact section for the system prompt.
///
/// Each entry shows the tier, title, stable id, score, and the first 160
/// chars of content — long enough to convey the gist, short enough to keep
/// the volatile tail (the only per-turn reprocessed bytes) small. The id
/// (first parenthesized field) lets the agent supersede/update a memory
/// straight from recalled context.
pub fn format_recall_context(memories: &[ScoredMemory]) -> String {
    let mut out = String::new();
    for sm in memories {
        out.push_str(&format!(
            "[{}] {} (id: {}, score: {:.2})\n  {}\n\n",
            sm.memory.tier,
            sm.memory.title,
            sm.memory.id,
            sm.score,
            sm.memory.content.chars().take(160).collect::<String>(),
        ));
    }
    out
}

/// Format the standing project-memory primer (the top strongest
/// semantic+procedural memories) into a compact section for the stable head.
///
/// Unlike the per-turn recalled-memories block (which is query-driven and
/// lives in the volatile tail), the primer is fetched once at session start
/// and stays byte-stable for the whole session — so it can live in the cached
/// stable head without invalidating the prefix cache. Each entry shows the
/// tier, title, stable id, and the first 200 chars of content (a bit more
/// than the volatile-tail 160, since this is paid once per session, not per
/// turn). The id lets the agent supersede/update a memory straight from the
/// standing context. Returns an empty string when there are no memories (the
/// caller omits the section entirely).
pub fn format_primer(memories: &[Memory]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut out = String::from("# PROJECT MEMORY (standing context)\n\n");
    for m in memories {
        out.push_str(&format!(
            "[{}] {} (id: {}) — {}\n",
            m.tier,
            m.title,
            m.id,
            m.content.chars().take(200).collect::<String>(),
        ));
    }
    out
}

/// Push one state's static guidance, skipping empty sections and
/// normalizing the trailing newline — byte-identical to the compiled consts,
/// which each end with exactly one `\n`.
fn push_state_text(out: &mut String, text: &str) {
    let t = text.trim_end();
    if !t.is_empty() {
        out.push_str(t);
        out.push('\n');
    }
}

/// Build the workflow section of the system prompt.
///
/// The static per-state guidance comes from the compiled STATE_* consts; the
/// dynamic lines — CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP, the skill
/// name/goal, the sub-agent plan context, the CAPABILITIES block — are interpolated
/// from live workflow state.
fn workflow_section(caps: &Capabilities, workflow: &Workflow) -> String {
    let state = workflow.state();
    let mut s = String::new();

    s.push_str("# WORKFLOW STATE\n\n");
    s.push_str(&format!("Current state: {state}\n\n"));

    match state {
        WorkflowState::Planning => {
            push_state_text(&mut s, STATE_PLANNING);
        }
        WorkflowState::Executing => {
            if let Some(plan) = workflow.plan() {
                // Show the nesting breadcrumb when this is a sub-plan, so the
                // agent knows there's a parent to fall back to.
                let parents = workflow.parent_titles();
                if parents.is_empty() {
                    s.push_str(&format!("CURRENT PLAN: {}\n", plan.title));
                } else {
                    s.push_str(&format!(
                        "CURRENT PLAN (sub-plan, depth {}): {} ▸ {}\n",
                        workflow.plan_depth(),
                        parents.join(" ▸ "),
                        plan.title
                    ));
                }
                s.push_str(&format!("GOAL: {}\n", plan.goal));
                // Backlog 77ff8f45: surface the Bug + Context in the
                // resume footer — a fresh session picking up an
                // in-flight plan gets the symptom + the context gist
                // here; the full detail, including a bug plan's
                // Detailed steps, stays in the plan file (the
                // crash-resumption document). One line each — the tail
                // is re-processed per step bump, so every token is paid
                // on every bump.
                if let Some(bug) = &plan.bug_symptom {
                    s.push_str(&format!("BUG: {}\n", short_context_line(bug, 160)));
                }
                if !plan.context.trim().is_empty() {
                    s.push_str(&format!(
                        "CONTEXT: {}\n",
                        short_context_line(plan.context.trim(), 240)
                    ));
                }
                s.push_str(&format!(
                    "PROGRESS: {}/{} steps complete\n",
                    plan.completed_count(),
                    plan.steps.len()
                ));
                if let Some(step) = plan.current_step() {
                    // Show the step HEADER (the bold punchline) when present,
                    // else a truncated body. The full step text stays
                    // available in the conversation (the create_plan /
                    // update_plan results). Keeping this line short matters:
                    // with the R5 CONTEXT_FOOTER the volatile tail is the
                    // only part re-processed per complete_step, so every
                    // token here is paid on every step bump.
                    let current = step
                        .header
                        .as_deref()
                        .map(short_step_text)
                        .unwrap_or_else(|| short_step_text(&step.text));
                    s.push_str(&format!("CURRENT STEP: \"{current}\" ← resume here\n\n",));
                    push_state_text(&mut s, STATE_EXECUTING);
                }
            }
        }
        WorkflowState::Reviewing => {
            push_state_text(&mut s, STATE_REVIEWING);
        }
        WorkflowState::Complete => {
            push_state_text(&mut s, STATE_COMPLETE);
        }
        WorkflowState::Skill => {
            if let Some(skill) = workflow.active_skill() {
                s.push_str(&format!(
                    "You are running the skill \"{name}\". Drive toward this goal:\n\n{prompt}\n\n",
                    name = skill.name,
                    prompt = skill.prompt
                ));
                s.push_str(&format!(
                    "Goal achieved → skill_end (exits to {target}); rollback → \
                     abandon_skill. Security controls still apply to every tool \
                     call.\n",
                    target = skill.target_state
                ));
            } else {
                // Defensive: Skill state with no active skill overlay. This
                // shouldn't happen (start_skill sets both atomically), but if
                // it does, tell the agent to end the skill so the workflow
                // recovers to a sane state.
                s.push_str(
                    "A skill is nominally active but no skill overlay is recorded. Call \
                     skill_end or abandon_skill to return to a normal workflow state.\n",
                );
            }
        }
        WorkflowState::Subagent => {
            // Parented sub-agent: the plan stack below is the READ-ONLY MIRROR
            // of the main plan (loaded from the shared plans dir by
            // build_inner) — orientation, not a work order. The sub-agent's
            // actual task arrives in its spawn prompt; it cannot mutate plans
            // and its tool surface comes from its spawn-time allow-list.
            if let Some(plan) = workflow.plan() {
                let parents = workflow.parent_titles();
                if parents.is_empty() {
                    s.push_str(&format!("PARENT PLAN (read-only context): {}\n", plan.title));
                } else {
                    s.push_str(&format!(
                        "PARENT PLAN (read-only context, depth {}): {} ▸ {}\n",
                        workflow.plan_depth(),
                        parents.join(" ▸ "),
                        plan.title
                    ));
                }
                s.push_str(&format!("GOAL: {}\n", plan.goal));
                s.push_str(&format!(
                    "PROGRESS: {}/{} steps complete\n\n",
                    plan.completed_count(),
                    plan.steps.len()
                ));
            }
            push_state_text(&mut s, STATE_SUBAGENT);
        }
    }

    // Capability notes.
    s.push_str("\n# CAPABILITIES\n\n");
    if caps.supports_tool_choice {
        s.push_str("- Tool choice: supported (you may be directed to call specific tools).\n");
    } else {
        s.push_str("- Tool choice: not supported. Choose tools freely.\n");
    }
    if !caps.reliable_finish_reason {
        s.push_str(
            "- Finish reason may be unreliable; tool calls are detected by their presence.\n",
        );
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Constitution;
    use crate::workflow::Workflow;
    use tempfile::tempdir;

    #[test]
    fn planning_prompt_mentions_planning() {
        let dir = tempdir().unwrap();
        let wf = Workflow::new(dir.path());
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(prompt.contains("PLANNING"));
        assert!(prompt.contains("create_plan"));
    }

    #[test]
    fn lifecycle_encourages_research_planning_and_subplans() {
        // The stable head's workflow-lifecycle map must (a) encourage planning
        // even for research/investigation work (kind:"research"), and (b)
        // emphasize that sub-plans can be created at any point during Executing.
        let dir = tempdir().unwrap();
        let wf = Workflow::new(dir.path());
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(
            prompt.contains("research"),
            "lifecycle should encourage research planning"
        );
        assert!(
            prompt.contains("sub-plan"),
            "lifecycle should mention sub-plans"
        );
        assert!(
            prompt.contains("AT ANY POINT"),
            "lifecycle should emphasize sub-plans can be created any time"
        );
    }

    #[test]
    fn preamble_carries_core_working_principles() {
        // The stable head's CORE PRINCIPLES (the very first thing the model
        // reads) must carry the core working principles directly — not rely on
        // the later WORKFLOW LIFECYCLE map alone. After the dedupe, the
        // workflow emphases (tool-gating, research planning, sub-plans) live
        // in the WORKFLOW_LIFECYCLE const (covered by
        // `lifecycle_encourages_research_planning_and_subplans`); the preamble
        // carries the working principles unique to it. Each assertion uses a
        // substring unique to the preamble (absent from WORKFLOW_LIFECYCLE),
        // so a regression that strips the preamble while leaving the lifecycle
        // intact would fail here — keeping this test disjoint from the
        // lifecycle test.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("Plan before you code"),
            "preamble should carry the plan-first principle"
        );
        assert!(
            head.contains("Read before you write"),
            "preamble should carry the read-before-write principle"
        );
        assert!(
            head.contains("Be precise"),
            "preamble should carry the precision principle"
        );
        // The 2026-04 user-added principles: memory-first + ask-when-unsure.
        assert!(
            head.contains("semantic memory system"),
            "preamble should carry the memory-first principle"
        );
        assert!(
            head.contains("Ask questions when unsure"),
            "preamble should carry the ask-when-unsure principle"
        );
        // The chaining principle: counter the asymmetric "stop" bias by telling
        // the model to continue obvious consecutive steps in the same turn.
        assert!(
            head.contains("Chain consecutive obvious steps"),
            "preamble should carry the chain-consecutive-steps principle"
        );
        assert!(
            head.contains("continuations of your turn, not new questions"),
            "preamble should frame tool results as turn continuations"
        );
    }

    #[test]
    fn stable_head_contains_universal_closing_rules() {
        // The universal closing-sequence + app-mechanics rules (APP_RULES) are
        // baked into the compiled stable head so every project inherits them —
        // a fresh project with only the lean DEFAULT_PROJECT_TEMPLATE agent.md
        // must still get them. Assert the head carries the key markers.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("CLOSING SEQUENCE"),
            "head should carry the closing-sequence section"
        );
        assert!(
            head.contains("role:\"reviewer\""),
            "head should instruct spawning a reviewer subagent"
        );
        assert!(
            head.contains(".coding/reviews/"),
            "head should name the review-report directory"
        );
        assert!(
            head.contains("Fix EVERY finding"),
            "head should mandate fixing every review finding"
        );
        assert!(
            head.contains("Never commit to main"),
            "head should carry the never-commit-to-main rule"
        );
        assert!(
            head.contains("working branch"),
            "head should instruct committing to a working branch"
        );
        assert!(
            head.contains("never prompt"),
            "head should note bookkeeping tools never prompt"
        );
        assert!(
            head.contains("Never resend a failed tool call"),
            "head should carry the never-resend rule"
        );
        // The retry-cadence clause: tell the model to re-issue corrected
        // immediately, without pausing to narrate the error first.
        assert!(
            head.contains("without pausing to narrate"),
            "head should carry the retry-cadence clause"
        );
        // The resume-cadence clause: when the reviewer-finish notification
        // resumes the agent, it must immediately continue the closing sequence
        // rather than stalling and waiting for the user.
        assert!(
            head.contains("immediately continue the closing sequence"),
            "head should carry the resume-cadence clause"
        );
    }

    #[test]
    fn stable_head_carries_tool_call_discipline() {
        // The TOOL-CALL DISCIPLINE block (generation-time rules) is baked into
        // the compiled stable head so every project inherits it: content-first
        // drafting, schema-first pre-flight, escape-trap avoidance, chunking,
        // and the malformed-JSON recovery protocol.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("TOOL-CALL DISCIPLINE"),
            "head should carry the tool-call discipline section"
        );
        assert!(
            head.contains("CONTENT FIRST, CALL SECOND"),
            "head should mandate writing content before emitting the call"
        );
        assert!(
            head.contains("never the place where the text gets invented"),
            "head should state that the call body is never where text is invented"
        );
        assert!(
            head.contains("SCHEMA-FIRST PRE-FLIGHT"),
            "head should carry the schema-first field checklist"
        );
        assert!(
            head.contains("create_plan → title, goal, steps"),
            "head should name create_plan's required fields"
        );
        assert!(
            head.contains("forward slashes"),
            "head should warn against JSON escape traps in paths"
        );
        assert!(
            head.contains("MALFORMED-JSON RECOVERY"),
            "head should carry the malformed-JSON recovery protocol"
        );
        assert!(
            head.contains("rewrite the COMPLETE call"),
            "head should mandate rewriting the complete call, not tweaking it"
        );
        assert!(
            head.contains("TWO IDENTICAL FAILURES IN A ROW"),
            "head should carry the two-strikes loop-breaker"
        );
        assert!(
            head.contains("identical call fails identically"),
            "head should state that identical calls fail identically"
        );
    }

    #[test]
    fn executing_prompt_shows_plan() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan(
            "Build feature X",
            "Add login",
            "Found routes",
            vec!["step 1".into(), "step 2".into()],
        )
        .unwrap();
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(prompt.contains("Build feature X"));
        assert!(prompt.contains("CURRENT STEP"));
        assert!(prompt.contains("step 1"));
    }

    #[test]
    fn constitution_included() {
        let dir = tempdir().unwrap();
        let wf = Workflow::new(dir.path());
        let caps = Capabilities::openai();
        let constitution = Constitution {
            global: "Never commit to main".into(),
            project: "Use edition 2021".into(),
        };
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(prompt.contains("Never commit to main"));
        assert!(prompt.contains("Use edition 2021"));
        assert!(prompt.contains("HARD RULES — GLOBAL"));
        assert!(prompt.contains("HARD RULES — PROJECT"));
    }

    #[test]
    fn complete_prompt_mentions_complete() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(prompt.contains("complete"));
    }

    #[test]
    fn complete_prompt_is_directive_about_planning_vs_freeform() {
        // The enhanced STATE_COMPLETE must tell the agent WHEN to plan (a
        // code-change task) vs. when to answer freeform (a pure question) —
        // the "more eager switching to plan" the user asked for. Pins the
        // directive language so a regression that reverts to the terse
        // "Write tools need a new plan" guidance fails here.
        //
        // Uses a Research plan so complete_step auto-transitions to Complete
        // (an Implementation plan would land in Reviewing instead).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan_with_kind(
            "T",
            "G",
            "C",
            vec!["a".into()],
            crate::workflow::PlanKind::Research,
            None,
        )
        .unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Complete);
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(
            prompt.contains("NEW TASK"),
            "STATE_COMPLETE should frame code-change messages as a new task"
        );
        assert!(
            prompt.contains("do NOT answer freeform"),
            "STATE_COMPLETE should forbid freeform answers for code-change tasks"
        );
    }

    #[test]
    fn looks_like_work_intent_fires_on_imperative_verbs() {
        // Imperative work verbs (and their -ing/-ed stems) trigger the
        // heuristic — these are the messages that should nudge toward
        // planning when the agent is resting in Complete.
        assert!(looks_like_work_intent("fix the bug in login.rs"));
        assert!(looks_like_work_intent("add a dark mode toggle"));
        assert!(looks_like_work_intent("implement the auth module"));
        assert!(looks_like_work_intent("refactor the parser"));
        assert!(looks_like_work_intent("Can you update the config?"));
        // Stems: "fixing", "added", "updated" all match their verb roots.
        assert!(looks_like_work_intent("I'm fixing the login flow"));
        assert!(looks_like_work_intent("we added a new endpoint"));
        assert!(looks_like_work_intent("the updated config broke"));
        // Punctuation around the verb is stripped by trim_matches (Low 3):
        // trailing punctuation on a word, parenthesized verbs, and
        // sentence-final punctuation all still match.
        assert!(looks_like_work_intent("fix the bug in login.rs!"));
        assert!(looks_like_work_intent("Please (refactor) the parser."));
        assert!(looks_like_work_intent("Fix, then test."));
    }

    #[test]
    fn looks_like_work_intent_excludes_false_positives_from_prefix_match() {
        // Low 1 regression: a raw starts_with would match "address" ← "add",
        // "wireless" ← "wire", "movement" ← "move", "fixture" ← "fix". The
        // exact-or-suffix match excludes these — the remainder after the verb
        // prefix is not a valid inflection suffix.
        assert!(!looks_like_work_intent("what's the memory address?"));
        assert!(!looks_like_work_intent("address this concern"));
        assert!(!looks_like_work_intent("the wireless connection dropped"));
        assert!(!looks_like_work_intent("the movement was smooth"));
        assert!(!looks_like_work_intent("the fixture is loose"));
        // A hyphenated compound like "fix-the-bug" is a single whitespace
        // token whose stem ("fix-the-bug") doesn't match any verb+suffix — a
        // known false negative that falls back to STATE_COMPLETE.
        assert!(!looks_like_work_intent("fix-the-bug"));
    }

    #[test]
    fn looks_like_work_intent_does_not_fire_on_pure_questions() {
        // Pure questions (how/where/what) and conversational filler must NOT
        // trigger the nudge — the agent should answer these freeform.
        assert!(!looks_like_work_intent("how does the login work?"));
        assert!(!looks_like_work_intent("what is the config format?"));
        assert!(!looks_like_work_intent("where is the main function?"));
        assert!(!looks_like_work_intent(""));
        assert!(!looks_like_work_intent("thanks"));
        assert!(!looks_like_work_intent("great, that worked"));
    }

    #[test]
    fn append_plan_nudge_appends_for_complete_and_work_intent() {
        // Complete + a work-verb message → the nudge is appended.
        let mut tail = String::from("base tail\n");
        append_plan_nudge(
            &mut tail,
            crate::workflow::WorkflowState::Complete,
            Some("fix the bug in login.rs"),
        );
        assert!(tail.contains("PLAN NUDGE"), "nudge header appended");
        assert!(tail.contains("create_plan"), "nudge names create_plan");
        assert!(tail.contains("base tail"), "original tail preserved");
    }

    #[test]
    fn append_plan_nudge_skips_for_complete_and_pure_question() {
        // Complete + a pure question → no nudge (the agent answers freeform).
        let mut tail = String::from("base tail\n");
        append_plan_nudge(
            &mut tail,
            crate::workflow::WorkflowState::Complete,
            Some("how does the login work?"),
        );
        assert!(!tail.contains("PLAN NUDGE"), "no nudge for a pure question");
        assert_eq!(tail, "base tail\n", "tail unchanged");
    }

    #[test]
    fn append_plan_nudge_skips_for_non_complete_state() {
        // Executing (or any non-Complete state) + a work-verb message → no
        // nudge: the agent already has an active plan.
        let mut tail = String::from("base tail\n");
        append_plan_nudge(
            &mut tail,
            crate::workflow::WorkflowState::Executing,
            Some("fix the bug in login.rs"),
        );
        assert!(!tail.contains("PLAN NUDGE"), "no nudge outside Complete");
        assert_eq!(tail, "base tail\n", "tail unchanged");
    }

    #[test]
    fn append_plan_nudge_skips_when_no_user_message() {
        // Complete + no user message (e.g. a tool-result-only turn) → no nudge.
        let mut tail = String::from("base tail\n");
        append_plan_nudge(&mut tail, crate::workflow::WorkflowState::Complete, None);
        assert!(
            !tail.contains("PLAN NUDGE"),
            "no nudge without a user message"
        );
        assert_eq!(tail, "base tail\n", "tail unchanged");
    }

    #[test]
    fn skill_prompt_injects_skill_goal_and_exit() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.start_skill(
            "merge_to_main",
            "Merge this branch into main and delete it.",
            crate::workflow::WorkflowState::Planning,
            vec!["git".into(), "skill_end".into()],
        )
        .unwrap();
        let caps = Capabilities::openai();
        let constitution = Constitution::default();
        let prompt = build_system_prompt(&caps, &wf, &constitution, None);
        assert!(prompt.contains("merge_to_main"), "prompt names the skill");
        assert!(
            prompt.contains("Merge this branch into main and delete it."),
            "prompt includes the skill goal"
        );
        assert!(prompt.contains("skill_end"), "prompt mentions skill_end");
        assert!(
            prompt.contains("abandon_skill"),
            "prompt mentions abandon_skill"
        );
    }

    #[test]
    fn stable_head_carries_tool_strategy() {
        // The stable head must carry concise "when to use" guidance for the
        // tools whose schemas document parameters but not strategy —
        // especially memory_write (the key "learning agent" lever: the model
        // must proactively persist distilled facts, not rely on auto-capture).
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("TOOL STRATEGY"),
            "head should carry the tool-strategy section"
        );
        assert!(
            head.contains("memory_write"),
            "strategy should name memory_write"
        );
        assert!(
            head.contains("memory_write: the moment you learn"),
            "strategy must make memory writes an immediate trigger"
        );
        assert!(
            head.contains("MANDATORY, not suggestions"),
            "strategy must make the first habits mandatory, not a soft nudge"
        );
        assert!(
            head.contains("memory_search"),
            "strategy should name memory_search"
        );
        assert!(
            head.contains("memory_consolidate"),
            "strategy should name memory_consolidate"
        );
        assert!(
            head.contains("spawn_agent"),
            "strategy should name spawn_agent"
        );
        assert!(head.contains("ask_user"), "strategy should name ask_user");
        assert!(
            head.contains("do not narrate or apologize"),
            "strategy must tell the model not to narrate useless auto-recalled memories"
        );
    }

    #[test]
    fn stable_head_carries_memory_records_block() {
        // The typed-record conventions ship in the SAME phase as the hygiene
        // tools (user requirement 2026-08-22: prompt enforcement is its own
        // visible step) — without this block the tools would be dead code.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("MEMORY RECORDS"),
            "head should carry the typed-record conventions"
        );
        // Every typed prefix (the soft guideline replaces the old hard budgets).
        for marker in ["SPEC:", "DECISION:", "BUG:", "PLAN:", "HOW:", "REVIEW:"] {
            assert!(
                head.contains(marker),
                "conventions name the {marker} prefix"
            );
        }
        assert!(
            head.contains("compact and pointer-first"),
            "conventions state the compact + pointer-first guideline"
        );
        // The pointer-first rule + all four hygiene tools.
        assert!(
            head.contains("Pointer-first"),
            "conventions state the pointer-first rule"
        );
        for tool in [
            "memory_supersede",
            "memory_search",
            "memory_update",
            "memory_delete",
            "memory_amend",
        ] {
            assert!(head.contains(tool), "conventions name {tool}");
        }
        assert!(
            head.contains("NEVER leave both live"),
            "supersede habit: never leave both live"
        );
        assert!(
            head.contains("superseded, never deleted"),
            "delete habit: stale state is superseded, not deleted"
        );
        // The file-mutation policy bullet (plan be16ea36 step 8).
        assert!(
            head.contains("File mutation goes through the file tools"),
            "core principles state the file-tools-first policy"
        );
        // The block rides right after TOOL STRATEGY in the stable head.
        let strategy = head.find("TOOL STRATEGY").expect("strategy present");
        let records = head.find("MEMORY RECORDS").expect("records present");
        assert!(
            strategy < records,
            "MEMORY RECORDS follows TOOL STRATEGY in the stable head"
        );
    }

    #[test]
    fn stable_head_carries_phase3_retrieval_habits() {
        // The Phase-3 retrieval tools ship with MANDATORY habits in the SAME
        // phase (user requirement 2026-08-22: prompt enforcement is its own
        // visible step) — without the habits the tools would be dead code.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        // context_pack: MANDATORY before planning non-trivial work.
        assert!(
            head.contains("MANDATORY before planning non-trivial work"),
            "the planning trigger must stay MANDATORY-phrased"
        );
        assert!(
            head.contains("pointer-first bundle"),
            "the planning trigger states the pointer-first contract"
        );
        assert!(
            head.contains("MANDATORY before diagnosing any bug"),
            "the bug trigger must stay MANDATORY-phrased"
        );
        assert!(
            head.contains(r#"record_type:"bug""#),
            "the bug trigger names the scope that replaced past_fixes"
        );
        assert!(
            head.contains("BUG:"),
            "the bug trigger names the BUG: record class"
        );
        // Both triggers live inside TOOL STRATEGY (the memory_supersede habit
        // stays in MEMORY_RECORDS — no duplication).
        let strategy = head.find("TOOL STRATEGY").expect("strategy present");
        let records = head.find("MEMORY RECORDS").expect("records present");
        let pack = head
            .find("MANDATORY before planning non-trivial work")
            .expect("planning trigger present");
        let fixes = head
            .find("MANDATORY before diagnosing any bug")
            .expect("bug trigger present");
        assert!(
            strategy < pack && pack < records && fixes < records,
            "both triggers ride in TOOL STRATEGY, before MEMORY RECORDS"
        );
        // The memory_supersede habit is NOT duplicated in TOOL STRATEGY
        // (it lives once, in MEMORY_RECORDS).
        let supersede_in_strategy = TOOL_STRATEGY.contains("memory_supersede");
        assert!(
            !supersede_in_strategy,
            "memory_supersede habit stays in MEMORY_RECORDS only"
        );
    }

    #[test]
    fn stable_head_carries_bug_fixing_habit() {
        // Phase 4: bugs ALWAYS use kind=bug_fixing, never implementation —
        // the compiled-prompt habit that makes the locked skeleton + BUG:
        // capture reachable.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("kind:\"bug_fixing\""),
            "head names the bug_fixing kind"
        );
        assert!(
            head.contains("never \"implementation\""),
            "head forbids implementation for bugs"
        );
        assert!(
            head.contains("bug:{symptom}"),
            "head names the required bug param"
        );
        assert!(
            head.contains("BUG: memory auto-captured at finish"),
            "head names the finish capture"
        );
    }

    #[test]
    fn stable_head_carries_feature_scale_carve_out() {
        // Backlog 51ee41c1: bug-triggered FEATURES (the fix adds
        // capabilities, new dependencies, or spans multiple modules) are
        // kind=implementation with the bug as motivation — bug_fixing is
        // for CONTAINED defect fixes. Without the carve-out, feature-scale
        // work runs under the locked skeleton with a mega "Minimal fix"
        // step and a BUG: auto-capture that mislabels a feature (cf. plan
        // c87093c1).
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("bug-triggered FEATURES"),
            "head names the feature-scale carve-out"
        );
        assert!(
            head.contains("bug_fixing is for CONTAINED"),
            "head scopes bug_fixing to contained defect fixes"
        );
        assert!(
            head.contains("documented as motivation in goal/context"),
            "head says where the bug goes in an implementation plan"
        );
    }

    #[test]
    fn stable_head_carries_branch_status_default() {
        // Merge hygiene (backlog #87): with no live "unmerged" memory about
        // a feature/bug, the agent assumes it is IN main — the rule that
        // keeps stale branch-only hints from being trusted forever.
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("Branch-status default"),
            "head names the branch-status default rule"
        );
        assert!(
            head.contains("ABSENCE of an unmerged marker means merged"),
            "head states the default assumption"
        );
    }

    #[test]
    fn closing_sequence_mentions_verdict_and_bug_checklist() {
        // The verdict contract + the bug-plan reviewer checklist ship in the
        // closing sequence (the reviewer task text the agent pastes).
        let constitution = Constitution::default();
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("## Verdict: PASS"),
            "closing sequence names the verdict line"
        );
        assert!(
            head.contains("fail closed"),
            "closing sequence states fail-closed"
        );
        assert!(
            head.contains("regression test exercises the changed path"),
            "closing sequence carries the bug-plan reviewer checklist"
        );
        assert!(
            head.contains("BUG: memory written"),
            "closing sequence checks the BUG: memory"
        );
        assert!(
            head.contains("The main agent can NEVER author a review itself"),
            "closing sequence enforces reviewer-only authorship"
        );
        assert!(
            head.contains("abandon_plan is the escape"),
            "closing sequence names the Reviewing escape hatch"
        );
    }

    #[test]
    fn reviewing_state_block_points_at_app_rules_without_duplicating_them() {
        // The Reviewing block sits in the VOLATILE TAIL — re-processed every
        // turn, after the cached head. So it points at the closing sequence
        // and drives the fix loop; the verdict rule and the bug checklist are
        // stated once, in the head's APP RULES.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        let caps = Capabilities::openai();
        let tail = build_volatile_tail(&caps, &wf, None);
        assert!(
            tail.contains("closing sequence (APP RULES)"),
            "Reviewing block points at the head's closing sequence"
        );
        assert!(
            tail.contains("role:\"reviewer\"") && tail.contains("fix EVERY"),
            "Reviewing block still names the reviewer and the fix loop"
        );
        assert!(
            !tail.contains("unparseable = FINDINGS"),
            "verdict rule is not duplicated into the volatile tail"
        );
        assert!(
            !tail.contains("regression test exercises the changed path"),
            "bug checklist is not duplicated into the volatile tail"
        );

        // The detail still exists — once — in the cached stable head.
        let head = build_stable_head(&Constitution::default());
        assert!(
            head.contains("unparseable = FINDINGS")
                && head.contains("regression test exercises the changed path")
                && head.contains("The main agent can NEVER author a review itself")
                && head.contains("abandon_plan is the escape"),
            "the head carries the full closing-sequence detail"
        );
    }

    #[test]
    fn tool_strategy_lists_graph_tools_before_search() {
        // Order matters: the graph_* line must come BEFORE the search line so
        // "try graph first" reads as the primary instruction, not an
        // afterthought tacked onto the established grep habit. Asserted on
        // the const directly (same-file access) so a constitution string can
        // never shift the comparison.
        let graph = TOOL_STRATEGY
            .find("graph_search / graph_context")
            .expect("graph line present");
        let search = TOOL_STRATEGY
            .find("search / search_read")
            .expect("search line present");
        assert!(
            graph < search,
            "graph tools must be listed before search in TOOL STRATEGY"
        );
        // The first-choice framing is scoped to SYMBOL questions, and the
        // string-literal carve-out rides on the graph line itself so the
        // headline can never over-apply graph-first to text lookups.
        assert!(
            TOOL_STRATEGY.contains("MANDATORY first step"),
            "graph line carries the mandatory first-choice framing"
        );
        assert!(
            TOOL_STRATEGY.contains("for SYMBOL questions"),
            "graph-first framing is scoped to symbol questions"
        );
        // The cost model makes the ordering rationale explicit: indexed
        // lookups beat tree walks beat process spawns.
        assert!(
            TOOL_STRATEGY.contains("Cost model"),
            "strategy states the cost model behind the tool ordering"
        );
        assert!(
            TOOL_STRATEGY.contains("String literals"),
            "graph line states what is NOT indexed"
        );
        // Language coverage is stated explicitly (user request 2026-08-27:
        // the agent greps + whole-file-reads frontend symbols because nothing
        // told it the graph indexes .ts/.tsx too; broadened 2027-01-11 to
        // the full indexed-language set).
        assert!(
            TOOL_STRATEGY.contains("JavaScript, Python"),
            "graph line states the multi-language coverage explicitly"
        );
        assert!(
            TOOL_STRATEGY.contains("FALLBACK when the graph"),
            "search line is framed as the fallback"
        );
        assert!(
            TOOL_STRATEGY.contains("switch to graph_context"),
            "search line carries the search-to-graph chaining rule"
        );
    }

    #[test]
    fn stable_head_excludes_volatile_sections() {
        // The stable head (preamble + constitution) must NOT contain the
        // volatile workflow/memories sections — those move to the tail so the
        // provider's prompt-cache prefix survives complete_step bumps + new
        // turns.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("Build X", "Goal", "Ctx", vec!["step 1".into()])
            .unwrap();
        let caps = Capabilities::openai();
        let constitution = Constitution {
            global: "Never commit to main".into(),
            project: "Use edition 2021".into(),
        };
        let head = build_stable_head(&constitution);
        assert!(
            head.contains("plan-first workflow"),
            "head has the preamble"
        );
        assert!(
            head.contains("HARD RULES — GLOBAL"),
            "head has global constitution"
        );
        assert!(head.contains("Never commit to main"));
        assert!(
            head.contains("HARD RULES — PROJECT"),
            "head has project constitution"
        );
        assert!(head.contains("Use edition 2021"));
        // Volatile sections must be absent.
        assert!(
            !head.contains("WORKFLOW STATE"),
            "head excludes workflow state"
        );
        assert!(
            !head.contains("RECALLED MEMORIES"),
            "head excludes memories"
        );
        assert!(!head.contains("CURRENT STEP"), "head excludes current step");
        assert!(!head.contains("Build X"), "head excludes plan title");
        // `caps` + `wf` are only used by the tail; reference them so the
        // compiler doesn't warn about unused vars in this assertion-only test.
        let _ = build_volatile_tail(&caps, &wf, None);
    }

    #[test]
    fn stable_head_byte_stable_across_workflow_changes() {
        // The head ignores the workflow entirely, so it must be byte-identical
        // across Planning / Executing (with a plan) / Complete.
        let dir = tempdir().unwrap();
        let constitution = Constitution {
            global: "Global rule".into(),
            project: "Project rule".into(),
        };

        let head_planning = {
            let _wf = Workflow::new(dir.path());
            build_stable_head(&constitution)
        };

        let head_executing = {
            let mut wf = Workflow::new(dir.path());
            wf.create_plan("T", "G", "C", vec!["a".into(), "b".into()])
                .unwrap();
            build_stable_head(&constitution)
        };

        let head_complete = {
            let mut wf = Workflow::new(dir.path());
            wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
            wf.complete_step(0).unwrap();
            build_stable_head(&constitution)
        };

        assert_eq!(
            head_planning, head_executing,
            "head stable Planning→Executing"
        );
        assert_eq!(
            head_executing, head_complete,
            "head stable Executing→Complete"
        );
    }

    #[test]
    fn volatile_tail_includes_workflow_and_memories() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("Build X", "Goal", "Ctx", vec!["step 1".into()])
            .unwrap();
        let caps = Capabilities::openai();

        // With memories: tail has both workflow state + memories.
        let tail_with_mem = build_volatile_tail(&caps, &wf, Some("memory text"));
        assert!(
            tail_with_mem.contains("WORKFLOW STATE"),
            "tail has workflow state"
        );
        assert!(
            tail_with_mem.contains("RECALLED MEMORIES"),
            "tail has memories header"
        );
        assert!(
            tail_with_mem.contains("memory text"),
            "tail has memory content"
        );
        assert!(
            tail_with_mem.contains("CURRENT STEP"),
            "tail has current step"
        );

        // Without memories: tail has workflow state but no memories block.
        let tail_no_mem = build_volatile_tail(&caps, &wf, None);
        assert!(
            tail_no_mem.contains("WORKFLOW STATE"),
            "tail still has workflow state"
        );
        assert!(
            !tail_no_mem.contains("RECALLED MEMORIES"),
            "tail omits memories when None"
        );
    }

    #[test]
    fn workflow_section_current_step_prefers_header_and_truncates() {
        // A step with a bold header renders the punchline only — the full
        // body stays in the conversation, and keeping the CURRENT STEP line
        // short cuts the per-complete_step reprocess cost (the tail is the
        // only part that changes once the R5 footer is in place).
        let dir = tempdir().unwrap();
        let long_body = "do the very long thing that takes hundreds of tokens to describe \
                         in detail with all the sub-steps and caveats and edge cases";
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec![format!("**Header A** — {long_body}")])
            .unwrap();
        let caps = Capabilities::openai();
        let tail = build_volatile_tail(&caps, &wf, None);
        assert!(
            tail.contains("CURRENT STEP: \"Header A\""),
            "tail shows the header"
        );
        assert!(
            !tail.contains("hundreds of tokens"),
            "tail must not contain the long body"
        );

        // A headerless step longer than 120 chars is truncated with an
        // ellipsis (char-safe), and its tail end is not rendered.
        let mut wf2 = Workflow::new(dir.path().join("plans2"));
        let plain = "plain step with no bold header whose body is very long indeed and keeps \
                     going well past the one hundred and twenty character cutoff point";
        assert!(plain.chars().count() > 120);
        wf2.create_plan("P2", "G", "C", vec![plain.to_string()])
            .unwrap();
        let tail2 = build_volatile_tail(&caps, &wf2, None);
        let head_120: String = plain.chars().take(120).collect();
        assert!(
            tail2.contains(&format!("CURRENT STEP: \"{head_120}…\"")),
            "tail shows the truncated text"
        );
        assert!(
            !tail2.contains("character cutoff point"),
            "tail must not contain text past the 120-char cap"
        );
    }

    #[test]
    fn workflow_section_surfaces_bug_and_context_for_resume() {
        // Backlog 77ff8f45: a fresh session picking up an in-flight plan
        // gets the Bug + Context in the volatile tail — its only
        // plan-derived injection. The full detail (including a bug plan's
        // Detailed steps) stays in the plan file; the tail lines are
        // one-line, budget-truncated (paid per step bump).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind_and_detail(
            "P",
            "G",
            "the root cause lives in src/app.rs:42",
            vec!["s1".into()],
            crate::workflow::PlanKind::BugFixing,
            Some("the app crashes on open"),
            Some("- Reproduce — run cargo test crashy"),
        )
        .unwrap();
        let caps = Capabilities::openai();
        let tail = build_volatile_tail(&caps, &wf, None);
        assert!(
            tail.contains("BUG: the app crashes on open"),
            "tail surfaces the bug symptom: {tail}"
        );
        assert!(
            tail.contains("CONTEXT: the root cause lives in src/app.rs:42"),
            "tail surfaces the context gist: {tail}"
        );
        // The detailed steps stay in the plan FILE (too big for the tail).
        assert!(
            !tail.contains("cargo test crashy"),
            "the detailed steps must not bloat the tail"
        );

        // A long context is truncated to the budget (one line, ellipsis).
        let mut wf2 = Workflow::new(dir.path().join("plans2"));
        let long_context = "c ".repeat(300);
        wf2.create_plan("P2", "G", &long_context, vec!["s".into()])
            .unwrap();
        let tail2 = build_volatile_tail(&caps, &wf2, None);
        let head_240: String = long_context.trim().chars().take(240).collect();
        assert!(
            tail2.contains(&format!("CONTEXT: {head_240}…")),
            "tail truncates the context to the 240-char budget"
        );

        // Simulated resume: a FRESH workflow loads the plan from disk and
        // the tail still surfaces the bug + context (the parse preserved
        // them) — the crash-resumption read path.
        let mut resumed = Workflow::new(dir.path().join("plans"));
        resumed.load_latest().unwrap();
        let tail3 = build_volatile_tail(&caps, &resumed, None);
        assert!(
            tail3.contains("BUG: the app crashes on open"),
            "the resumed tail surfaces the bug: {tail3}"
        );
        assert!(
            tail3.contains("CONTEXT: the root cause lives in src/app.rs:42"),
            "the resumed tail surfaces the context: {tail3}"
        );
    }

    #[test]
    fn workflow_section_subagent_shows_read_only_parent_plan() {
        // A parented sub-agent's workflow: the plan stack is the read-only
        // mirror of the main plan (loaded from the shared dir by build_inner),
        // and the spawn path stamps WorkflowState::Subagent over whatever
        // lifecycle state load_latest derived. The section must show the
        // parent plan as orientation — never CURRENT STEP (the sub-agent is
        // not executing it) — plus the sub-agent guidance.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["s".into()]).unwrap();
        wf.enter_subagent_state();
        let caps = Capabilities::openai();
        let tail = build_volatile_tail(&caps, &wf, None);
        assert!(
            tail.contains("Current state: Subagent"),
            "tail reports the Subagent role state"
        );
        assert!(
            tail.contains("PARENT PLAN (read-only context): P"),
            "tail shows the mirrored parent plan as read-only context"
        );
        assert!(tail.contains("GOAL: G"), "tail shows the parent goal");
        assert!(
            tail.contains("PROGRESS: 0/1 steps complete"),
            "tail shows the parent progress"
        );
        assert!(
            tail.contains("single-task worker"),
            "tail carries the sub-agent guidance"
        );
        assert!(
            !tail.contains("CURRENT STEP"),
            "a sub-agent is not executing the plan — no CURRENT STEP line"
        );

        // No plan on the stack (sub-agent spawned while the main agent had
        // none): the guidance still renders, without any plan lines.
        let mut wf2 = Workflow::new(dir.path().join("plans2"));
        wf2.enter_subagent_state();
        let tail2 = build_volatile_tail(&caps, &wf2, None);
        assert!(
            tail2.contains("Current state: Subagent"),
            "state line renders without a plan"
        );
        assert!(
            !tail2.contains("PARENT PLAN"),
            "no plan lines without a mirrored stack"
        );
        assert!(
            tail2.contains("single-task worker"),
            "guidance renders without a plan"
        );
    }

    #[test]
    fn recall_context_caps_memory_length() {
        // Memory content is capped at 160 chars per entry — the gist is
        // preserved, the volatile tail stays small (it is the only part
        // re-processed per turn once the R5 footer is in place).
        let long_content = format!(
            "{}UNIQUE-TAIL-MARKER",
            "alpha ".repeat(50) // 300 chars
        );
        assert!(long_content.chars().count() > 160);
        let head_160: String = long_content.chars().take(160).collect();
        let memory = crate::memory::Memory::new(
            crate::memory::MemoryTier::Semantic,
            "some title",
            long_content,
            0,
        );
        let scored = crate::memory::ScoredMemory {
            memory,
            score: 0.75,
        };
        let memory_id = scored.memory.id.clone();
        let out = format_recall_context(std::slice::from_ref(&scored));
        assert!(out.contains(&head_160), "first 160 chars are kept");
        assert!(
            !out.contains("UNIQUE-TAIL-MARKER"),
            "no content past char 160"
        );
        // Stable ids (Phase 1): the id is the FIRST parenthesized field, so
        // existing score regexes still match — and the agent can
        // supersede/update straight from recalled context.
        assert!(
            out.contains(&format!(
                "[semantic] some title (id: {memory_id}, score: 0.75)"
            )),
            "header line carries the stable id first: {out}"
        );
    }

    #[test]
    fn format_primer_caps_and_formats() {
        // The primer formats each memory as `[tier] title (id: <id>) —
        // {content first 200 chars}` under a standing-context header, and
        // returns empty for no memories. Content past 200 chars is truncated
        // (char-safe). The stable id (Phase 1) lets the agent supersede a
        // memory straight from the standing context.
        let empty = format_primer(&[]);
        assert!(empty.is_empty(), "no memories → empty primer");

        let long_content = format!("{}UNIQUE-TAIL-MARKER", "beta ".repeat(60)); // 300 chars
        assert!(long_content.chars().count() > 200);
        let head_200: String = long_content.chars().take(200).collect();
        let memories = vec![
            crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "auth fact",
                "this project uses jose for JWT",
                0,
            ),
            crate::memory::Memory::new(
                crate::memory::MemoryTier::Procedural,
                "long entry",
                long_content,
                0,
            ),
        ];
        let auth_id = memories[0].id.clone();
        let out = format_primer(&memories);
        assert!(
            out.contains("# PROJECT MEMORY (standing context)"),
            "has header"
        );
        assert!(out.contains(&format!(
            "[semantic] auth fact (id: {auth_id}) — this project uses jose for JWT"
        )));
        assert!(
            out.contains(&head_200),
            "first 200 chars of long content kept"
        );
        assert!(
            !out.contains("UNIQUE-TAIL-MARKER"),
            "no content past char 200"
        );
    }

    #[test]
    fn context_footer_is_stable_literal() {
        // The footer is the byte-stable FINAL message (appended by turn.rs
        // after the volatile tail). It must be a fixed literal with no
        // volatile content, and must not already appear inside the head or
        // tail (so it is added only by turn.rs as a separate message).
        assert!(!CONTEXT_FOOTER.is_empty(), "footer is non-empty");
        assert!(
            !CONTEXT_FOOTER.contains("WORKFLOW STATE"),
            "footer must not contain the workflow marker"
        );
        assert!(
            !CONTEXT_FOOTER.contains("RECALLED MEMORIES"),
            "footer must not contain the memories marker"
        );
        assert!(
            CONTEXT_FOOTER.len() <= 60,
            "footer stays short (keeps reprocessing cost tiny)"
        );

        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("Build X", "Goal", "Ctx", vec!["step 1".into()])
            .unwrap();
        let caps = Capabilities::openai();
        let constitution = Constitution {
            global: "Never commit to main".into(),
            project: "Use edition 2021".into(),
        };
        let head = build_stable_head(&constitution);
        let tail = build_volatile_tail(&caps, &wf, None);
        assert!(
            !head.contains(CONTEXT_FOOTER),
            "head does not contain the footer (turn.rs adds it separately)"
        );
        assert!(
            !tail.contains(CONTEXT_FOOTER),
            "tail does not contain the footer (turn.rs adds it separately)"
        );
        // Two calls produce identical bytes — the literal is byte-stable.
        assert_eq!(CONTEXT_FOOTER, CONTEXT_FOOTER);
    }
}

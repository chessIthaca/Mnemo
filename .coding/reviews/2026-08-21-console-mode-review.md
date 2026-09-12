# Console mode review — `feat/console-mode` (2026-08-21)

**Scope reviewed:** `src-tauri/src/console.rs` (new, full read), `src-tauri/src/main.rs`,
`src-tauri/src/ipc/agent.rs`, `src-tauri/src/ipc/config_io.rs` (full diff), plus the lib
surfaces they depend on (`src/runtime/mod.rs`, `src/runtime/channels.rs`,
`src/agent/dispatch.rs`, `src/agent/approval.rs`, `src/agent/factory.rs`,
`src/config/mod.rs`, `src/config/endpoints.rs`, `src/provider/trace.rs`,
`src-tauri/src/ipc/spawn.rs`, `src-tauri/src/ipc/events.rs`, `frontend/.../StatusBar.tsx`,
`.gitattributes`). `git diff HEAD` shows no other changes besides `.coding/` bookkeeping.

**Verdict:** The extraction refactor (`config_io`) is clean and behavior-identical; the
pure console layer is well-tested. However, the console REPL reuses the shared
agent-manager plumbing **without replicating the bookkeeping the GUI's event forwarder
performs**, which silently disables the "no workflow transitions while subagents run"
gate, can cancel running background agents, and breaks the parent completion
notification loop. These are real divergences from the plan's own claim that a
console-spawned agent is "indistinguishable from a GUI one". One high, two medium, and
three low findings follow.

---

## HIGH

### H1 — Console mode never maintains agent `running` flags → workflow transition gate bypassed + running subagents cancelled mid-flight

The GUI's event forwarder is the *only* writer of `AgentHandle.running`:
`set_running(agent_id, true)` on `Started`, `false` on `Finished` and on final
non-retrying `Error` (`src-tauri/src/ipc/events.rs:281, 289, 323`). The console REPL
has no forwarder and `handle_event` (`src-tauri/src/console.rs:909-961`) never touches
the manager's running flags — `ReplState.running` is an unrelated prompt-suppression
flag.

`ConsoleSpawner::has_running_descendants` (`console.rs:572-575`) delegates to
`AgentManager::has_running_descendants` (`src/runtime/mod.rs:140-144`), which filters
on `h.is_running()` (`src/runtime/channels.rs:635`). Since nothing in console mode ever
sets that flag, **`has_running_descendants` always returns `false`**. Two concrete
failures follow:

1. **Workflow state-transition gate permanently open.** `src/agent/dispatch.rs:145-168`
   blocks `create_plan`/`update_plan`/`complete_step`/`abandon_plan`/`skill_*`/`finish`
   while spawned subagents run, via the `DescendantTracker` (the `ConsoleSpawner`).
   In console mode the gate never fires, so the main agent can `finish()` (and do any
   other transition) while a spawned reviewer/background agent is still mid-turn — the
   exact race the GUI refuses with "cannot change workflow state while spawned
   subagents are still running".
2. **`cleanup_inactive_subagents` cancels RUNNING background agents.**
   `spawn_agent_shared` calls `cleanup_inactive_subagents` on every spawn
   (`src-tauri/src/ipc/spawn.rs:108`); its filter is
   `h.parent_id.is_some() && !h.is_running()` (`spawn.rs:589-593`). With every flag
   permanently `false`, every previously spawned subagent looks inactive — so when the
   main agent spawns a **second** background agent while the first is still running,
   the first receives a `Cancel` mid-flight. The GUI leaves running subagents alone
   (`events.rs:215-217`). This can silently kill an in-progress reviewer and lose its
   work.

This directly contradicts the documented design claim at `console.rs:495-501`
("the only difference is that no `PromptDispatched` event is emitted").

**Fix direction:** give the console a forwarder-equivalent for lifecycle bookkeeping.
In `handle_event` (all agent ids, not just main): on `Started` →
`manager.lock().await; mgr.set_running(id, true)`; on `Finished` and on
`Error { retrying: false }` → `set_running(id, false)`; on `Exited` → `set_running`
false + `mgr.remove(id)` + `agent_loops.remove(&id)` (see H3). Then H1(1) and H1(2)
recover GUI semantics automatically. Add regression tests (e.g. spawn child → assert
`has_running_descendants` true after its `Started`; assert a second spawn does not
cancel the running child).

---

## MEDIUM

### M1 — Parent completion notification loop missing: reviewers' reports never combined into the parent's plan

In the GUI, when a tool-spawned background agent finishes (cleanly or with a final
error), the forwarder sends a completion `Suggestion` to the **parent** agent
("\[background agent X finished — read its report ... and combine the results into a
plan\]", including the review-report path) and synthesizes a `ChildFinished` event
(`src-tauri/src/ipc/events.rs:241-259` calling `notify_parent_on_completion`,
`events.rs:463-514`). This is the feedback loop an orchestrating agent uses to learn
its spawned agents are done.

Console mode has neither: `handle_event` (`console.rs:909-961`) renders the child's
`Finished`/`Error` but never notifies the parent. A spawned reviewer's report may
therefore never be read/combined by the parent agent unless it coincidentally looks.
Consequently `render_event`'s `ChildFinished` arm (`console.rs:325-332`) is dead code
in console mode — the event is forwarder-synthesized, and the console has no
synthesizer.

**Fix direction:** replicate the manager-side of `notify_parent_on_completion` in
`handle_event` for child `Finished`/final `Error`: read `mgr.parent_id(id)` (and the
child's `last_review_report()` from `agent_loops`), then
`mgr.send(parent, AgentCommand::Suggestion(text))` using
`completion_suggestion_text` (make it `pub(crate)` or mirror its text). Only the
`ChildFinished` *UI emit* can be skipped. Keep a notified-children dedup set so a
multi-turn child notifies once (mirrors `events.rs:187, 245`).

### M2 — Dead agents never removed from the manager / loop map in console mode (unbounded growth)

The GUI forwarder removes an agent from the manager and from `agent_loops` on `Exited`
(`events.rs:366-376`). The console only sets `state.main_exited` for the main agent
(`console.rs:956-958`) and never calls `mgr.remove` / `agent_loops.remove`. Over a long
REPL session with many spawns, the manager map and the loop map grow without bound,
dead loops keep receiving provider swaps and `set_resolved_model(None)`
(`swap_provider_into_loops` iterates every value), and `has_running_descendants`/
`cleanup_inactive_subagents` scans keep walking stale entries. There is also no
Complete→Executing cleanup of stale finished subagents (forwarder-only,
`events.rs:218-233`), so subagents from a previous plan linger until the process ends.

**Fix direction:** as in H1, handle `Exited` for every id by removing it from both maps
(and the notified-children set from M1). Optionally replicate the
Complete→Executing inactive-subagent cleanup for the main agent.

---

## LOW

### L1 — `/model` swap resets reasoning effort to the endpoint default; the GUI picker keeps the current override

The console comment claims "selecting a model resets the effort to the endpoint's
default, exactly like the status-bar picker" (`console.rs:1016-1017`) and passes
`effort = None` (`console.rs:1020-1028`), which `resolve_reasoning_effort` maps to the
endpoint's configured value (`"max"` default). That is **not** what the picker does:
for a supported→supported endpoint switch the GUI keeps the current toolbar effort
(`frontend/src/components/layout/StatusBar.tsx:323-333` — `effort = reasoningEffort`;
it only forces `"off"`/the endpoint default when crossing to/from an unsupported
endpoint). So in the console, `/reasoning high` followed by
`/model openai/o4` silently drops the user's `high` override back to `max`, while the
GUI would preserve it. This is a drift between the two surfaces the shared-path design
was supposed to prevent.

**Fix direction:** track the *requested* effort in `ModelSelection` (add a field) and
pass the current requested value through on model swaps (map `None`→`Some("off")` for a
supported endpoint, mirroring the dropdown), or accept the divergence and fix the
comment/plan doc — but as written the comment is factually wrong.

### L2 — `initial_selection` does not mirror `build_brain`'s startup provider construction

`build_brain` resolves the startup endpoint via `default_provider` → `default_endpoint()`
(named, else **first** endpoint; `src-tauri/src/main.rs:799-802`,
`src/config/mod.rs:106-110`) and the model via `default_model` → endpoint's **first
model** → `"gpt-4o"` (`main.rs:804-810`) — it always builds a provider.
`initial_selection` (`console.rs:481-493`) instead requires `default_model` AND an
endpoint whose `models` list contains it; otherwise it returns `None`. Consequences:

- With no `default_model` configured (a common fresh config), a live provider runs but
  `status()` is `None`: `/reasoning` refuses ("no model selected — use /model first",
  `console.rs:1037, 1044-1047`) and `/model` shows no "current" marker.
- If `default_model` is listed at an endpoint other than the startup endpoint (the
  implicit-default case), the reported endpoint/effort/supports is wrong, and
  `/reasoning <v>` would rebuild the provider from that **other** endpoint —
  silently changing which endpoint serves the live agent.

**Fix direction:** mirror the exact chain — endpoint = `default_provider` or
`default_endpoint()`; model = `default_model` or that endpoint's first model (or
`"gpt-4o"`); effort = `ep.effective_reasoning_effort()`. Return `None` only when
`build_brain` also would have fallen to the dummy provider (no endpoint at all).

### L3 — Busy-spin (one core at 100%) during the exit drain after stdin EOF

Once the stdin reader closes the channel, `input_rx.recv()` in the `select!` loop is
**always ready** and returns `None` forever (`console.rs:1144-1159`). The `None` arm
does nothing when `state.exiting` is already true, so the loop re-enters `select!` and
re-wins the input branch every iteration — a tight spin until `main_exited` arrives or
the 5-second `exit_deadline` check at the top of the loop (`console.rs:1129-1136`)
breaks. It terminates correctly (no hang) but pegs a CPU core for the whole drain
window whenever the agent is slow to exit, contending with the very agent tasks being
drained.

**Fix direction:** add `stdin_closed: bool` to `ReplState`; set it in the `None` arm
and gate the input branch with `if !state.stdin_closed` (or `.fuse()` the receiver and
skip polling once exhausted).

---

## NITS (no action strictly required, but cheap)

- **N1 — misleading doc comment:** `summarize_args` says `{"a":1,"b":"x"}` "with no
  spaces", but the implementation joins with `", "` (`console.rs:206-220`). Fix the
  doc or the join.
- **N2 — missing doc comments on trait impl methods:** `AgentSpawner::spawn` and
  `parent_aware` on `ConsoleSpawner` (`console.rs:524-537`) have no doc comments while
  `IpcSpawner`'s equivalents do (`spawn.rs:401-417`). The constitution requires doc
  comments on all public functions; these are public via the trait.
- **N3 — double `> ` prompt on failed turns:** a final `Error { retrying: false }`
  prints the prompt (`console.rs:949-955`), and the bookkeeping `Finished` that follows
  it (the agent emits both — see the latch comment at `events.rs:656-659`) prints it
  again (`console.rs:943-948`). Guard with `if !state.running`-style check.
- **N4 — `/exit` unreachable while an interaction is pending:** `handle_input` gives
  pending approval/question answers precedence (`console.rs:967-996`), so any typed
  line — including `/exit`, `/model`, `/reasoning` — is consumed as an answer attempt
  and re-prompts. The only escape during a pending approval is Ctrl+Z/EOF (which does
  work, `console.rs:1144-1159`). Sequential resolution is by design, but the help text
  advertises `/exit` unconditionally.
- **N5 — informational:** release binaries are GUI-subsystem
  (`src-tauri/src/main.rs:6`), so `cmd.exe` does not wait for the process and the shell
  prompt may interleave with REPL output when launched from `cmd` (PowerShell with
  `Start-Process -Wait`, or a future console-subsystem build, avoid this). Worth a line
  in the final user-facing notes; not a code bug.

---

## Verified correct (no action needed)

- **`set_model` refactor is behavior-identical.** Validation order (endpoint → model
  allowlist incl. implicit global `default_model` → effort mapping → build), error
  strings, `"off"`→`None` and `!supports_reasoning_effort`→`None` mapping, trace wiring,
  `eprintln!("switched model to ...")`, and per-agent `ModelChanged` emission are all
  preserved (`agent.rs:429-458` vs. pre-refactor block; `config_io.rs:98-141, 183-210`).
- **Locks:** no await under any held lock in the console runtime ops —
  `send_prompt`/`request_shutdown` use `try_send` under the manager lock
  (`src/runtime/mod.rs:58-68`), `swap_model` holds the config lock only across the
  synchronous resolver + std-mutex selection update (`console.rs:755-778`). Lock
  ordering is consistent (config → selection; manager → nothing).
- **Exit handshake is bounded and fail-closed.** A dropped approval responder →
  `ApprovalOutcome::ChannelClosed` → the tool is **not** executed
  (`src/agent/approval.rs:169-179`; `src/agent/dispatch.rs:277-282`); a dropped
  question responder → error result the model can react to (`dispatch.rs:407-421`).
  `await_approval`/`ask_user` also select on `cmd_rx`, so the `/exit` `Cancel` is
  honored even mid-approval (`approval.rs:183-196`). The 5s drain deadline + 10s
  browser-close bound prevent hangs; the deadline branch self-terminates.
- **`swap_model` is live-only** (no config persist), keeps the tracked `ModelSelection`
  in sync before releasing the config lock, and the `supports` flag is read from the
  same config snapshot (`console.rs:749-790`).
- **Security:** no key resolution or key printing anywhere in `console.rs`; key
  resolution stays in the lib (`build_openai_client`); rendered output only ever sees
  tool names/args/results and model ids. `AttachConsole` FFI is correct:
  `ATTACH_PARENT_PROCESS = usize::MAX` ((DWORD)-1), `extern "system"`, `link kernel32`,
  failure benign, called before any print and before any env mutation
  (`main.rs:56-66`, `console.rs:1196-1206`). Flag check precedes the WEBVIEW2 env var
  and the Tauri builder (`main.rs:52-68`). No new dependencies (Cargo files untouched).
- **Spawn parity:** the console main-agent spawn args exactly match the GUI's
  (`main.rs:256-268` vs `console.rs:635-647`), and `ConsoleSpawner::spawn_with_parent`
  matches `IpcSpawner`'s except for the UI-only `PromptDispatched` emit
  (`spawn.rs:429-456` vs `console.rs:544-565`).
- **Constitution:** no `#[allow(...)]` anywhere in the diff; all `pub(crate)` free
  functions/structs carry doc comments (except N2); `#![deny(warnings)]` was green in
  the pre-review `cargo test` (120 passed). New pure functions and the extraction seams
  have regression tests (9 in `config_io.rs`, ~25 in `console.rs`); the H1/M1/M2
  lifecycle gaps have none yet — the fixes should add them.
- **Line endings:** `.gitattributes` pins `eol=lf`; the CRLF warnings on the modified
  files are the machine-global autocrlf normalization, not a mixed-ending regression.

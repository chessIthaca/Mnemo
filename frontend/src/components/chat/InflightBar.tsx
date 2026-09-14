// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useRef, useEffect } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import type { AgentState, ActivityEntry } from "../../hooks/useAgentStore";
import { useAgentStore, RECENT_TIMING_WINDOW, recentOutputTokPerSec } from "../../hooks/useAgentStore";
import { fmtTokens, fmtRate, fmtDuration, fmtPct } from "../../lib/format";
import { readInflightPanelOpen, writeInflightPanelOpen } from "../../lib/inflightPanelPref";
import { compact } from "../../lib/tauri";
import type { TurnPhase } from "../../lib/types";

/** Props for the inner panel — the agent's state slice + id. */
interface InflightBarProps {
  state: AgentState;
  agentId: number | null;
}

/** Default panel height — ~5 lines of monospace text. */
const DEFAULT_HEIGHT = 120;
const MIN_HEIGHT = 40;
const MAX_HEIGHT = 600;

/**
 * The inflight bar's live status label for a turn phase, with the
 * approval/question pauses overlaid: while the backend sits in
 * "running tools" awaiting the user, the label shows what it is really
 * waiting FOR.
 */
function phaseLabel(
  phase: TurnPhase,
  waitingApproval: boolean,
  waitingQuestion: boolean,
): string {
  if (waitingApproval) return "waiting for approval…";
  if (waitingQuestion) return "waiting for your answer…";
  switch (phase) {
    case "sending":
      return "sending…";
    case "compacting":
      return "compacting context…";
    case "waiting":
      return "waiting…";
    case "reasoning":
      return "reasoning…";
    case "streaming":
      return "answering…";
    case "running_tools":
      return "running tools…";
    case "idle":
      return "idle";
  }
}

/** Amber when the agent waits on the USER (approval/question), cyan otherwise. */
function phaseLabelClass(waitingApproval: boolean, waitingQuestion: boolean): string {
  return waitingApproval || waitingQuestion ? "text-amber-400" : "text-cyan-400";
}

/**
 * Self-subscribing mount for the inflight bar (mem-perf review HIGH 1):
 * pulls the agent's state slice from the store itself, so ONLY this bar
 * re-renders per streaming frame — App no longer subscribes to the whole
 * agent object (whose identity changes on every rAF streaming flush),
 * which used to re-render the entire app shell per frame. Renders
 * nothing while the agent doesn't exist (null id or not registered).
 */
export function InflightBar({ agentId }: { agentId: number | null }) {
  const state = useAgentStore((s) => (agentId != null ? s.agents[agentId] : undefined));
  if (!state) return null;
  return <InflightBarPanel state={state} agentId={agentId} />;
}

/** The bar itself — receives the agent's state slice (guaranteed defined). */
function InflightBarPanel({ state, agentId }: InflightBarProps) {
  const { running, activityLog, tokenUsage, lastRequestTiming, sessionTiming, lastContextBreakdown, contextUsage, contextBreakdown } = state;
  const {
    phase,
    turnStartedAt,
    liveCompletionTokens,
    liveReasoningTokens,
    pendingApproval,
    pendingQuestion,
  } = state;
  const showTokenUsage = useAgentStore((s) => s.showTokenUsage);

  // Whether the panel is open is the USER's choice ALONE (user report
  // 2027-01-11): nothing in the app opens it. The effect that used to sit
  // here auto-expanded the panel on every reasoning-block edge, so each new
  // turn re-opened a panel the user had deliberately collapsed. The choice
  // is remembered across restarts via localStorage and defaults to CLOSED.
  const [expanded, setExpanded] = useState(readInflightPanelOpen);
  const [height, setHeight] = useState(DEFAULT_HEIGHT);
  const panelRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  /**
   * Apply + persist the user's open/closed choice — the ONLY writer of
   * `expanded` (the chevron click and the drag handle both route through
   * here). No other code path may expand the panel on its own.
   */
  function setPanel(open: boolean) {
    setExpanded(open);
    writeInflightPanelOpen(open);
  }

  // Live elapsed timer: tick once a second while a turn is running so the
  // "1m 3s" readout advances. `turnStartedAt` is set by the started event
  // (idle→running only — provider retries keep the original time).
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [running]);
  const elapsedMs = running && turnStartedAt !== null ? now - turnStartedAt : null;

  // Auto-scroll the reasoning panel to the bottom as new content streams in.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [activityLog]);

  // Token counters render whenever the user has the usage display enabled —
  // they sit at 0 from startup rather than appearing only after the first
  // Usage event. The 🧠 reasoning and tok/s sub-segments still gate
  // on their own data below.
  const showCounts = showTokenUsage;

  const hasContext = contextUsage.max > 0;

  // Output tok/sec over the rolling last-3-requests window (persists across
  // turns): tracks the model's CURRENT speed instead of drifting toward the
  // session-wide average — after an endpoint switch or a slow patch the
  // display follows within 3 requests. Total-over-total via
  // recentOutputTokPerSec (NOT average-of-rates — see aggregateTokPerSec).
  // avgTtftMs/avgGenMs are kept for the tooltip only.
  const avgTtftMs =
    sessionTiming.timed_requests > 0
      ? sessionTiming.ttft_ms_total / sessionTiming.timed_requests
      : null;
  const avgGenMs =
    sessionTiming.timed_requests > 0
      ? sessionTiming.generation_ms_total / sessionTiming.timed_requests
      : null;
  const outputRate = recentOutputTokPerSec(sessionTiming);

  // Token counts render their current values directly — event-driven, like
  // the trace graph (user report 2027-01-07: the interpolated count-up was
  // distracting). The completion / reasoning values include their live
  // chars/4 streaming estimates so the counters advance DURING the stream —
  // in the right bucket while reasoning streams — and snap to the
  // authoritative counts when the usage event lands (the estimates reset to
  // 0 there). No interpolation: the display changes exactly when data
  // arrives.
  const promptDisplay = Math.max(0, tokenUsage.prompt - tokenUsage.cached);
  const completionDisplay = tokenUsage.completion + liveCompletionTokens;
  const reasoningDisplay = tokenUsage.reasoning + liveReasoningTokens;
  const contextUsedDisplay = contextUsage.used;

  const contextPct = hasContext
    ? Math.min(100, (contextUsedDisplay / contextUsage.max) * 100)
    : 0;
  // Color the bar by fill level: green < 50%, amber < 80%, red >= 80%.
  const contextColor =
    contextPct >= 80 ? "bg-red-500" : contextPct >= 50 ? "bg-amber-500" : "bg-green-500";

  const hasActivity = activityLog.length > 0;

  // Drag-to-resize the panel. The handle sits at the top of the bar (above
  // the thinking indicator). Dragging up grows the panel, down shrinks it.
  // (Hooks must run before any early return — rules of hooks.)
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!dragging.current || !panelRef.current) return;
      const rect = panelRef.current.getBoundingClientRect();
      // New height = distance from the panel's bottom to the cursor.
      // Cursor above the panel bottom → larger height.
      const newHeight = rect.bottom - e.clientY;
      setHeight(Math.max(MIN_HEIGHT, Math.min(MAX_HEIGHT, newHeight)));
    }
    function onUp() {
      dragging.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

  function startDrag(e: React.MouseEvent) {
    // Only resize when the panel is expanded; a drag on the collapsed bar
    // expands it first (a user action, so the choice persists too).
    if (!expanded) {
      setPanel(true);
    }
    e.preventDefault();
    dragging.current = true;
    document.body.style.cursor = "ns-resize";
    document.body.style.userSelect = "none";
  }

  // Show the bar whenever there's anything to display: while running, when
  // there's reasoning/activity, when context has been seen, OR when the token
  // usage display is on (so the bar is visible from the start — before the
  // first prompt — showing 0s / placeholder, rather than hidden until the
  // first Usage event). The ctx-bar max is seeded at startup via the
  // context_caps IPC + the registration-time ContextUsage event, so the bar
  // shows the real window size immediately.
  const showBar = running || hasActivity || showCounts || hasContext;

  if (!showBar) return null;

  return (
    <div className="bg-bg-secondary">
      {/* Drag handle — at the very top, above the thinking bar.
          Drag up to expand the reasoning panel, down to shrink. The handle
          carries the 1px border-y hairlines that separate the bar from the
          chat above and the compact bar below — the same region-separation
          motif as the App.tsx ResizeHandle (which owns the border-x
          hairlines between the agent column and the tools panel). With
          border-box sizing the 6px strip reads as 1px line + 1px gap + 2px
          grip pill + 1px gap + 1px line. The strip paints the chrome-band
          background (bg-bg-secondary) — a transparent strip showed the
          bar's lighter background between the hairlines while the vertical
          handle showed the dark app background, and the intended direction
          (user follow-up) is the lighter seam everywhere: the vertical
          handle matches the horizontal bars, not the other way around
          (user-reported: "horizontal resize bar wrong background color"). */}
      <div
        onMouseDown={startDrag}
        className="group flex h-1.5 shrink-0 cursor-ns-resize items-center justify-center border-y border-border bg-bg-secondary transition-colors hover:bg-cyan-500/20"
        title="Drag to resize"
      >
        {/* Grip pill — brightened (slate-500) so the handle reads as a
            draggable affordance against the dark bar; it was bg-border
            (#334155) on bg-bg-secondary (#1e293b), nearly invisible
            (user-reported: "too dark horizontal resize bar"). */}
        <div className="h-0.5 w-8 rounded-full bg-slate-500 group-hover:bg-slate-400" />
      </div>

      {/* The compact bar — always clickable: the reasoning panel can be
          opened/closed even when the log is still empty (e.g. the agent is
          running but hasn't streamed reasoning text yet). */}
      <button
        onClick={() => setPanel(!expanded)}
        aria-expanded={expanded}
        aria-label="Toggle reasoning panel"
        className="flex w-full items-center gap-3 px-4 py-1.5 text-xs hover:bg-bg-tertiary"
      >
        {/* Left: live phase indicator + elapsed timer (Claude-style). The
            phase cycles sending (→ compacting) → waiting → reasoning →
            answering → running tools; an approval or ask_user question
            overlays its own amber label. */}
        <span className="flex items-center gap-1.5">
          {running ? (
            <>
              <span className="thinking-dots">
                <span></span>
                <span></span>
                <span></span>
              </span>
              <span className={phaseLabelClass(pendingApproval !== null, pendingQuestion !== null)}>
                {phaseLabel(phase, pendingApproval !== null, pendingQuestion !== null)}
              </span>
              {elapsedMs !== null && (
                <span className="font-mono tabular-nums text-slate-500">
                  {fmtDuration(elapsedMs)}
                </span>
              )}
            </>
          ) : (
            <span className="text-slate-500">idle</span>
          )}
        </span>

        {/* Expand/collapse chevron — always shown while the bar is visible.
            It must NOT be gated on `hasActivity`: when the reasoning text is
            still empty (log has no entries yet) the triangle vanished and the
            panel could not be opened at all (user-reported bug). */}
        <span className="text-slate-500">
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5" />
          )}
        </span>

        {/* Right: token counts (0s until the first Usage event, except the 🧠
            counter which climbs on its live estimate while reasoning
            streams). */}
        {showCounts && (
          <span className="ml-auto flex items-center gap-3 font-mono tabular-nums text-slate-400">
            <span title="Prompt tokens sent (excludes cached)">
              <span className="text-slate-500">↑</span> {fmtTokens(promptDisplay)}
            </span>
            <span title="Completion tokens (live estimate while streaming)">
              <span className="text-slate-500">↓</span> {fmtTokens(completionDisplay)}
            </span>
            {/* The 🧠 counter also shows during the FIRST reasoning phase:
                the gate admits the live chars/4 estimate, not just landed
                usage — tokenUsage.reasoning is only set by the Usage event
                after a response completes, so gating on it alone hid the
                counter while reasoning streamed and the stats row sat
                frozen until the response landed (user report 2027-01-04). */}
            {(tokenUsage.reasoning > 0 || liveReasoningTokens > 0) && (
              <span title="Reasoning tokens (thinking)">
                <span className="text-purple-400">🧠</span> {fmtTokens(reasoningDisplay)}
              </span>
            )}
            {/* Rolling last-3-requests tok/sec (persists across turns) —
                tracks the model's CURRENT speed, not the session average.
                The per-request + session-avg timing is in the title tooltip.
                '—' while the window is still empty (no generation-timed
                requests yet — a ttft-only request never enters the window). */}
            {sessionTiming.timed_requests > 0 && (
              <span
                className="flex items-center gap-2 border-l border-border pl-3"
                title={`Session avg over ${sessionTiming.timed_requests} timed request(s) · TTFT ${avgTtftMs?.toFixed(0) ?? "?"}ms avg · generation ${avgGenMs?.toFixed(0) ?? "?"}ms avg${
                  lastRequestTiming
                    ? ` · last: TTFT ${lastRequestTiming.ttft_ms ?? "?"}ms · gen ${lastRequestTiming.generation_ms ?? "?"}ms`
                    : ""
                }`}
              >
                <span title={`Output tok/sec over the last ${RECENT_TIMING_WINDOW} requests (completion / generation time)`}>
                  <span className="text-slate-500">↓</span>{" "}
                  {outputRate !== null ? (
                    <>
                      {fmtRate(outputRate)}
                      <span className="text-slate-600">/s</span>
                    </>
                  ) : (
                    "—"
                  )}
                </span>
              </span>
            )}
          </span>
        )}

        {/* Context progress bar — tokens used vs. window max. Hover for the
            breakdown of what the context is made of (prompt/completion/
            reasoning/cached from the last request). */}
        {hasContext && (
          <span
            className={`group relative flex items-center gap-1.5 font-mono text-slate-400 ${
              !showCounts ? "ml-auto" : ""
            }`}
          >
            <span className="text-slate-500">ctx</span>
            <span className="relative h-1.5 w-20 overflow-hidden rounded-full bg-bg-tertiary">
              <span
                className={`absolute left-0 top-0 h-full rounded-full ${contextColor} transition-all duration-300`}
                style={{ width: `${contextPct}%` }}
              />
            </span>
            <span className="text-slate-500">
              {fmtTokens(contextUsedDisplay)}/{fmtTokens(contextUsage.max)}
            </span>
            {/* Hover popup: the context breakdown (what the window is made
                of). Mirrors the Hermes ctx popup. Made interactive
                (pointer-events-auto) so the Compact button is clickable.

                HOVER BRIDGE: the gap between the bar and the popup is created
                with `pb-1` (padding) on this outer positioned wrapper — NOT
                with a margin (`mb-1`). Padding stays inside the element's
                hover box, so the mouse can travel from the bar down into the
                popup without leaving the `group` span. A margin is a dead
                zone: crossing it breaks `group-hover` and the popup hides
                before the Compact button can be reached. Do NOT change this
                back to a margin. */}
            <div className="absolute bottom-full right-0 z-50 hidden w-52 pb-1 group-hover:block">
              <div className="rounded-lg border border-border bg-bg-secondary p-2 text-left text-[0.7rem] shadow-lg">
                <div className="mb-1 font-medium text-slate-300">
                  Context breakdown
                </div>
                <div className="mb-1 text-[0.65rem] leading-snug text-slate-500">
                  ctx = the provider-reported prompt token count after each
                  response (local estimate before the first response lands).
                </div>
              <div className="space-y-0.5 font-mono">
                <div className="flex justify-between">
                  <span className="text-slate-500">prompt</span>
                  <span className="text-slate-300">
                    {lastContextBreakdown
                      ? fmtTokens(lastContextBreakdown.prompt)
                      : "—"}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="text-slate-500">completion</span>
                  <span className="text-slate-300">
                    {lastContextBreakdown
                      ? fmtTokens(lastContextBreakdown.completion)
                      : "—"}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="text-purple-400">reasoning</span>
                  <span className="text-slate-300">
                    {lastContextBreakdown && lastContextBreakdown.reasoning > 0
                      ? fmtTokens(lastContextBreakdown.reasoning)
                      : "—"}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="text-amber-400">cached</span>
                  <span className="text-slate-300">
                    {lastContextBreakdown && lastContextBreakdown.cached > 0
                      ? fmtTokens(lastContextBreakdown.cached)
                      : "—"}
                  </span>
                </div>
              </div>
              <div className="mt-1 border-t border-border pt-1 text-slate-500">
                {fmtTokens(contextUsage.used)} / {fmtTokens(contextUsage.max)}{" "}
                ({fmtPct(contextPct)}%)
              </div>
              {/* Per-role breakdown (system/user/assistant/tool) — shows what
                  the context window is made of by message role. */}
              <div className="mt-1 space-y-0.5 border-t border-border pt-1 font-mono">
                <div className="mb-0.5 text-slate-600">by role</div>
                <div className="flex justify-between">
                  <span className="text-slate-500">system</span>
                  <span className="text-slate-300">{fmtTokens(contextBreakdown.system)}</span>
                </div>
                <div className="flex justify-between">
                  <span className="text-slate-500">user</span>
                  <span className="text-slate-300">{fmtTokens(contextBreakdown.user)}</span>
                </div>
                <div className="flex justify-between">
                  <span className="text-slate-500">assistant</span>
                  <span className="text-slate-300">{fmtTokens(contextBreakdown.assistant)}</span>
                </div>
                <div className="flex justify-between">
                  <span className="text-slate-500">tool</span>
                  <span className="text-slate-300">{fmtTokens(contextBreakdown.tool)}</span>
                </div>
              </div>
              {!lastContextBreakdown && (
                <div className="mt-0.5 text-slate-600">
                  no breakdown yet (send a prompt)
                </div>
              )}
              {/* Compact button — summarizes old messages into a summary
                  system message. Disabled while running (can't compact
                  mid-turn). */}
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  if (agentId !== null) {
                    compact(agentId).catch((err) =>
                      console.error("failed to compact:", err),
                    );
                  }
                }}
                disabled={running || agentId === null}
                className="mt-1.5 w-full rounded border border-cyan-600/40 bg-cyan-600/10 px-2 py-1 text-cyan-400 transition-colors hover:bg-cyan-600/20 disabled:cursor-not-allowed disabled:opacity-40"
                title={
                  running
                    ? "Wait for the agent to finish before compacting"
                    : "Summarize old messages into a summary (frees context)"
                }
              >
                Compact context
              </button>
              </div>
            </div>
          </span>
        )}
      </button>

      {/* Expandable activity panel — stays open when expanded, even if empty. */}
      {expanded && (
        <div ref={panelRef} style={{ height }} className="flex flex-col">
          {/* Scrolling content */}
          <div
            ref={scrollRef}
            className="flex-1 overflow-y-auto px-4 py-1 text-xs leading-relaxed"
            style={{
              fontFamily: "var(--app-font-family)",
              fontSize: "var(--app-font-size)",
            }}
          >
            {/* Empty state: render nothing — just the gray panel background.
                The old "—" placeholder read as a rendering artifact when
                resizing the open-but-empty panel (user-reported bug: it
                should just be an empty gray space). */}
            {hasActivity &&
              activityLog.map((entry, i) => (
                <ActivityLine key={i} entry={entry} />
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

function ActivityLine({ entry }: { entry: ActivityEntry }) {
  // Color decision (user 2026-08-22): reasoning keeps its historical dimmed
  // italic gray; the live answer channel renders in blueish gray (slate-400)
  // so the two are distinguishable in the box.
  const color =
    entry.kind === "reasoning"
      ? "text-slate-500 italic"
      : entry.kind === "answer"
        ? "text-slate-400"
        : entry.kind === "error"
          ? "text-red-400"
          : "text-slate-300";

  return (
    <div className={`whitespace-pre-wrap break-words py-0.5 ${color}`}>{entry.text}</div>
  );
}

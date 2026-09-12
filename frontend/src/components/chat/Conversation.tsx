// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useMemo, useRef, useState } from "react";
import { useAgentStore } from "../../hooks/useAgentStore";
import { isActivityEntry, isKnowledgeActivityEntry } from "../../hooks/agentState";
import type { AgentState } from "../../hooks/useAgentStore";
import type { TranscriptEntry } from "../../lib/types";
import { Message } from "./Message";
import { ApprovalPrompt } from "./ApprovalPrompt";
import { QuestionPrompt } from "./QuestionPrompt";

/** Level-2 activity kinds — consecutive runs of these get the thread-line
 *  wrapper in the transcript (the run wrapper owns their indent; Message
 *  renders them bare). Mirrors the isActivityEntry kind set. */
const ACTIVITY = new Set(["tool", "memory", "vision", "skill"]);

/** Hover timestamp: time-only for today's entries, date+time otherwise. */
function fmtTs(ts: number): string {
  const d = new Date(ts);
  return d.toDateString() === new Date().toDateString()
    ? d.toLocaleTimeString()
    : d.toLocaleString();
}

/** Split a turn's entries into chunks: maximal runs of consecutive
 *  activity entries (the thread-line groups) and single non-activity
 *  entries. */
function chunkRuns(entries: TranscriptEntry[]): TranscriptEntry[][] {
  const chunks: TranscriptEntry[][] = [];
  for (const e of entries) {
    const last = chunks[chunks.length - 1];
    if (ACTIVITY.has(e.kind) && last && last.every((x) => ACTIVITY.has(x.kind))) {
      last.push(e);
    } else {
      chunks.push([e]);
    }
  }
  return chunks;
}

/** Default render window (mem-perf): only the last TRANSCRIPT_WINDOW visible
 *  entries hit the DOM, bounding the transcript's DOM size + per-flush
 *  reconciliation cost for long (run-all overnight) sessions — the store
 *  keeps its MAX_TRANSCRIPT_ENTRIES cap regardless. Grown stepwise by the
 *  show-earlier expander. */
export const TRANSCRIPT_WINDOW = 200;

/** Tail-window a list for rendering: under the cap the SAME array is
 *  returned (hidden 0 — no allocation, no behavior change); over it, the
 *  last `windowSize` entries plus the hidden count for the expander label. */
export function windowEntries<T>(entries: T[], windowSize: number): {
  windowed: T[];
  hidden: number;
} {
  if (entries.length <= windowSize) {
    return { windowed: entries, hidden: 0 };
  }
  return {
    windowed: entries.slice(-windowSize),
    hidden: entries.length - windowSize,
  };
}

interface ConversationProps {
  state: AgentState;
}

export function Conversation({ state }: ConversationProps) {
  // GUI-only display filter (backlog db489070): when show_tool_activity is
  // off (the default), agent-activity cards (tool/memory/vision/skill) are
  // skipped at RENDER time — the transcript store keeps every entry (the
  // model's context echo is built server-side and is unaffected), and the
  // Output tab + console still log every tool call. Flipping the toggle
  // mid-session reveals past cards immediately (nothing was dropped).
  const showToolActivity = useAgentStore((s) => s.showToolActivity);
  const showKnowledgeActivity = useAgentStore((s) => s.showKnowledgeActivity);
  // Chat-readability affordances (plan afa81f0a): a thread line along
  // consecutive activity cards, an alternating turn background, and hover
  // timestamps — all GUI-only display filters, default ON.
  const chatThreadLine = useAgentStore((s) => s.chatThreadLine);
  const chatTurnTint = useAgentStore((s) => s.chatTurnTint);
  const chatHoverTimestamps = useAgentStore((s) => s.chatHoverTimestamps);
  // Render window (mem-perf): how many of the visible entries render. The
  // expander grows it by TRANSCRIPT_WINDOW per click; it never shrinks back
  // (cheap — the transcript is store-capped at MAX_TRANSCRIPT_ENTRIES).
  const [windowSize, setWindowSize] = useState(TRANSCRIPT_WINDOW);
  const endRef = useRef<HTMLDivElement>(null);
  // Ref to the scrollable container so we can measure "near bottom".
  const scrollRef = useRef<HTMLDivElement>(null);
  // Throttle scrollIntoView to once per 100ms. During streaming, the store
  // updates ~60fps (batched via requestAnimationFrame), but calling
  // scrollIntoView on every update causes jank — the browser's smooth-scroll
  // animation can't keep up. A 100ms throttle keeps the view pinned to the
  // bottom without overwhelming the layout engine.
  const lastScrollRef = useRef(0);
  const scrollTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // "Stick to bottom" flag: only auto-scroll when the user is near the bottom.
  // Starts true so a fresh conversation auto-scrolls. Becomes false when the
  // user scrolls up; becomes true again when they scroll back to the bottom.
  const stickToBottomRef = useRef(true);

  // Threshold in px: if the distance from scroll position to bottom is <= this,
  // we consider the user "at bottom" and will auto-stick.
  const NEAR_BOTTOM_PX = 80;

  // Helper: compute current distance to bottom for the scroll container.
  const distanceToBottom = (): number => {
    const el = scrollRef.current;
    if (!el) return 0;
    return el.scrollHeight - el.scrollTop - el.clientHeight;
  };

  // Update the stick flag on user scroll (passive listener).
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onScroll = () => {
      const dist = distanceToBottom();
      stickToBottomRef.current = dist <= NEAR_BOTTOM_PX;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
    };
  }, []);

  useEffect(() => {
    const doScroll = () => {
      lastScrollRef.current = Date.now();
      // Only auto-scroll if the user hasn't scrolled away from the bottom.
      if (!stickToBottomRef.current) return;
      endRef.current?.scrollIntoView({ behavior: "smooth" });
      // After a programmatic scroll we are (again) at bottom.
      stickToBottomRef.current = true;
    };
    const now = Date.now();
    const elapsed = now - lastScrollRef.current;
    if (elapsed >= 100) {
      // Enough time has passed — scroll immediately (subject to stick flag).
      if (scrollTimerRef.current) {
        clearTimeout(scrollTimerRef.current);
        scrollTimerRef.current = null;
      }
      doScroll();
    } else {
      // Schedule a trailing scroll so we always end up at the bottom when due.
      if (scrollTimerRef.current) clearTimeout(scrollTimerRef.current);
      scrollTimerRef.current = setTimeout(doScroll, 100 - elapsed);
    }
  }, [state.transcript, state.streamingText, state.pendingApproval]);

  // Clean up any pending scroll timer on unmount.
  useEffect(() => {
    return () => {
      if (scrollTimerRef.current) clearTimeout(scrollTimerRef.current);
    };
  }, []);

  // The GUI-only display filter (backlog db489070): when show_tool_activity
  // is off (the default), agent-activity cards (tool/memory/vision/skill)
  // are skipped at RENDER time — the transcript store keeps every entry
  // (the model's context echo is built server-side and is unaffected), and
  // the Output tab + console still log every tool call. Flipping the toggle
  // mid-session reveals past cards immediately (nothing was dropped).
  //
  // Group the visible entries into turns: a turn starts at each prompt
  // (user/steer) and carries everything after it (response, tools, …)
  // until the next prompt; entries before the first prompt form their own
  // turn. The grouping drives the alternating background band
  // (chat_turn_tint). Each turn is then split into activity runs
  // (chunkRuns) for the thread-line wrapper.
  //
  // Memoized (mem-perf review HIGH 2): Conversation re-renders on every
  // streaming frame (the state prop's identity changes per rAF flush),
  // but the transcript ARRAY keeps its identity while text streams into
  // state.streamingText — so the common case (text streaming into the
  // last turn) skips the entire O(transcript) filter+group+chunk pass
  // instead of redoing it at up to 60 fps. The toggles are keys too:
  // flipping one re-filters.
  const { turns, hidden } = useMemo(() => {
    const visible = state.transcript.filter(
      (entry) =>
        showToolActivity ||
        !isActivityEntry(entry) ||
        (showKnowledgeActivity && isKnowledgeActivityEntry(entry)),
    );
    // Tail window (mem-perf): only the last `windowSize` visible entries are
    // grouped + rendered; the show-earlier expander grows the window
    // stepwise. The streaming block lives in the LAST turn's group, which
    // the tail always contains — the live stream is never windowed away.
    const { windowed, hidden } = windowEntries(visible, windowSize);
    const grouped: TranscriptEntry[][] = [];
    for (const entry of windowed) {
      if (entry.kind === "user" || entry.kind === "steer" || grouped.length === 0) {
        grouped.push([entry]);
      } else {
        grouped[grouped.length - 1].push(entry);
      }
    }
    return { turns: grouped.map((turn) => chunkRuns(turn)), hidden };
  }, [state.transcript, showToolActivity, showKnowledgeActivity, windowSize]);

  // One entry wrapped for the hover-timestamp tooltip (native title attr —
  // no permanent clutter; absent ts = legacy entry = no tooltip).
  const renderEntry = (entry: TranscriptEntry, key: number | string) => (
    <div
      key={key}
      title={
        chatHoverTimestamps && entry.ts !== undefined
          ? fmtTs(entry.ts)
          : undefined
      }
    >
      <Message entry={entry} />
    </div>
  );

  // The in-flight streaming response renders INSIDE the last turn's group
  // (review L1) so the turn-tint band already covers it while streaming —
  // no visual jump when the text flushes into the transcript. Standalone
  // fallback when the transcript is still empty.
  const streamingBlock = state.streamingText ? (
    <Message
      entry={{ kind: "assistant", text: state.streamingText }}
      streaming
    />
  ) : null;

  return (
    <div
      ref={scrollRef}
      className="h-full overflow-y-auto px-4 py-4"
      style={{
        fontFamily: "var(--app-font-family)",
        fontSize: "var(--app-font-size)",
      }}
    >
      <div className="space-y-[0.5em]">
        {/* Show-earlier expander (mem-perf): the render window hides older
            entries; one click reveals the previous TRANSCRIPT_WINDOW batch.
            Disappears once everything is visible. */}
        {hidden > 0 && (
          <div className="flex justify-center py-1">
            <button
              type="button"
              onClick={() => setWindowSize((w) => w + TRANSCRIPT_WINDOW)}
              className="rounded-full border border-border px-3 py-1 text-[0.7rem] text-[color:var(--text-muted)] transition-colors hover:border-[color:var(--accent-color)] hover:text-[color:var(--accent-color)]"
            >
              Show earlier messages ({hidden} hidden)
            </button>
          </div>
        )}
        {turns.length === 0 && streamingBlock}
        {/* Keys are the stable monotonic entryId (mem-perf review HIGH 3),
            NOT the index: the transcript is capped at its last 1000 entries,
            so once at cap every append shifts all indices — an index key
            lands on a DIFFERENT entry (arePropsEqual fails → deep re-render;
            local row state like an expanded tool card silently transfers).
            Turn/run keys are their first entry's id; the position fallback
            only covers id-less legacy entries (pre-entryId saves). */}
        {turns.map((turn, ti) => (
          <div
            key={turn[0][0].entryId ?? `t${ti}`}
            className={`space-y-[0.5em] ${
              chatTurnTint && ti % 2 === 1
                ? "rounded-lg bg-bg-tertiary/30"
                : ""
            }`}
          >
            {turn.map((run, ri) =>
              run.every((e) => ACTIVITY.has(e.kind)) ? (
                <div
                  key={run[0].entryId ?? `r${ri}`}
                  className={
                    chatThreadLine
                      ? "ml-[1.5em] border-l border-slate-700/60 pl-[0.5em]"
                      : "ml-[2em]"
                  }
                >
                  {run.map((e, i) => renderEntry(e, e.entryId ?? `i${i}`))}
                </div>
              ) : (
                renderEntry(run[0], run[0].entryId ?? `r${ri}`)
              ),
            )}
            {/* Streaming text — rendered as plain text (no markdown) while
                actively streaming, then re-rendered with markdown once
                finalized and flushed to the transcript. Lives inside the
                last turn's group (review L1) so the tint band covers it. */}
            {ti === turns.length - 1 && streamingBlock}
          </div>
        ))}
        {/* Pending approval — key by toolCallId so local UI state resets
            between consecutive approvals (review L6). */}
        {state.pendingApproval && (
          <div className="pl-[1em]">
            <ApprovalPrompt
              key={state.pendingApproval.toolCallId}
              approval={state.pendingApproval}
            />
          </div>
        )}
        {/* Pending ask_user question — key by questionId so local UI state
            resets between consecutive questions. */}
        {state.pendingQuestion && (
          <div className="pl-[1em]">
            <QuestionPrompt
              key={state.pendingQuestion.questionId}
              question={state.pendingQuestion}
            />
          </div>
        )}
        <div ref={endRef} />
      </div>
    </div>
  );
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useRef, useEffect, useCallback, memo } from "react";
import { Send, Square, Play, Compass, X, Image as ImageIcon, MessageCircleQuestion, Pencil, PauseCircle } from "lucide-react";
import { InlineMarkdown } from "../chat/InlineMarkdown";
import { useAgentStore, emptyAgentState, capTranscript } from "../../hooks/useAgentStore";
import { allocEntryId, stampEntryIds, type SteerEntry } from "../../hooks/agentState";
import { clearStreamingBuffer } from "../../hooks/useAgentEvents";
import {
  sendPrompt,
  interrupt,
  approve,
  sendSuggestion,
  cancelSuggestion,
  saveConversation,
  loadConversation,
  answerQuestion,
  compact,
  clearConversation as clearConversationIpc,
} from "../../lib/tauri";
import { parseSlash, filterSlashCommands, defaultSavePath, HELP_TEXT, resolveArglessCommand } from "../../lib/slash";
import { parseNumericAnswer } from "../../lib/answerSelect";
import { fileToAttachedDataUrl } from "../../lib/attachImages";
import type { SlashCommandInfo } from "../../lib/slash";
import type { TranscriptEntry } from "../../lib/types";
import {
  pushPrompt,
  navigateUp,
  navigateDown,
  isOnFirstVisualLine,
  isOnLastVisualLine,
  resetNavigation,
} from "../../lib/promptHistory";
import { autosizeForScrollHeight } from "../../lib/textareaAutosize";

/**
 * Prompt input bar (self-subscribing). Memoized (mem-perf review HIGH 1):
 * prop-less, so App-level re-renders can't drag it along — it only
 * re-renders when its own store slices change.
 */
export const InputBar = memo(function InputBar() {
  const [text, setText] = useState("");
  const [attachedImages, setAttachedImages] = useState<string[]>([]);
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const agents = useAgentStore((s) => s.agents);
  const clearConversation = useAgentStore((s) => s.clearConversation);
  const toggleRightPanel = useAgentStore((s) => s.toggleRightPanel);
  const setModel = useAgentStore((s) => s.setModel);
  const setProvider = useAgentStore((s) => s.setProvider);
  const addSteer = useAgentStore((s) => s.addSteer);
  const removeSteer = useAgentStore((s) => s.removeSteer);
  const recordQuestionAnswer = useAgentStore((s) => s.recordQuestionAnswer);
  const setFreeformQuestion = useAgentStore((s) => s.setFreeformQuestion);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const running = activeAgent !== null ? agents[activeAgent]?.running ?? false : false;
  // True when the active agent's last turn ended in a FINAL error — drives
  // the Continue button (`!running && failed`): the task loop survived, so
  // the user can resume the failed task with a continuation prompt.
  const failed = activeAgent !== null ? agents[activeAgent]?.failed ?? false : false;
  // Pre-stall evidence from the backend `parked` event — set when the agent
  // went idle awaiting input instead of auto-continuing. The two
  // manual-input reasons (user stop / exhausted auto-continue budget)
  // surface as the parked banner below so an interrupted turn never looks
  // like a hang (backlog 5c33e945, 2027-01-07 live: a mid-Executing stop
  // looked like a hang and needed a manual "c").
  const parked = activeAgent !== null ? agents[activeAgent]?.parked ?? null : null;
  const parkedNeedsInput =
    parked !== null &&
    (parked.reason === "interrupted" || parked.reason === "budget_exhausted");
  // Steer vs. send is now automatic: while the agent is running, the input
  // steers (mid-work guidance injected at the next turn boundary); while
  // idle, it sends a new prompt. No manual toggle. The one carve-out:
  // slash commands are control input — they execute in every agent state
  // and never steer (see handleSend).
  const steerMode = running;

  // The active agent's pending ask_user question + freeform-answer mode.
  // When a question is pending, typing `1)`, `2)`, … resolves it by choice
  // index; picking the "Let's talk about it" number switches the InputBar
  // into freeform-answer mode (Enter sends the typed text as the answer).
  const pendingQuestion =
    activeAgent !== null ? agents[activeAgent]?.pendingQuestion ?? null : null;
  const freeformQuestionId =
    activeAgent !== null ? agents[activeAgent]?.freeformQuestionId ?? null : null;
  const inFreeformMode =
    freeformQuestionId !== null && freeformQuestionId === pendingQuestion?.questionId;

  // Subagent lock: when the active agent is a subagent (has a parent), the
  // InputBar is disabled — the user cannot send commands to a subagent. The
  // Stop button stays available (interrupting a subagent is allowed).
  const agentParents = useAgentStore((s) => s.agentParents);
  const isSubagent =
    activeAgent !== null && (agentParents[activeAgent] ?? null) !== null;

  // Slash-command menu state. `menuDismissed` hides the menu for the current
  // text (set by Escape); it re-arms when the text changes.
  const [menuIndex, setMenuIndex] = useState(0);
  const [menuDismissed, setMenuDismissed] = useState(false);
  const menuOpen =
    attachedImages.length === 0 && text.startsWith("/") && !menuDismissed;
  const menuItems = menuOpen ? filterSlashCommands(text) : [];

  // Auto-resize the textarea. overflowY switches to "auto" only once the
  // height clamps at the cap — below it, the border-box shortfall would
  // otherwise render a permanent (phantom) scrollbar thumb at the right
  // edge.
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    const { heightPx, overflowY } = autosizeForScrollHeight(ta.scrollHeight, 200);
    ta.style.height = `${heightPx}px`;
    ta.style.overflowY = overflowY;
  }, [text]);

  // Reset the highlighted menu row whenever the filter changes.
  useEffect(() => {
    setMenuIndex(0);
  }, [text]);

  // Clamp the highlight if the filtered list shrinks (e.g. typing narrows it).
  useEffect(() => {
    if (menuIndex >= menuItems.length) {
      setMenuIndex(Math.max(0, menuItems.length - 1));
    }
  }, [menuItems.length, menuIndex]);

  /** Add image files (from paste or drop) to the attached-images list.
   * Each file is downscaled to the attachment long-edge cap and re-encoded
   * as JPEG (lib/attachImages) before it reaches the store — full-size
   * screenshots must not live in the transcript for the whole session
   * (mem-perf review LOW 4). */
  const addImageFiles = useCallback(async (files: File[]) => {
    const imageFiles = files.filter((f) => f.type.startsWith("image/"));
    if (imageFiles.length === 0) return;
    const dataUrls = await Promise.all(imageFiles.map(fileToAttachedDataUrl));
    setAttachedImages((prev) => [...prev, ...dataUrls]);
  }, []);

  /** Paste handler — detects image files in the clipboard. */
  function handlePaste(e: React.ClipboardEvent) {
    const files = Array.from(e.clipboardData.files);
    if (files.some((f) => f.type.startsWith("image/"))) {
      e.preventDefault();
      void addImageFiles(files);
    }
  }

  /** Drag-and-drop handler — accepts image files dropped onto the input. */
  function handleDrop(e: React.DragEvent) {
    e.preventDefault();
    const files = Array.from(e.dataTransfer.files);
    void addImageFiles(files);
  }

  function handleDragOver(e: React.DragEvent) {
    e.preventDefault();
  }

  /** Remove an attached image by index. */
  function removeImage(index: number) {
    setAttachedImages((prev) => prev.filter((_, i) => i !== index));
  }

  /** Push a transient system/assistant message into the transcript.
   *  Stamps the arrival time (hover timestamps, plan afa81f0a) + the stable
   *  entryId (React keys, mem-perf review HIGH 3) — these direct pushes
   *  bypass applyAgentEvent's identity-based stamping. */
  function pushTranscriptMessage(agentId: number, msg: TranscriptEntry) {
    const stamped = {
      ...msg,
      ts: msg.ts ?? Date.now(),
      entryId: msg.entryId ?? allocEntryId(),
    };
    useAgentStore.setState((s) => {
      const agent = s.agents[agentId] ?? emptyAgentState();
      return {
        agents: {
          ...s.agents,
          [agentId]: {
            ...agent,
            transcript: capTranscript([...agent.transcript, stamped]),
          },
        },
      };
    });
  }

  /**
   * Complete/select a slash command from the menu. Argument-less commands
   * (takesArgument === false: /clear, /compact, /new, /panel, /help) run
   * immediately. Commands that take an argument (/model, /provider, /load)
   * complete the text to "<usage> " so the user can type the argument. /save
   * completes with a sensible default path
   * (`.coding/conversations/<timestamp>.json`) since there is no file-picker
   * dialog wired in — the user can edit or delete it (empty = same default).
   */
  function completeSlashCommand(cmd: SlashCommandInfo) {
    if (!cmd.takesArgument) {
      // Run immediately. Invoke the command directly rather than routing
      // through handleSend(): handleSend reads `text` from the current
      // render's closure, so a `setText` + deferred handleSend would see the
      // STALE partial input (e.g. "/c"), not "/clear". Direct invocation
      // sidesteps the stale closure entirely. (handleSend itself now runs
      // slash commands in every agent state — running included — so both
      // paths execute mid-run identically; the direct call is purely about
      // the stale closure, not steer interception.)
      setText("");
      void handleSlashCommand(parseSlash(`/${cmd.name}`));
    } else if (cmd.name === "save") {
      setText(`/save ${defaultSavePath()}`);
    } else {
      // /model, /provider, /load — complete to "/name " and let the user
      // type the argument.
      setText(`/${cmd.name} `);
    }
    setMenuDismissed(false);
    textareaRef.current?.focus();
  }

  async function handleSend() {
    // When the slash menu is open, Enter/Tab/click already completed the
    // command into `text`; this path runs the now-complete command (or a
    // regular prompt). Clear the menu dismissal so a fresh `/` re-opens it.
    setMenuDismissed(false);
    if (activeAgent === null) return;

    // Freeform-answer mode: the user picked the "Let's talk about it" number
    // and is now typing their answer. Enter submits it as a freeform answer
    // (not a prompt/steer). Esc (handled in handleKeyDown) cancels the mode.
    if (inFreeformMode && pendingQuestion) {
      const answer = text.trim();
      if (!answer) return;
      const q = pendingQuestion;
      setText("");
      setFreeformQuestion(activeAgent, null);
      const ok = await answerQuestion(q.questionId, { kind: "freeform", text: answer });
      if (ok) {
        recordQuestionAnswer(activeAgent, {
          questionId: q.questionId,
          question: q.question,
          answer,
        });
      }
      return;
    }

    // Numeric-answer interception: if a question is pending and the trimmed
    // text is `N)` (digit(s) + `)`), resolve the question by choice index.
    // `1..options.length` → choice; `options.length+1` → freeform mode. This
    // takes precedence over sending a prompt/steer so the user can answer
    // without leaving the command box.
    if (pendingQuestion && !inFreeformMode) {
      const parsed = parseNumericAnswer(text, pendingQuestion.options.length);
      if (parsed) {
        const q = pendingQuestion;
        if (parsed.kind === "choice") {
          const label = q.options[parsed.index]?.label ?? `Option ${parsed.index + 1}`;
          setText("");
          const ok = await answerQuestion(q.questionId, {
            kind: "choice",
            index: parsed.index,
          });
          if (ok) {
            recordQuestionAnswer(activeAgent, {
              questionId: q.questionId,
              question: q.question,
              answer: label,
            });
          }
        } else {
          // "Let's talk about it" — switch to freeform mode, keep focus + the
          // typed text so the user can continue typing their answer.
          setFreeformQuestion(activeAgent, q.questionId);
        }
        return;
      }
    }

    if ((!text.trim() && attachedImages.length === 0)) return;
    const input = text;
    const images = attachedImages;
    // Commands are recorded in history on send (dedupes consecutive
    // duplicates) — both steers (pending while the agent runs) and regular
    // prompts. Slash commands are not (they're control input, not prompts).
    setText("");
    setAttachedImages([]);
    const agentId = activeAgent;

    // Slash commands are control input, not prompts: they execute in EVERY
    // agent state — running or idle — and never steer. This check must sit
    // AHEAD of the steer branch below: pre-fix it sat after it, so a
    // menu-closed "/compact" mid-run (Esc-dismissed menu, or the Send
    // button) was sent to the model as a steer suggestion while the
    // menu-open path ran it directly — the mid-run asymmetry (backlog
    // b47b3f44). Every handler is mid-run-safe by construction: /clear,
    // /new and /load interrupt + drain first, /compact is designed to
    // resume mid-task, the rest are frontend-only. Image-bearing input is
    // never a command (it's a prompt/steer).
    if (images.length === 0) {
      const slash = parseSlash(input);
      if (slash) {
        handleSlashCommand(slash);
        return;
      }
    }

    // Steer mode: send as a mid-work suggestion (injected at next turn
    // boundary), not a new prompt. Add it to the backlog so the user sees
    // it pending until the agent injects it (suggestion_injected event).
    // Images ride the steer payload end-to-end and are injected as image
    // blocks exactly like a normal prompt's. Slash commands never reach
    // this branch (they execute above, in every agent state).
    if (steerMode) {
      pushPrompt(input);
      addSteer(agentId, input, images);
      try {
        await sendSuggestion(agentId, input, images);
      } catch (e) {
        console.error("failed to send suggestion:", e);
      }
      return;
    }

    // Regular prompt — add the user message to the transcript immediately.
    // Include images in the transcript entry so they render as thumbnails.
    pushPrompt(input);
    useAgentStore.setState((s) => {
      const agent = s.agents[agentId] ?? emptyAgentState();
      return {
        agents: {
          ...s.agents,
          [agentId]: {
            ...agent,
            transcript: capTranscript([
              ...agent.transcript,
              {
                kind: "user",
                text: input,
                ts: Date.now(),
                entryId: allocEntryId(),
                ...(images.length > 0 ? { images } : {}),
              },
            ]),
          },
        },
      };
    });

    try {
      await sendPrompt(agentId, input, images);
    } catch (e) {
      console.error("failed to send prompt:", e);
    }
  }

  async function handleStop() {
    if (activeAgent === null) return;
    try {
      await interrupt(activeAgent);
    } catch (e) {
      console.error("failed to interrupt:", e);
    }
  }

  // Resume a task whose turn ended in a FINAL error: the agent's task loop
  // survived (the backend keeps the agent alive on final errors), so a fresh
  // prompt continues the work from where it stopped. Available even for
  // subagents — a failed background agent must be continuable, same as Stop
  // (only typing commands into a subagent is locked out).
  async function handleContinue() {
    if (activeAgent === null) return;
    try {
      await sendPrompt(activeAgent, "Continue from where you left off.");
    } catch (e) {
      console.error("failed to send continue:", e);
    }
  }

  async function handleSlashCommand(cmd: ReturnType<typeof parseSlash>) {
    if (!cmd) return;
    const agentId = activeAgent;
    switch (cmd.type) {
      case "clear":
        if (agentId !== null) {
          // If an approval is sitting open, resolve as deny_all so the rest of
          // the turn latches (Phase 1c / review M1) — not a single deny that
          // could leave later tools in the batch free to prompt or auto-run.
          // Fire-and-forget — clear still proceeds.
          const pending =
            useAgentStore.getState().agents[agentId]?.pendingApproval ?? null;
          if (pending) {
            approve(pending.toolCallId, "deny_all").catch((e) =>
              console.error("failed to deny_all pending approval on clear:", e),
            );
          }
          // Interrupt whenever the agent is running OR an approval was open
          // (approval can sit while running is still true; belt-and-suspenders
          // if the running flag is briefly stale). Fire-and-forget.
          if (running || pending) {
            interrupt(agentId).catch((e) =>
              console.error("failed to interrupt before clear:", e),
            );
          }
          // Drain any text_delta fragments already buffered in the rAF flush
          // queue — otherwise they'd flush on the next animation frame and
          // repopulate the just-cleared streamingText.
          clearStreamingBuffer(agentId);
          clearConversation(agentId);
        }
        break;
      case "compact":
        if (agentId !== null) {
          // Fire-and-forget — the agent processes the compaction
          // asynchronously (if mid-turn, the turn ends first via
          // StopReason::Compact, then RESUMES on the compacted
          // conversation). The backend announces start (compact_started) +
          // result (compacted/error) in the transcript — no optimistic push
          // here, so the popup-button path announces identically.
          compact(agentId).catch((e) =>
            console.error("failed to compact:", e),
          );
        }
        break;
      case "new":
        if (agentId !== null) {
          // If running, interrupt first so the turn ends cleanly before
          // clearing. Fire-and-forget.
          if (running) {
            interrupt(agentId).catch((e) =>
              console.error("failed to interrupt before new:", e),
            );
          }
          // Drain any buffered text_delta fragments.
          clearStreamingBuffer(agentId);
          // Send the Clear command to the backend (wipes the message
          // history + emits a ContextUsage { used: 0 } event).
          clearConversationIpc(agentId).catch((e) =>
            console.error("failed to clear conversation:", e),
          );
          // Reset the frontend agent state to a fresh empty state (clears
          // transcript + tokenUsage + sessionTiming + contextUsage +
          // lastContextBreakdown — a full reset).
          useAgentStore.setState((s) => ({
            agents: {
              ...s.agents,
              [agentId]: emptyAgentState(),
            },
          }));
        }
        break;
      case "panel":
        toggleRightPanel();
        break;
      case "help":
        // Push help text into the transcript as an assistant message.
        if (agentId !== null) {
          pushTranscriptMessage(agentId, { kind: "assistant", text: HELP_TEXT });
        }
        break;
      case "model":
        if (cmd.name) {
          setModel(cmd.name);
          if (agentId !== null) {
            pushTranscriptMessage(agentId, {
              kind: "assistant",
              text: `Model set to "${cmd.name}". The change takes effect on restart.`,
            });
          }
        }
        break;
      case "provider":
        if (cmd.name) {
          setProvider(cmd.name);
          if (agentId !== null) {
            pushTranscriptMessage(agentId, {
              kind: "assistant",
              text: `Provider set to "${cmd.name}". The change takes effect on restart.`,
            });
          }
        }
        break;
      case "save": {
        if (agentId === null) break;
        const agent = useAgentStore.getState().agents[agentId];
        const transcript = agent?.transcript ?? [];
        // Serializes the CAPPED transcript: entries beyond the 1000-entry
        // count cap are already gone, and images evicted by the transcript
        // image byte budget (capTranscriptImages) serialize as
        // `imagesEvicted` placeholder counts — no payload. Intended: saves
        // stay bounded instead of growing unbounded with pasted images.
        const content = JSON.stringify(transcript, null, 2);
        try {
          const written = await saveConversation(agentId, cmd.path ?? "", content);
          pushTranscriptMessage(agentId, {
            kind: "assistant",
            text: `Conversation saved to ${written}`,
          });
        } catch (e) {
          pushTranscriptMessage(agentId, {
            kind: "error",
            text: `Failed to save conversation: ${e}`,
          });
        }
        break;
      }
      case "load": {
        if (agentId === null) break;
        if (!cmd.path) {
          pushTranscriptMessage(agentId, {
            kind: "error",
            text: "Usage: /load <path>",
          });
          break;
        }
        try {
          const raw = await loadConversation(agentId, cmd.path);
          const parsed = JSON.parse(raw) as TranscriptEntry[];
          // Interrupt the backend + drain the rAF buffer when loading
          // mid-stream, for the same reason as `/clear`: otherwise the agent
          // keeps emitting events that append to / repopulate the replaced
          // transcript. Fire-and-forget — the load proceeds regardless.
          if (running) {
            interrupt(agentId).catch((e) =>
              console.error("failed to interrupt before load:", e),
            );
          }
          clearStreamingBuffer(agentId);
          // Replace the current transcript with the loaded one (capped — a
          // loaded file is untrusted in size; keep the most recent entries).
          // Stamp stable entryIds on any entries lacking one (legacy saves
          // predate the field) so the React keys stay stable past the cap.
          useAgentStore.setState((s) => {
            const agent = s.agents[agentId] ?? emptyAgentState();
            return {
              agents: {
                ...s.agents,
                [agentId]: {
                  ...agent,
                  transcript: capTranscript(stampEntryIds(parsed)),
                },
              },
            };
          });
          pushTranscriptMessage(agentId, {
            kind: "assistant",
            text: `Loaded conversation from ${cmd.path}`,
          });
        } catch (e) {
          pushTranscriptMessage(agentId, {
            kind: "error",
            text: `Failed to load conversation: ${e}`,
          });
        }
        break;
      }
      case "unknown":
        console.log("unknown command:", cmd.raw);
        break;
    }
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    // Slash menu navigation takes precedence while it is open with matches.
    if (menuOpen && menuItems.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMenuIndex((i) => (i + 1) % menuItems.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMenuIndex((i) => (i - 1 + menuItems.length) % menuItems.length);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuDismissed(true);
        return;
      }
      // Tab always completes the highlighted command.
      if (e.key === "Tab") {
        e.preventDefault();
        completeSlashCommand(menuItems[Math.min(menuIndex, menuItems.length - 1)]);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        const body = text.slice(1);
        const hasArgs = /\s/.test(body.trim());
        if (!hasArgs) {
          // A fully-typed argument-less command ("/compact", or "/compact "
          // after a completion) — run it now. Without this, Enter re-completed
          // forever: completion appends the trailing space, the trim in
          // hasArgs strips it, and the loop never reached a run path.
          const exact = resolveArglessCommand(text);
          if (exact) {
            completeSlashCommand(exact);
            return;
          }
          // Still choosing a command — Enter completes the highlighted one
          // (same as Tab). This is the "pick the top/active suggestion" UX:
          // Enter on a partial `/xyz` selects, it does not run an unknown cmd.
          completeSlashCommand(menuItems[Math.min(menuIndex, menuItems.length - 1)]);
          return;
        }
        // The user typed a full command + args — run it as-is.
        setMenuDismissed(false);
        void handleSend();
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void handleSend();
      return;
    }
    // Esc cancels freeform-answer mode (keeps the question pending) — the
    // user backed out of typing a freeform answer. Only when in freeform
    // mode (not when the slash menu is open — that's handled above).
    if (e.key === "Escape" && inFreeformMode && activeAgent !== null) {
      e.preventDefault();
      setFreeformQuestion(activeAgent, null);
      return;
    }

    // Prompt history (up/down) — only when the slash menu is NOT open.
    // Multiline-safe: Up navigates only when the cursor is on the first line
    // (no newline before it); Down only when on the last line (no newline
    // after). Otherwise the textarea's default cursor movement handles
    // multiline editing.
    const ta = e.currentTarget as HTMLTextAreaElement;
    const selStart = ta.selectionStart;
    if (e.key === "ArrowUp" && isOnFirstVisualLine(ta, selStart)) {
      const shown = navigateUp(text);
      if (shown !== null) {
        e.preventDefault();
        setText(shown);
        // Move the cursor to the end of the inserted prompt.
        requestAnimationFrame(() => {
          const t = textareaRef.current;
          if (t) {
            t.selectionStart = t.selectionEnd = t.value.length;
          }
        });
      }
    } else if (e.key === "ArrowDown" && isOnLastVisualLine(ta, selStart)) {
      const shown = navigateDown();
      if (shown !== null) {
        e.preventDefault();
        setText(shown);
        requestAnimationFrame(() => {
          const t = textareaRef.current;
          if (t) {
            t.selectionStart = t.selectionEnd = t.value.length;
          }
        });
      }
    }
  }

  /** Cancel a pending steer FULL-STACK (the X flow, shared with
   * click-to-edit): remove the frontend bubble AND send a backend cancel so
   * the queued steer is dropped before injection (a frontend-only removal
   * would hide the bubble but the backend would still inject it). */
  function cancelPendingSteer(steer: SteerEntry) {
    if (activeAgent === null) return;
    removeSteer(activeAgent, steer.id);
    cancelSuggestion(activeAgent, steer.text).catch((e) =>
      console.error("failed to cancel suggestion:", e),
    );
  }

  const steers =
    activeAgent !== null ? agents[activeAgent]?.steers ?? [] : [];

  const canSend =
    (text.trim() || attachedImages.length > 0) &&
    activeAgent !== null &&
    !isSubagent;

  return (
    <div className="flex flex-col gap-2 border-t border-border bg-bg-secondary p-3">
      {/* Freeform-answer banner — shown when the user picked the "Let's talk
          about it" number and is typing their answer in this InputBar. */}
      {inFreeformMode && pendingQuestion && (
        <div className="flex items-center gap-2 rounded-md border border-cyan-600/40 bg-cyan-950/20 px-2 py-1 text-xs text-cyan-300">
          <MessageCircleQuestion className="h-3.5 w-3.5 shrink-0" />
          <span className="truncate">
            Answering: {pendingQuestion.question} — Esc to cancel
          </span>
        </div>
      )}
      {/* Steers backlog — pending + landed steers, shown above the input.
          Pending = waiting to be injected; landed = injected (fades out). */}
      {steers.length > 0 && (
        <div className="flex flex-col gap-1">
          {steers.map((steer) => (
            <div
              key={steer.id}
              className={`flex items-center gap-2 rounded-md border px-2 py-1 text-xs ${
                steer.status === "landed"
                  ? "border-amber-500/60 bg-amber-950/30 text-amber-300"
                  : "group border-amber-600/30 bg-amber-950/10 text-amber-400/80"
              }`}
            >
              <Compass className="h-3 w-3 shrink-0" />
              {steer.images && steer.images.length > 0 && (
                <span
                  className="flex shrink-0 items-center gap-0.5 text-amber-400/70"
                  title={`${steer.images.length} image attachment${steer.images.length > 1 ? "s" : ""} carried with this steer`}
                >
                  <ImageIcon className="h-3 w-3" />
                  {steer.images.length}
                </span>
              )}
              {steer.status === "landed" ? (
                // Landed — injected; NOT editable (the steer already took
                // effect, and the bubble auto-removes after fading out).
                <span className="truncate">
                  <InlineMarkdown text={steer.text} />
                </span>
              ) : (
                // Pending — click-to-edit (draft mode): the text loads into
                // the main input (the edit surface) and the pending steer is
                // cancelled FULL-STACK immediately — the exact X flow.
                // Sending re-queues the edited text as a fresh steer via the
                // normal steer branch. Once the backend processes the cancel
                // the queue no longer holds the steer, so nothing can land
                // mid-edit; the residual click-instant race (the cancel
                // arriving after injection) is the X flow's pre-existing IPC
                // race — the landing still shows as a transcript steer
                // entry. No edit state exists, so nothing goes stale after
                // injection. An accidental click is recoverable: the text
                // sits in the input, one Enter away from re-queuing.
                // Overwrites any input draft — same semantics as the
                // Up-arrow history navigation. NOTE: the backend queue is
                // TEXT-based — cancelling drops ALL pending steers with
                // identical text (pre-existing limitation, see the
                // X-to-delete SPEC).
                <button
                  type="button"
                  onClick={() => {
                    if (activeAgent === null) return;
                    setText(steer.text);
                    // The steer's images load back into the attachment tray
                    // so re-sending keeps them (same draft semantics as the
                    // text).
                    setAttachedImages(steer.images ?? []);
                    resetNavigation();
                    cancelPendingSteer(steer);
                    textareaRef.current?.focus();
                  }}
                  className="flex min-w-0 flex-1 cursor-pointer items-center text-left transition-colors hover:text-amber-300"
                  title="Edit this steer — loads it into the input; send re-queues it"
                >
                  <span className="truncate">
                    <InlineMarkdown text={steer.text} />
                  </span>
                  <Pencil className="ml-1 h-2.5 w-2.5 shrink-0 opacity-40 transition-opacity group-hover:opacity-100" />
                </button>
              )}
              {steer.status === "landed" ? (
                <span className="ml-auto shrink-0 font-medium text-amber-400">
                  landed
                </span>
              ) : (
                // Pending steer — an "x" to cancel it before injection
                // (the shipped full-stack cancel, plan 7da676df): removes
                // the frontend bubble AND sends a backend cancel so the
                // queued steer is dropped (a frontend-only removal would
                // hide the bubble but the backend would still inject it).
                <button
                  onClick={() => cancelPendingSteer(steer)}
                  className="ml-auto flex h-4 w-4 shrink-0 items-center justify-center rounded text-amber-400/60 transition-colors hover:bg-amber-950/40 hover:text-amber-300"
                  title="Cancel this steer"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      {/* Attached images — thumbnails with a remove button. */}
      {attachedImages.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {attachedImages.map((dataUrl, i) => (
            <div
              key={i}
              className="group relative h-16 w-16 overflow-hidden rounded-lg border border-border bg-bg-primary"
            >
              <img
                src={dataUrl}
                alt={`attachment ${i + 1}`}
                className="h-full w-full object-cover"
              />
              <button
                onClick={() => removeImage(i)}
                className="absolute right-0.5 top-0.5 flex h-5 w-5 items-center justify-center rounded bg-black/60 text-white opacity-0 transition-opacity group-hover:opacity-100"
                title="Remove image"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          ))}
        </div>
      )}
      {/* Parked banner — the agent went idle awaiting input instead of
          auto-continuing (backend `parked` event). Only the two manual-input
          reasons surface: a user Stop (interrupted) and an exhausted
          auto-continue budget. An interrupted turn must not look like a hang
          (backlog 5c33e945, 2027-01-07 live: a mid-Executing stop looked
          like a hang and needed a manual "c"). Cleared by the next turn. */}
      {!running && parkedNeedsInput && parked !== null && (
        <div
          role="status"
          className="flex items-center gap-2 rounded-lg border border-amber-700/40 bg-amber-950/30 px-3 py-1.5 text-xs text-amber-300"
        >
          <PauseCircle className="h-4 w-4 shrink-0" />
          {parked.reason === "interrupted"
            ? "Turn interrupted — send a message to resume."
            : "Auto-continue budget exhausted — send a message to continue."}
          <span className="ml-auto shrink-0 text-amber-500/70">
            wf={parked.workflow_state} · streak={parked.auto_continue_streak}
          </span>
        </div>
      )}
      <div
        className="relative flex items-end gap-2"
        onDrop={handleDrop}
        onDragOver={handleDragOver}
      >
        {/* Slash-command menu — shown while the input starts with "/" and no
            images are attached. Filtered by the typed prefix; Enter/Tab/click
            completes the highlighted command, Escape dismisses. */}
        {menuOpen && menuItems.length > 0 && (
          <div
            role="listbox"
            aria-label="Slash commands"
            className="absolute bottom-full left-0 z-20 mb-2 w-full max-w-md overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-lg"
          >
            {menuItems.map((cmd, i) => (
              <button
                key={cmd.name}
                type="button"
                role="option"
                aria-selected={i === menuIndex}
                onMouseEnter={() => setMenuIndex(i)}
                onClick={() => completeSlashCommand(cmd)}
                className={`flex w-full items-baseline gap-2 px-3 py-2 text-left text-sm ${
                  i === menuIndex
                    ? "bg-cyan-600/20 text-slate-200"
                    : "text-slate-300 hover:bg-bg-primary"
                }`}
              >
                <span className="shrink-0 font-medium text-cyan-400">
                  {cmd.usage}
                </span>
                <span className="truncate text-slate-400">
                  {cmd.description}
                  {cmd.hint && (
                    <span className="text-slate-500"> — {cmd.hint}</span>
                  )}
                </span>
              </button>
            ))}
            <div className="border-t border-border px-3 py-1 text-[10px] text-slate-500">
              ↑↓ navigate · Enter/Tab select · Esc dismiss
            </div>
          </div>
        )}
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            // Typing re-arms the menu after an Escape dismissal.
            setMenuDismissed(false);
            // Typing after navigating history resets the cursor so a fresh Up
            // starts from the newest entry (the typed text is the new draft).
            resetNavigation();
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          disabled={isSubagent}
          placeholder={
            isSubagent
              ? "Subagents are read-only — switch to the main agent to send commands"
              : inFreeformMode && pendingQuestion
                ? `Type your answer to "${pendingQuestion.question}"… (Enter to send, Esc to cancel)`
                : steerMode
                  ? "Steer the agent — guidance injected mid-work (Enter to send)..."
                  : pendingQuestion
                    ? "Type a message, or answer the question above with 1), 2), … (Enter to send)"
                    : "Type your message... (Enter to send, Shift+Enter for newline, / for commands, paste images)"
          }
          rows={1}
          style={{
            fontFamily: "var(--app-font-family)",
            fontSize: "var(--app-font-size)",
          }}
          className="flex-1 resize-none rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-slate-200 placeholder-slate-500 focus:border-cyan-500 focus:outline-none disabled:opacity-50"
        />
        {/* Image attachment indicator — shown when images are attached. */}
        {attachedImages.length > 0 && (
          <div
            className="flex h-9 items-center gap-1 rounded-lg bg-cyan-600/20 px-2 text-xs text-cyan-400"
            title={`${attachedImages.length} image${attachedImages.length > 1 ? "s" : ""} attached`}
          >
            <ImageIcon className="h-4 w-4" />
            {attachedImages.length}
          </div>
        )}
        {/* Continue — shown when the agent's last turn ended in a FINAL error
            (idle + failed): resume the dead task with a continuation prompt.
            Available even for a subagent (a failed background agent must be
            continuable; only typing commands into it is locked out). */}
        {!running && failed && (
          <button
            onClick={handleContinue}
            className="flex h-9 w-9 items-center justify-center rounded-lg bg-amber-600 text-white transition-colors hover:bg-amber-500"
            title="Continue from where you left off"
          >
            <Play className="h-4 w-4" />
          </button>
        )}
        {/* Stop — only shown when the agent is running. Available even for a
            subagent (interrupting a subagent is allowed; only sending commands
            is locked out). */}
        {running && (
          <button
            onClick={handleStop}
            className="flex h-9 w-9 items-center justify-center rounded-lg bg-red-600 text-white transition-colors hover:bg-red-500"
            title="Stop generation"
          >
            <Square className="h-4 w-4" />
          </button>
        )}
        {/* Send / Steer — hidden when the active agent is a subagent (no
            sending commands to subagents). The Stop button above stays. */}
        {!isSubagent && (
          <button
            onClick={handleSend}
            disabled={!canSend}
            className={`flex h-9 w-9 items-center justify-center rounded-lg text-white transition-colors disabled:opacity-40 ${
              inFreeformMode
                ? "bg-cyan-600 hover:bg-cyan-500"
                : steerMode
                  ? "bg-amber-600 hover:bg-amber-500"
                  : "bg-cyan-600 hover:bg-cyan-500"
            }`}
            title={
              inFreeformMode
                ? "Send freeform answer"
                : steerMode
                  ? "Send steering guidance"
                  : "Send message"
            }
          >
            <Send className="h-4 w-4" />
          </button>
        )}
      </div>
    </div>
  );
});

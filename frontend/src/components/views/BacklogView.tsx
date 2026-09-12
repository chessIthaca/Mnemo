// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useState, useRef, useEffect, useCallback } from "react";
import {
  Play,
  X,
  ChevronUp,
  ChevronDown,
  ChevronRight,
  RotateCcw,
  Trash2,
  Image as ImageIcon,
  Plus,
  Pencil,
  Check,
  Copy,
  ArrowUpToLine,
  ArrowDownToLine,
  LayoutTemplate,
} from "lucide-react";
import remarkBreaks from "remark-breaks";
import { Markdown } from "../chat/Markdown";
import { fileToAttachedDataUrl } from "../../lib/attachImages";
import { useAgentStore, selectMainAgentId } from "../../hooks/useAgentStore";
import {
  backlogAdd,
  backlogRemove,
  backlogReorder,
  backlogClearFinished,
  backlogRetry,
  backlogSetDeferred,
  backlogDispatchItem,
  backlogSetAutoFeed,
  backlogSetParallelRunAll,
  backlogRunAll,
  backlogStopAll,
  backlogEdit,
  errMsg,
} from "../../lib/tauri";
import { autosizeForScrollHeight } from "../../lib/textareaAutosize";
import type { AgentId, BacklogItem, BacklogStatus } from "../../lib/types";

/** Badge colors per backlog status. */
const STATUS_STYLES: Record<BacklogStatus, string> = {
  pending: "bg-slate-700/50 text-slate-400",
  in_flight: "bg-cyan-950/60 text-cyan-400",
  done: "bg-green-950/60 text-green-400",
  failed: "bg-red-950/60 text-red-400",
  cant_resolve: "bg-amber-950/60 text-amber-400",
};

/** Badge colors per live workflow phase (used for the in-flight item). */
const PHASE_STYLES: Record<string, string> = {
  planning: "bg-yellow-950/60 text-yellow-400",
  executing: "bg-green-950/60 text-green-400",
  reviewing: "bg-purple-950/60 text-purple-400",
  complete: "bg-blue-950/60 text-blue-400",
};

/** Short relative/absolute timestamp for a backlog item. */
function formatTimestamp(createdAt: number): string {
  const now = Date.now();
  const diffMs = now - createdAt;
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 1) return "just now";
  if (diffMin < 60) return `${diffMin}m ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const d = new Date(createdAt);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/**
 * The backlog item template (2027-01-07 detail bar): the 5-section skeleton
 * a detailed item carries — problem+repro, files/symbols, fix direction,
 * acceptance, related pointers. Pre-filled by the composer's Template
 * button so a hand-written item passes the detail bar the backend enforces
 * (≥160-char body naming at least one file path, or an explicit
 * 'no-code research' marker).
 */
export function backlogItemTemplate(): string {
  return [
    "Short headline",
    "",
    "Problem (date + how to reproduce):",
    "...",
    "",
    "Files/symbols (exact paths):",
    "src/...",
    "",
    "Fix direction:",
    "...",
    "",
    "Acceptance / how to verify:",
    "...",
    "",
    "Related (memories, commits, reviews):",
    "...",
  ].join("\n");
}

/**
 * The input box at the top of the Backlog tab: textarea + image paste/drop,
 * thumbnails with remove buttons. Enter (no shift) adds to the backlog.
 */
function BacklogInput() {
  // The draft text + attached images live in the store (not useState) so
  // switching away from the Backlog tab — which unmounts this component via
  // the conditional render in RightPanel.tsx — keeps a half-typed prompt.
  const text = useAgentStore((s) => s.backlogDraft);
  const setText = useAgentStore((s) => s.setBacklogDraft);
  const attachedImages = useAgentStore((s) => s.backlogDraftImages);
  const setAttachedImages = useAgentStore((s) => s.setBacklogDraftImages);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // The error shown under the input when the backend rejects an add
  // (backlog 45a4eb88: shape-less text — the message names the required
  // shape). Cleared on the next attempt and when the draft is edited.
  const [addError, setAddError] = useState<string | null>(null);
  // Set by handleTemplate; the effect below applies focus + selection
  // AFTER React commits the template text (review LOW-1, round 1: a
  // synchronous setSelectionRange runs before the commit — React 18
  // batches the state update, so the selection clamps to the old (empty)
  // value and the caret lands at the end once the new value lands).
  const [templateJustInserted, setTemplateJustInserted] = useState(false);

  useEffect(() => {
    if (!templateJustInserted) return;
    setTemplateJustInserted(false);
    const el = textareaRef.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(0, "Short headline".length);
  }, [templateJustInserted]);

  // Auto-resize the textarea. overflowY switches to "auto" only once the
  // height clamps at the cap — below it, the border-box shortfall would
  // otherwise render a permanent (phantom) scrollbar thumb.
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    const { heightPx, overflowY } = autosizeForScrollHeight(ta.scrollHeight, 160);
    ta.style.height = `${heightPx}px`;
    ta.style.overflowY = overflowY;
  }, [text]);

  /** Add image files (from paste or drop) to the attached-images list. */
  const addImageFiles = useCallback(async (files: File[]) => {
    const imageFiles = files.filter((f) => f.type.startsWith("image/"));
    if (imageFiles.length === 0) return;
    const dataUrls = await Promise.all(imageFiles.map(fileToAttachedDataUrl));
    // Read the latest draft from the store (not the render closure) so rapid
    // successive pastes/drops can't lose images to a stale snapshot.
    const store = useAgentStore.getState();
    store.setBacklogDraftImages([...store.backlogDraftImages, ...dataUrls]);
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
    const store = useAgentStore.getState();
    store.setBacklogDraftImages(store.backlogDraftImages.filter((_, i) => i !== index));
  }

  async function handleAdd() {
    if (!text.trim() && attachedImages.length === 0) return;
    const input = text;
    const images = attachedImages;
    setText("");
    setAttachedImages([]);
    setAddError(null);
    try {
      await backlogAdd(input, images);
    } catch (e) {
      console.error("failed to add backlog item:", e);
      // Surface the rejection inline (backlog 45a4eb88) — the backend
      // rejects shape-less text with an error naming the required shape.
      // Restore the draft so the user can fix it: the optimistic clear
      // above would otherwise eat the text they typed — but only what is
      // STILL empty: anything typed or pasted while the add was in flight
      // (a slow non-validation failure) wins over the rejected draft
      // (review LOW-1).
      const s = useAgentStore.getState();
      if (!s.backlogDraft) setText(input);
      if (s.backlogDraftImages.length === 0) setAttachedImages(images);
      setAddError(errMsg(e));
    }
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleAdd();
    }
  }

  /**
   * Insert the item template (2027-01-07 detail bar) — only into an empty
   * composer, so a click never clobbers typed text (the button is also
   * disabled then; this is the double guard). Focus + headline selection
   * are applied by the templateJustInserted effect after the commit.
   */
  function handleTemplate() {
    if (text.trim()) return;
    setText(backlogItemTemplate());
    setAddError(null);
    setTemplateJustInserted(true);
  }

  const canAdd = text.trim() || attachedImages.length > 0;

  return (
    <div className="border-b border-border p-2">
      {/* Attached images — thumbnails with a remove button. */}
      {attachedImages.length > 0 && (
        <div className="mb-2 flex flex-wrap gap-2">
          {attachedImages.map((dataUrl, i) => (
            <div
              key={i}
              className="group relative h-14 w-14 overflow-hidden rounded-lg border border-border bg-bg-primary"
            >
              <img
                src={dataUrl}
                alt={`attachment ${i + 1}`}
                className="h-full w-full object-cover"
              />
              <button
                onClick={() => removeImage(i)}
                className="absolute right-0.5 top-0.5 flex h-4 w-4 items-center justify-center rounded bg-black/60 text-white opacity-0 transition-opacity group-hover:opacity-100"
                title="Remove image"
              >
                <X className="h-2.5 w-2.5" />
              </button>
            </div>
          ))}
        </div>
      )}
      <div
        className="flex items-end gap-2"
        onDrop={handleDrop}
        onDragOver={handleDragOver}
      >
        <textarea
          ref={textareaRef}
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            setAddError(null);
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          placeholder="Add to backlog — Enter to queue, Shift+Enter for newline, paste images..."
          rows={1}
          className="flex-1 resize-none rounded-lg border border-border bg-bg-primary px-3 py-2 text-[0.875em] text-slate-200 placeholder-slate-500 focus:border-cyan-500 focus:outline-none"
        />
        {attachedImages.length > 0 && (
          <div
            className="flex h-8 items-center gap-1 rounded-lg bg-cyan-600/20 px-2 text-[0.75em] text-cyan-400"
            title={`${attachedImages.length} image${attachedImages.length > 1 ? "s" : ""} attached`}
          >
            <ImageIcon className="h-3.5 w-3.5" />
            {attachedImages.length}
          </div>
        )}
        <button
          onClick={handleTemplate}
          disabled={!!text.trim()}
          className="flex h-8 w-8 items-center justify-center rounded-lg border border-border text-slate-400 transition-colors hover:border-cyan-500 hover:text-cyan-400 disabled:opacity-40"
          title="Insert the item template (problem, files, fix direction, acceptance, related)"
        >
          <LayoutTemplate className="h-4 w-4" />
        </button>
        <button
          onClick={handleAdd}
          disabled={!canAdd}
          className="flex h-8 w-8 items-center justify-center rounded-lg bg-cyan-600 text-white transition-colors hover:bg-cyan-500 disabled:opacity-40"
          title="Add to backlog"
        >
          <Plus className="h-4 w-4" />
        </button>
      </div>
      {/* The backend's rejection (e.g. shape-less text) shown under the
          input — the message names the required shape, so it doubles as
          the fix-it hint. */}
      {addError && (
        <p className="mt-1 text-[0.75em] text-red-400" role="alert">
          {addError}
        </p>
      )}
    </div>
  );
}

/**
 * Split a backlog item's text into headline + body (backlog 40763a24): the
 * first line is the headline — the same convention the backlog_add ToolCard
 * chip uses (backlog fef458f1) — and everything after the first newline is
 * the body. Items without a newline are headline-only (empty body). The
 * split is display-only; the editor and copy button keep using the raw
 * text.
 */
export function splitHeadline(text: string): { headline: string; body: string } {
  const idx = text.search(/\r?\n/);
  if (idx === -1) {
    return { headline: text.trim(), body: "" };
  }
  return {
    headline: text.slice(0, idx).trim(),
    body: text.slice(idx).replace(/^\r?\n/, ""),
  };
}

/**
 * Move `id` to the front of a backlog id order (the "send to top" action,
 * user request 2027-01-06): returns a NEW array with the id first and the
 * rest in their existing relative order. Already-first and absent ids
 * return an equal order (a stale-render click is a harmless no-op reorder).
 */
export function moveIdToFront(ids: string[], id: string): string[] {
  const at = ids.indexOf(id);
  if (at <= 0) return ids;
  return [id, ...ids.slice(0, at), ...ids.slice(at + 1)];
}

/**
 * Move `id` to the back of a backlog id order (the "send to bottom"
 * action, user request 2027-01-07): returns a NEW array with the id last
 * and the rest in their existing relative order. Already-last and absent
 * ids return an equal order (a stale-render click is a harmless no-op
 * reorder).
 */
export function moveIdToBack(ids: string[], id: string): string[] {
  const at = ids.indexOf(id);
  if (at < 0 || at === ids.length - 1) return ids;
  return [...ids.slice(0, at), ...ids.slice(at + 1), id];
}

/** The agent to switch to when the plan chip is clicked (backlog
 * f45513b2): the MAIN agent while the item is in flight (backlog items
 * are main-agent-dispatched — the working agent), else `null` (inert: no
 * agent is actively working the plan). Extracted so the decision is
 * unit-testable — the node test harness cannot fire onClick. */
export function planChipTarget(
  status: BacklogStatus,
  mainAgentId: AgentId | null,
): AgentId | null {
  return status === "in_flight" ? mainAgentId : null;
}

/** A single backlog item card. */
export function BacklogItemCard({
  item,
  index,
  total,
}: {
  item: BacklogItem;
  index: number;
  total: number;
}) {
  // Headline/body split (backlog 40763a24): first line = headline, rest =
  // collapsible body. Long bodies collapse by default; short ones start
  // expanded. Display-only — the editor and copy button keep using the
  // raw text.
  const { headline, body } = splitHeadline(item.text);
  const hasBody = body.trim().length > 0;
  const isLongBody = body.length > 200;
  const [expanded, setExpanded] = useState(!isLongBody);
  const backlog = useAgentStore((s) => s.backlog);
  // The backlog dispatches to the MAIN agent — track its id explicitly (the
  // smallest-id parentless agent) rather than assuming it's the active one, so
  // the phase badge stays live even while the user is viewing a subagent. The
  // selector reads the maps inline (not via the mainAgentId action) to stay
  // reactive to agentParents/workflowStates changes.
  const workflowState = useAgentStore((s) => {
    const mainId = selectMainAgentId(s.agentParents, s.agents);
    return mainId !== null ? s.workflowStates[mainId] ?? null : null;
  });

  // For the in-flight item, mirror the main agent's live workflow phase
  // (planning/executing/complete) instead of a static "in-flight" label.
  const isInFlight = item.status === "in_flight";
  const badgeLabel = isInFlight && workflowState ? workflowState : item.status.replace("_", "-");
  const badgeStyle = isInFlight && workflowState
    ? (PHASE_STYLES[workflowState] ?? STATUS_STYLES.in_flight)
    : STATUS_STYLES[item.status];

  // The working agent for backlog-dispatched items is the MAIN agent —
  // bound reactively (the same selector the workflowState above uses) so
  // the plan chip's click can switch to its chat tab (backlog f45513b2).
  const mainAgentId = useAgentStore((s) => selectMainAgentId(s.agentParents, s.agents));
  const setActiveAgent = useAgentStore((s) => s.setActiveAgent);
  // The chip's click target: the main agent while in flight, else null
  // (inert). See planChipTarget for the rationale.
  const chipTarget = planChipTarget(item.status, mainAgentId);

  // Note display (backlog f45513b2): strip the checkpoint-sha head — the
  // sha is shown in the copyable checkpoint detail below, not as the
  // note's headline. A note that is exactly the sha renders no note block.
  const noteHead = item.checkpoint_sha ?? "";
  const displayNote =
    item.note !== null && noteHead !== "" && item.note.startsWith(noteHead)
      ? item.note.slice(noteHead.length).replace(/^\s*\|\s*/, "")
      : (item.note ?? "");

  // Copy-to-clipboard feedback state. The reset timer is tracked so it can be
  // cleared on unmount (and on a fresh copy) — no setState on an unmounted
  // component, no overlapping timers.
  const [copied, setCopied] = useState(false);
  const copyResetTimer = useRef<number | null>(null);

  /** Copy the item's prompt text to the clipboard, with a 2s check feedback.
   * Only shows the check when the write actually succeeded. */
  function handleCopy() {
    navigator.clipboard.writeText(item.text).then(
      () => {
        setCopied(true);
        if (copyResetTimer.current !== null) window.clearTimeout(copyResetTimer.current);
        copyResetTimer.current = window.setTimeout(() => setCopied(false), 2000);
      },
      () => {
        // Clipboard write rejected (permission denied / non-secure context) —
        // don't show a false "copied" checkmark.
      }
    );
  }

  // Checkpoint-sha copy feedback (backlog f45513b2): the same 2s pattern
  // as the prompt-text copy above.
  const [checkpointCopied, setCheckpointCopied] = useState(false);
  const checkpointCopyTimer = useRef<number | null>(null);

  /** Copy the pre-work checkpoint sha (the manual resume/rollback anchor). */
  function handleCopyCheckpoint() {
    const sha = item.checkpoint_sha;
    if (!sha) return;
    navigator.clipboard.writeText(sha).then(
      () => {
        setCheckpointCopied(true);
        if (checkpointCopyTimer.current !== null) window.clearTimeout(checkpointCopyTimer.current);
        checkpointCopyTimer.current = window.setTimeout(() => setCheckpointCopied(false), 2000);
      },
      () => {
        // Clipboard write rejected — don't show a false "copied" label.
      }
    );
  }

  // Clear the copy reset timer on unmount (no setState on an unmounted card).
  useEffect(() => {
    return () => {
      if (copyResetTimer.current !== null) {
        window.clearTimeout(copyResetTimer.current);
        copyResetTimer.current = null;
      }
    };
  }, []);

  // Inline editor state. Only pending items can be edited (an in-flight or
  // finished item's text is the record of what was actually dispatched).
  const canEdit = item.status === "pending";
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState(item.text);
  const [editImages, setEditImages] = useState(item.images);
  const editRef = useRef<HTMLTextAreaElement>(null);

  // Keep the editor text + image strip in sync with the item when not
  // actively editing, so opening the editor always starts from the latest
  // persisted state (including images edited from elsewhere).
  useEffect(() => {
    if (!editing) {
      setEditText(item.text);
      setEditImages(item.images);
    }
  }, [item.text, item.images, editing]);

  // Auto-resize the editor textarea to fit its content. overflowY switches
  // to "auto" only once the height clamps at the cap — below it, the
  // border-box shortfall would otherwise render a permanent (phantom)
  // scrollbar thumb.
  useEffect(() => {
    if (!editing) return;
    const ta = editRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    const { heightPx, overflowY } = autosizeForScrollHeight(ta.scrollHeight, 240);
    ta.style.height = `${heightPx}px`;
    ta.style.overflowY = overflowY;
  }, [editing, editText]);

  // Focus the editor when it opens.
  useEffect(() => {
    if (editing) editRef.current?.focus();
  }, [editing]);

  /** Add image files (paste/drop during edit) to the editor's image list.
   * Reads the latest state via the setter callback so rapid successive
   * pastes/drops can't lose images to a stale snapshot. */
  const addEditImageFiles = useCallback(async (files: File[]) => {
    const imageFiles = files.filter((f) => f.type.startsWith("image/"));
    if (imageFiles.length === 0) return;
    const dataUrls = await Promise.all(imageFiles.map(fileToAttachedDataUrl));
    setEditImages((prev) => [...prev, ...dataUrls]);
  }, []);

  /** Remove an attached image from the editor strip by index. */
  function removeEditImage(index: number) {
    setEditImages((prev) => prev.filter((_, i) => i !== index));
  }

  /** Paste handler for the editor — detects image files in the clipboard. */
  function handleEditPaste(e: React.ClipboardEvent) {
    const files = Array.from(e.clipboardData.files);
    if (files.some((f) => f.type.startsWith("image/"))) {
      e.preventDefault();
      void addEditImageFiles(files);
    }
  }

  /** Drop handler for the editor — accepts image files dropped onto it. */
  function handleEditDrop(e: React.DragEvent) {
    e.preventDefault();
    const files = Array.from(e.dataTransfer.files);
    void addEditImageFiles(files);
  }

  function handleEditDragOver(e: React.DragEvent) {
    e.preventDefault();
  }

  async function handleSaveEdit() {
    // Persist the edited text AND the edited image set (user request
    // 2026-09-11: "For items in the backlog I can't edit the images or add
    // new screenshots" — the editor used to pass item.images unchanged, so
    // removed/added images never saved). Image-only items stay valid.
    if (!editText.trim() && editImages.length === 0) return;
    try {
      await backlogEdit(item.id, editText, editImages);
      setEditing(false);
    } catch (e) {
      console.error("failed to edit backlog item:", e);
    }
  }

  function handleCancelEdit() {
    setEditText(item.text);
    setEditImages(item.images);
    setEditing(false);
  }

  async function handleRemove() {
    try {
      await backlogRemove(item.id);
    } catch (e) {
      console.error("failed to remove backlog item:", e);
    }
  }

  async function handleDispatch() {
    try {
      await backlogDispatchItem(item.id);
    } catch (e) {
      console.error("failed to dispatch backlog item:", e);
    }
  }

  /** Move this item up/down by one, then push the full id order. */
  async function handleMove(direction: -1 | 1) {
    const ids = backlog.map((b) => b.id);
    const target = index + direction;
    if (target < 0 || target >= ids.length) return;
    [ids[index], ids[target]] = [ids[target], ids[index]];
    try {
      await backlogReorder(ids);
    } catch (e) {
      console.error("failed to reorder backlog:", e);
    }
  }

  /** Send this item to the top of the queue (the next dispatched pending
   * item) — the one-click version of repeated move-ups. */
  async function handleSendToTop() {
    const ids = moveIdToFront(
      backlog.map((b) => b.id),
      item.id,
    );
    try {
      await backlogReorder(ids);
    } catch (e) {
      console.error("failed to reorder backlog:", e);
    }
  }

  /** Send this item to the bottom of the queue — the one-click version of
   * repeated move-downs (user request 2027-01-07). */
  async function handleSendToBottom() {
    const ids = moveIdToBack(
      backlog.map((b) => b.id),
      item.id,
    );
    try {
      await backlogReorder(ids);
    } catch (e) {
      console.error("failed to reorder backlog:", e);
    }
  }

  /** Re-queue a finished item back to pending: retry a failed/cant_resolve
   * item, or reset an item the agent marked done that the user disagrees
   * with (user request 2026-08-20: "we need a way for me to reset tasks you
   * mark as done"). Never touches git history — re-dispatch builds on top of
   * it. The backend (`BacklogStore::requeue`) accepts only terminal statuses
   * and refuses pending/in-flight items, so a stale click is a safe no-op. */
  async function handleRetry() {
    try {
      await backlogRetry(item.id);
    } catch (e) {
      console.error("failed to retry backlog item:", e);
    }
  }

  const canRetry =
    item.status === "failed" ||
    item.status === "cant_resolve" ||
    item.status === "done";

  return (
    <div className="rounded-lg border border-border bg-bg-tertiary p-2.5">
      {/* Meta row: timestamp + controls (the status chip moved down beside
          the headline — backlog 40763a24). */}
      <div className="mb-1 flex items-center gap-1.5">
        <span className="text-[0.7em] text-slate-500">
          {formatTimestamp(item.created_at)}
        </span>
        <div className="ml-auto flex items-center gap-0.5">
          {canRetry && (
            <button
              onClick={handleRetry}
              className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-cyan-400"
              title={
                item.status === "done"
                  ? "Reset to pending — re-run this task (does not touch git history)"
                  : "Re-queue (retry)"
              }
            >
              <RotateCcw className="h-3.5 w-3.5" />
            </button>
          )}
          {canEdit && !editing && (
            <button
              onClick={() => setEditing(true)}
              className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-cyan-400"
              title="Edit"
            >
              <Pencil className="h-3.5 w-3.5" />
            </button>
          )}
          <button
            onClick={handleCopy}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-cyan-400"
            title="Copy text to clipboard"
          >
            {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
          </button>
          {item.status === "pending" && (
            <button
              onClick={handleDispatch}
              className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-cyan-400"
              title="Dispatch this item to main agent (when idle)"
            >
              <Play className="h-3.5 w-3.5" />
            </button>
          )}
          {/* Deferred (skip Run-All) toggle (user request 2027-01-07):
              lives in the card toolbar (moved from beside the status chip,
              same-day user request). Orthogonal to status — the item stays
              pending and visible; only Run-All selection skips it (the ▶
              button still dispatches it). */}
          <input
            type="checkbox"
            checked={item.deferred ?? false}
            onChange={(e) => {
              void backlogSetDeferred(item.id, e.target.checked).catch(
                (err) => console.error("failed to toggle deferred:", err)
              );
            }}
            className="mx-0.5 h-3 w-3 cursor-pointer accent-violet-500"
            title="Deferred (skip Run-All) — excluded from Run-All dispatch; still dispatchable manually"
            aria-label="Deferred (skip Run-All)"
          />
          <button
            onClick={handleSendToTop}
            disabled={index === 0}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200 disabled:opacity-30"
            title="Send to top"
          >
            <ArrowUpToLine className="h-3.5 w-3.5" />
          </button>
          <button
            onClick={() => handleMove(-1)}
            disabled={index === 0}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200 disabled:opacity-30"
            title="Move up"
          >
            <ChevronUp className="h-3.5 w-3.5" />
          </button>
          <button
            onClick={() => handleMove(1)}
            disabled={index === total - 1}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200 disabled:opacity-30"
            title="Move down"
          >
            <ChevronDown className="h-3.5 w-3.5" />
          </button>
          <button
            onClick={handleSendToBottom}
            disabled={index === total - 1}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200 disabled:opacity-30"
            title="Send to bottom"
          >
            <ArrowDownToLine className="h-3.5 w-3.5" />
          </button>
          <button
            onClick={handleRemove}
            className="rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-red-400"
            title="Remove"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      {/* Headline + collapsible body — or the inline editor when editing */}
      {editing ? (
        <div
          className="mt-1"
          onPaste={handleEditPaste}
          onDrop={handleEditDrop}
          onDragOver={handleEditDragOver}
        >
          <textarea
            ref={editRef}
            value={editText}
            onChange={(e) => setEditText(e.target.value)}
            onKeyDown={(e) => {
              // Enter saves (Shift+Enter for newline), Escape cancels.
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void handleSaveEdit();
              } else if (e.key === "Escape") {
                e.preventDefault();
                handleCancelEdit();
              }
            }}
            rows={2}
            className="w-full resize-none rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-[0.8em] text-slate-200 placeholder-slate-500 focus:border-cyan-500 focus:outline-none"
            placeholder="Edit the prompt… (paste or drop images to attach)"
          />
          {/* Editor image strip — existing + newly pasted/dropped attachments,
              each removable. Mirrors BacklogInput's thumbnails. */}
          {editImages.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1.5">
              {editImages.map((dataUrl, i) => (
                <div
                  key={i}
                  className="group relative h-14 w-14 overflow-hidden rounded-lg border border-border bg-bg-primary"
                >
                  <img
                    src={dataUrl}
                    alt={`attachment ${i + 1}`}
                    className="h-full w-full object-cover"
                  />
                  <button
                    onClick={() => removeEditImage(i)}
                    className="absolute right-0.5 top-0.5 flex h-4 w-4 items-center justify-center rounded bg-black/60 text-white opacity-0 transition-opacity group-hover:opacity-100"
                    title="Remove image"
                  >
                    <X className="h-2.5 w-2.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="mt-1 flex items-center justify-end gap-1">
            <button
              onClick={handleCancelEdit}
              className="rounded px-2 py-0.5 text-[0.7em] text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200"
              title="Cancel (Escape)"
            >
              Cancel
            </button>
            <button
              onClick={handleSaveEdit}
              disabled={!editText.trim() && editImages.length === 0}
              className="flex items-center gap-1 rounded bg-cyan-600 px-2 py-0.5 text-[0.7em] text-white transition-colors hover:bg-cyan-500 disabled:opacity-40"
              title="Save (Enter)"
            >
              <Check className="h-3 w-3" />
              Save
            </button>
          </div>
        </div>
      ) : (
        <>
          {/* Headline row: status chip beside the headline + rotating
              chevron for the collapsible body (backlog 40763a24) — the
              plan-steps pattern (PlanStepRow). Body-less items keep the
              headline aligned with a spacer instead of the chevron. */}
          <div className="flex items-start gap-1.5">
            <span
              className={`mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[0.65em] font-medium uppercase tracking-wide ${badgeStyle}`}
            >
              {badgeLabel}
            </span>
            {item.deferred ? (
              <span className="mt-0.5 shrink-0 rounded bg-violet-950/60 px-1.5 py-0.5 text-[0.65em] font-medium uppercase tracking-wide text-violet-400">
                Deferred
              </span>
            ) : null}
            {hasBody ? (
              <button
                type="button"
                onClick={() => setExpanded((v) => !v)}
                aria-expanded={expanded}
                className="flex min-w-0 flex-1 items-start gap-1 text-left"
                title={expanded ? "Collapse details" : "Expand details"}
              >
                <ChevronRight
                  className={`mt-0.5 h-3.5 w-3.5 shrink-0 text-slate-500 transition-transform ${
                    expanded ? "rotate-90" : ""
                  }`}
                />
                <span className="break-words text-[0.9em] font-semibold leading-snug text-slate-100">
                  {headline}
                </span>
              </button>
            ) : (
              <div className="flex min-w-0 flex-1 items-start gap-1">
                <span className="mt-0.5 block h-3.5 w-3.5 shrink-0" />
                <span className="break-words text-[0.9em] font-semibold leading-snug text-slate-100">
                  {headline}
                </span>
              </div>
            )}
          </div>

          {/* Plan chip (backlog f45513b2): the working plan's TITLE as the
              primary identifier + the short plan-id chip (the same 8-char
              prefix the tool layer reports, so user and agent references
              align) — replacing the raw checkpoint sha as the status line's
              identifier. Clickable while the item is in flight: switches to
              the working agent's chat tab (the main agent — backlog items
              are main-agent-dispatched). Inert otherwise: the plan panel
              only shows the active agent's live plan, so it can't display
              done/failed plans. */}
          {item.plan_id && (
            <div className="mt-1 flex min-w-0 items-center gap-1.5">
              <button
                type="button"
                onClick={chipTarget !== null ? () => setActiveAgent(chipTarget) : undefined}
                className={`flex min-w-0 max-w-full items-center gap-1.5 rounded border px-1.5 py-0.5 text-left text-[0.7em] ${
                  chipTarget !== null
                    ? "cursor-pointer border-cyan-800/60 bg-cyan-950/40 text-cyan-200 hover:border-cyan-600"
                    : "cursor-default border-border bg-bg-primary text-slate-400"
                }`}
                title={
                  chipTarget !== null
                    ? "Switch to the working agent's chat tab"
                    : "The plan dispatched for this item (no agent currently working it)"
                }
              >
                {item.plan_title && (
                  <span className="truncate font-medium">{item.plan_title}</span>
                )}
                <span className="shrink-0 rounded bg-bg-primary/80 px-1 font-mono text-[0.9em] text-slate-400">
                  {item.plan_id.slice(0, 8)}
                </span>
              </button>
            </div>
          )}

          {/* Body — the markdown stack (backlog 991942af), collapsed by
              default when long (backlog 40763a24). */}
          {hasBody && expanded && (
            <div className="prose prose-invert prose-sm mt-1 max-w-none break-words text-[0.8em] text-slate-300 prose-p:my-1 prose-pre:m-0 prose-pre:bg-bg-primary">
              <Markdown remarkPlugins={[remarkBreaks]}>{body}</Markdown>
            </div>
          )}
        </>
      )}

      {/* Image thumbnails (display mode only — while editing, the removable
          editor strip above is the single source of truth, so removed images
          disappear immediately instead of lingering until save). */}
      {!editing && item.images.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1.5">
          {item.images.map((dataUrl, i) => (
            <div
              key={i}
              className="h-10 w-10 overflow-hidden rounded border border-border bg-bg-primary"
            >
              <img
                src={dataUrl}
                alt={`attachment ${i + 1}`}
                className="h-full w-full object-cover"
              />
            </div>
          ))}
        </div>
      )}

      {/* Note (for failed / cant_resolve — and the run-all dispatch
          annotations). The checkpoint-sha head is stripped (backlog
          f45513b2): the sha lives in the copyable checkpoint detail
          below, never as the note's headline. */}
      {displayNote.trim().length > 0 && (
        <div className="mt-2 whitespace-pre-wrap break-words rounded border border-border bg-bg-primary px-2 py-1 text-[0.75em] text-slate-400">
          {displayNote}
        </div>
      )}

      {/* Checkpoint detail (backlog f45513b2): the pre-item git checkpoint
          sha — the manual resume/rollback anchor — demoted from the
          headline to a copyable detail. */}
      {item.checkpoint_sha && (
        <div className="mt-1 flex items-center gap-1 text-[0.65em] text-slate-500">
          <span className="truncate font-mono" title={item.checkpoint_sha}>
            pre-work checkpoint {item.checkpoint_sha.slice(0, 8)}…
          </span>
          <button
            type="button"
            onClick={handleCopyCheckpoint}
            className="shrink-0 rounded px-1 text-slate-500 hover:bg-bg-primary hover:text-slate-300"
            title="Copy the full checkpoint sha (resume/rollback anchor)"
          >
            {checkpointCopied ? "copied" : "copy"}
          </button>
        </div>
      )}

    </div>
  );
}

/** The Backlog tab content. */
export function BacklogView() {
  const backlog = useAgentStore((s) => s.backlog);
  const autoFeed = useAgentStore((s) => s.autoFeed);
  const parallelRunAll = useAgentStore((s) => s.parallelRunAll);
  const runAll = useAgentStore((s) => s.runAll);
  const safetyMode = useAgentStore((s) => s.safetyMode);

  async function handleToggleAutoFeed(enabled: boolean) {
    try {
      await backlogSetAutoFeed(enabled);
    } catch (e) {
      console.error("failed to set auto-feed:", e);
    }
  }

  async function handleToggleParallelRunAll(enabled: boolean) {
    try {
      await backlogSetParallelRunAll(enabled);
    } catch (e) {
      console.error("failed to set parallel run-all:", e);
    }
  }

  async function handleRunAll() {
    const strictMode =
      safetyMode === "approve-each-action" ||
      safetyMode === "auto-read-approve-writes";
    const warning =
      "Run All starts unattended processing of every pending backlog item (one plan/execute turn each, with a git checkpoint per item; deferred items are skipped). The loop respects your current safety mode and never changes it." +
      "\n\n⚠️ A failed item is rolled back to its checkpoint with `git reset --hard`, which DISCARDS any uncommitted changes made during that item's turn. Make sure your working tree is clean (or committed) before starting." +
      (strictMode
        ? "\n\nIn strict modes the loop stops at the first approval request (current item finishes its turn)."
        : "") +
      (parallelRunAll
        ? "\n\nParallel mode: items beyond the first run concurrently, each in its own git worktree on its own branch; finished branches merge into main automatically (a merge conflict keeps the branch for manual resolution)."
        : "");
    if (!window.confirm(warning)) return;
    try {
      // Parallel run-all (plan ffd7a86f): the checkbox gates the
      // concurrency — 3 lanes when on, sequential (today's behavior)
      // when off.
      await backlogRunAll(parallelRunAll ? 3 : undefined);
    } catch (e) {
      console.error("failed to start run-all:", e);
    }
  }

  async function handleStopAll() {
    try {
      await backlogStopAll();
    } catch (e) {
      console.error("failed to stop run-all:", e);
    }
  }

  async function handleClearFinished() {
    try {
      await backlogClearFinished();
    } catch (e) {
      console.error("failed to clear finished:", e);
    }
  }

  const hasFinished = backlog.some(
    (b) => b.status === "done" || b.status === "failed" || b.status === "cant_resolve"
  );

  return (
    <div className="flex h-full flex-col">
      <BacklogInput />

      {/* Toolbar */}
      <div className="flex items-center gap-2 border-b border-border px-2 py-1.5">
        {/* Auto-feed toggle */}
        <label
          className="flex cursor-pointer items-center gap-1.5 text-[0.75em] text-slate-400"
          title="When the main agent is idle, automatically dispatch the top pending backlog item"
        >
          <input
            type="checkbox"
            checked={autoFeed}
            onChange={(e) => handleToggleAutoFeed(e.target.checked)}
            className="h-3.5 w-3.5 accent-cyan-500"
          />
          auto-feed
        </label>

        {/* Parallel run-all toggle (plan ffd7a86f) */}
        <label
          className="flex cursor-pointer items-center gap-1.5 text-[0.75em] text-slate-400"
          title="Run-All dispatches items concurrently: item 1 on the main agent, items 2+ each in their own git worktree on their own branch (merged into main when they finish; a merge conflict keeps the branch for manual resolution). Auto-feed stays sequential."
        >
          <input
            type="checkbox"
            checked={parallelRunAll}
            onChange={(e) => handleToggleParallelRunAll(e.target.checked)}
            className="h-3.5 w-3.5 accent-cyan-500"
          />
          parallel
        </label>

        <div className="ml-auto flex items-center gap-1.5">
          {/* Run-All progress */}
          {runAll.active && (
            <span
              className="text-[0.75em] text-cyan-400"
              title="Run-All progress: resolved items / total items that were pending when the run started"
            >
              Run-All {runAll.done}/{runAll.total}
              {runAll.compacting && " · compacting…"}
              {runAll.spawned.length > 0 &&
                ` · ${runAll.spawned.length} parallel`}
            </span>
          )}
          {runAll.active ? (
            <button
              onClick={handleStopAll}
              className="flex items-center gap-1 rounded bg-red-600/80 px-2 py-1 text-[0.75em] text-white transition-colors hover:bg-red-500"
              title="Stop Run-All (graceful: the current in-flight item finishes its turn, then no more are dispatched)"
            >
              Stop
            </button>
          ) : (
            <button
              onClick={handleRunAll}
              disabled={backlog.filter((b) => b.status === "pending").length === 0}
              className="flex items-center gap-1 rounded bg-cyan-600 px-2 py-1 text-[0.75em] text-white transition-colors hover:bg-cyan-500 disabled:opacity-40"
              title="Run All: entry point for unattended processing of all pending backlog items (git checkpoint per item; respects safety)"
            >
              <Play className="h-3.5 w-3.5" />
              Run All
            </button>
          )}
          {runAll.note && (
            <span
              className="text-[0.75em] text-amber-400"
              title="Knowledge records written during the run — uncommitted in the main project tree (see the Memory tab)"
            >
              {runAll.note}
            </span>
          )}
          {hasFinished && (
            <button
              onClick={handleClearFinished}
              className="flex items-center gap-1 rounded border border-border px-2 py-1 text-[0.75em] text-slate-400 transition-colors hover:bg-bg-tertiary hover:text-slate-200"
              title="Remove all finished (done/failed/can't-resolve) items"
            >
              <Trash2 className="h-3 w-3" />
              clear finished
            </button>
          )}
        </div>
      </div>

      {/* Item list */}
      <div className="flex-1 overflow-y-auto p-2">
        {backlog.length === 0 ? (
          <div className="flex h-full items-center justify-center text-[0.875em] text-slate-500">
            No backlog items — type below to add one.
          </div>
        ) : (
          <div className="space-y-2.5">
            {backlog.map((item, i) => (
              <BacklogItemCard
                key={item.id}
                item={item}
                index={i}
                total={backlog.length}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

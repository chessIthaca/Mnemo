// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import { useAgentStore } from "../../../hooks/useAgentStore";
import { saveSettings, errMsg } from "../../../lib/tauri";
import { type ChatDraft, serializeChat } from "../types";
import type { SettingsSectionHandle } from "../types";

export interface ChatSectionProps {
  /** When true, capture a snapshot from the store (dialog open). */
  active: boolean;
  /** Report dirty so the shell can confirm discard on close. */
  onDirtyChange: (dirty: boolean) => void;
}

/**
 * Chat section — chat display toggles (token usage in the activity bar,
 * tool activity cards in the transcript). These were moved out of
 * Appearance: they gate what the chat surface shows, not visual styling.
 *
 * Draft model like Appearance, but with no live CSS preview — the toggles
 * write localStorage / config.toml only on Save, so discarding is a no-op.
 */
export const ChatSection = forwardRef<SettingsSectionHandle, ChatSectionProps>(
  function ChatSection({ active, onDirtyChange }, ref) {
  const [draft, setDraft] = useState<ChatDraft | null>(null);
  const [snapshot, setSnapshot] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  function readDraftFromStore(): ChatDraft {
    const s = useAgentStore.getState();
    return {
      showTokenUsage: s.showTokenUsage,
      showToolImages: s.showToolImages,
      showToolActivity: s.showToolActivity,
      showKnowledgeActivity: s.showKnowledgeActivity,
      showDelegationNotes: s.showDelegationNotes,
      chatThreadLine: s.chatThreadLine,
      chatProseCap: s.chatProseCap,
      chatTurnTint: s.chatTurnTint,
      chatHoverTimestamps: s.chatHoverTimestamps,
    };
  }

  // Capture snapshot when Settings opens. No rollback on deactivate: the
  // draft has no live side effects (unlike Appearance's CSS preview).
  useEffect(() => {
    if (active) {
      const d = readDraftFromStore();
      setDraft(d);
      setSnapshot(serializeChat(d));
      setError(null);
      setOk(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const dirty = useMemo(() => {
    if (!draft || !snapshot) return false;
    return serializeChat(draft) !== snapshot;
  }, [draft, snapshot]);

  useEffect(() => {
    onDirtyChange(dirty);
  }, [dirty, onDirtyChange]);

  function patch(p: Partial<ChatDraft>) {
    setDraft((prev) => (prev ? { ...prev, ...p } : prev));
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      if (!draft) return false;
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        const s = useAgentStore.getState();
        s.setShowTokenUsage(draft.showTokenUsage);
        s.setShowToolImages(draft.showToolImages);
        s.setShowToolActivity(draft.showToolActivity);
        s.setShowKnowledgeActivity(draft.showKnowledgeActivity);
        s.setShowDelegationNotes(draft.showDelegationNotes);
        s.setChatThreadLine(draft.chatThreadLine);
        s.setChatProseCap(draft.chatProseCap);
        s.setChatTurnTint(draft.chatTurnTint);
        s.setChatHoverTimestamps(draft.chatHoverTimestamps);
        await saveSettings({
          show_token_usage: draft.showTokenUsage,
          show_tool_images: draft.showToolImages,
          show_tool_activity: draft.showToolActivity,
          show_knowledge_activity: draft.showKnowledgeActivity,
          show_delegation_notes: draft.showDelegationNotes,
          chat_thread_line: draft.chatThreadLine,
          chat_prose_cap: draft.chatProseCap,
          chat_turn_tint: draft.chatTurnTint,
          chat_hover_timestamps: draft.chatHoverTimestamps,
        });
        setSnapshot(serializeChat(draft));
        setOk(true);
        window.setTimeout(() => setOk(false), 2500);
        return true;
      } catch (e) {
        setError(errMsg(e));
        return false;
      } finally {
        setSaving(false);
      }
    },
  }));

  if (!draft) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading chat settings…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-3">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Chat
        </h3>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.showTokenUsage}
            onChange={(e) => patch({ showTokenUsage: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Show token usage in the activity bar
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.showToolImages}
            onChange={(e) => patch({ showToolImages: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Show images from image tools in chat
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.showToolActivity}
            onChange={(e) => patch({ showToolActivity: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Show tool activity in chat
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.showKnowledgeActivity}
            onChange={(e) => patch({ showKnowledgeActivity: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Show knowledge activity in chat (graph, memory, auto-recall)
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.showDelegationNotes}
            onChange={(e) => patch({ showDelegationNotes: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Show auto-delegation notes in search results
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.chatThreadLine}
            onChange={(e) => patch({ chatThreadLine: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Thread line along activity cards
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.chatProseCap}
            onChange={(e) => patch({ chatProseCap: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Cap prose width (~100 columns)
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.chatTurnTint}
            onChange={(e) => patch({ chatTurnTint: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Alternating turn background
        </label>

        <label className="flex items-center gap-2 text-sm text-[color:var(--text-primary)]">
          <input
            type="checkbox"
            checked={draft.chatHoverTimestamps}
            onChange={(e) => patch({ chatHoverTimestamps: e.target.checked })}
            className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
          />
          Timestamps on hover
        </label>
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Chat settings saved.
        </div>
      )}
    </div>
  );
  },
);

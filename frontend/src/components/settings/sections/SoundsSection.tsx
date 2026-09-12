// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import { useAgentStore } from "../../../hooks/useAgentStore";
import { playSound, type SoundKind } from "../../../lib/sounds";
import { saveSettings, errMsg } from "../../../lib/tauri";
import { type SoundDraft, serializeSound } from "../types";
import type { SettingsSectionHandle } from "../types";

export interface SoundsSectionProps {
  /** When true, capture a snapshot from the store (dialog open). */
  active: boolean;
  /** Report dirty so the shell can confirm discard on close. */
  onDirtyChange: (dirty: boolean) => void;
}

/**
 * Sounds section — the three notification-sound toggles, each disabled
 * independently (persisted to config.toml [ui]):
 *
 * - the ding when an agent's plan reaches Complete,
 * - the ping when an agent needs user input (approval request / question),
 * - the doom tone when repeated errors stop an agent (three consecutive
 *   errors — the backend MAX_RETRIES cap).
 *
 * Draft model like ChatSection: toggles write config only on Save, so
 * discarding is a no-op. Each row also has a ▶ Preview button that plays the
 * sound DIRECTLY — regardless of the enable flags — so users can hear what
 * they're toggling; the click is an explicit user gesture, which also
 * satisfies the browsers' AudioContext unlock requirement.
 */
export const SoundsSection = forwardRef<SettingsSectionHandle, SoundsSectionProps>(
  function SoundsSection({ active, onDirtyChange }, ref) {
    const [draft, setDraft] = useState<SoundDraft | null>(null);
    const [snapshot, setSnapshot] = useState("");
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [ok, setOk] = useState(false);

    function readDraftFromStore(): SoundDraft {
      const s = useAgentStore.getState();
      return {
        soundComplete: s.soundComplete,
        soundInput: s.soundInput,
        soundDoom: s.soundDoom,
      };
    }

    // Capture snapshot when Settings opens. No rollback on deactivate: the
    // draft has no live side effects (a preview click only plays audio; it
    // never writes config).
    useEffect(() => {
      if (active) {
        const d = readDraftFromStore();
        setDraft(d);
        setSnapshot(serializeSound(d));
        setError(null);
        setOk(false);
      }
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [active]);

    const dirty = useMemo(() => {
      if (!draft || !snapshot) return false;
      return serializeSound(draft) !== snapshot;
    }, [draft, snapshot]);

    useEffect(() => {
      onDirtyChange(dirty);
    }, [dirty, onDirtyChange]);

    function patch(p: Partial<SoundDraft>) {
      setDraft((prev) => (prev ? { ...prev, ...p } : prev));
    }

    /** One settings row: toggle + label + preview button. */
    function row(
      key: keyof SoundDraft,
      label: string,
      kind: SoundKind,
      previewTitle: string,
    ) {
      return (
        <div className="flex items-center gap-2">
          <label className="flex flex-1 items-center gap-2 text-sm text-[color:var(--text-primary)]">
            <input
              type="checkbox"
              checked={draft![key]}
              onChange={(e) => patch({ [key]: e.target.checked } as Partial<SoundDraft>)}
              className="h-3.5 w-3.5 accent-[color:var(--accent-color)]"
            />
            {label}
          </label>
          {/* Preview plays the sound regardless of the enable flags — the
              user is auditioning it, and the click doubles as the
              AudioContext unlock gesture. */}
          <button
            type="button"
            onClick={() => playSound(kind)}
            title={previewTitle}
            className="shrink-0 rounded border border-border px-1.5 py-0.5 text-[0.7em] text-[color:var(--text-muted)] hover:bg-bg-tertiary hover:text-[color:var(--text-primary)]"
          >
            ▶
          </button>
        </div>
      );
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
          s.setSoundComplete(draft.soundComplete);
          s.setSoundInput(draft.soundInput);
          s.setSoundDoom(draft.soundDoom);
          await saveSettings({
            sound_complete: draft.soundComplete,
            sound_input_needed: draft.soundInput,
            sound_stopped_errors: draft.soundDoom,
          });
          setSnapshot(serializeSound(draft));
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
          Loading sound settings…
        </div>
      );
    }

    return (
      <div className="space-y-5">
        <div className="space-y-3">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            Notification sounds
          </h3>

          {row("soundComplete", "Ding when an agent completes its plan", "complete", "Play the completion ding")}

          {row(
            "soundInput",
            "Ping when an agent needs your input (approval or question)",
            "input",
            "Play the needs-input ping",
          )}

          {row(
            "soundDoom",
            "Doom tone when repeated errors stop an agent",
            "doom",
            "Play the doom tone",
          )}

          <p className="text-xs text-[color:var(--text-muted)]">
            The doom tone plays when three consecutive errors abort the agent's
            turn (the model may be stuck). Previews play regardless of the
            toggles.
          </p>
        </div>

        {error && (
          <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
          </div>
        )}
        {ok && (
          <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
            Sound settings saved.
          </div>
        )}
      </div>
    );
  },
);

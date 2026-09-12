// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { Sun, Moon, Monitor, RotateCcw } from "lucide-react";
import { forwardRef, useEffect, useImperativeHandle, useMemo, useState } from "react";
import {
  useAgentStore,
  applyTheme,
  applyFontVars,
  applyColors,
  applyCodeColors,
} from "../../../hooks/useAgentStore";
import type { Theme } from "../../../hooks/useAgentStore";
import { saveSettings, errMsg } from "../../../lib/tauri";
import {
  FONT_OPTIONS,
  type AppearanceDraft,
  serializeAppearance,
} from "../types";
import type { SettingsSectionHandle } from "../types";

export interface AppearanceSectionProps {
  /** When true, capture a snapshot from the store (dialog open). */
  active: boolean;
  /** Report dirty so the shell can confirm discard on close. */
  onDirtyChange: (dirty: boolean) => void;
}

/**
 * Appearance section — draft model with live CSS preview.
 * Edits do not write localStorage / config.toml until Save. Closing Settings
 * with unsaved appearance changes restores the open-time snapshot.
 */
export const AppearanceSection = forwardRef<SettingsSectionHandle, AppearanceSectionProps>(
  function AppearanceSection({ active, onDirtyChange }, ref) {
  const [draft, setDraft] = useState<AppearanceDraft | null>(null);
  const [snapshot, setSnapshot] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState(false);

  function readDraftFromStore(): AppearanceDraft {
    const s = useAgentStore.getState();
    return {
      theme: s.theme,
      fontFamily: s.fontFamily,
      fontSize: s.fontSize,
      accentColor: s.accentColor,
      borderColor: s.borderColor,
      textPrimaryColor: s.textPrimaryColor,
      textMutedColor: s.textMutedColor,
      codeTextColor: s.codeTextColor,
      codeCommentColor: s.codeCommentColor,
      codeKeywordColor: s.codeKeywordColor,
      codeStringColor: s.codeStringColor,
      codeNumberColor: s.codeNumberColor,
      codeTitleColor: s.codeTitleColor,
      codeVariableColor: s.codeVariableColor,
    };
  }

  function applyPreview(d: AppearanceDraft) {
    applyTheme(d.theme);
    applyFontVars(d.fontFamily, d.fontSize);
    applyColors(d.accentColor, d.borderColor, d.textPrimaryColor, d.textMutedColor);
    applyCodeColors(
      d.codeTextColor,
      d.codeCommentColor,
      d.codeKeywordColor,
      d.codeStringColor,
      d.codeNumberColor,
      d.codeTitleColor,
      d.codeVariableColor,
    );
  }

  function commitDraft(d: AppearanceDraft) {
    const s = useAgentStore.getState();
    s.setTheme(d.theme);
    s.setFontFamily(d.fontFamily);
    s.setFontSize(d.fontSize);
    s.setAccentColor(d.accentColor);
    s.setBorderColor(d.borderColor);
    s.setTextPrimaryColor(d.textPrimaryColor);
    s.setTextMutedColor(d.textMutedColor);
    s.setCodeTextColor(d.codeTextColor);
    s.setCodeCommentColor(d.codeCommentColor);
    s.setCodeKeywordColor(d.codeKeywordColor);
    s.setCodeStringColor(d.codeStringColor);
    s.setCodeNumberColor(d.codeNumberColor);
    s.setCodeTitleColor(d.codeTitleColor);
    s.setCodeVariableColor(d.codeVariableColor);
  }

  // Capture snapshot when Settings opens; restore on deactivate if still dirty.
  useEffect(() => {
    if (active) {
      const d = readDraftFromStore();
      setDraft(d);
      setSnapshot(serializeAppearance(d));
      setError(null);
      setOk(false);
      return;
    }
    // Deactivating: if dirty, roll back CSS + leave store as-is (store was
    // never committed during draft). Re-apply committed store values.
    if (draft && snapshot && serializeAppearance(draft) !== snapshot) {
      const committed = readDraftFromStore();
      applyPreview(committed);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const dirty = useMemo(() => {
    if (!draft || !snapshot) return false;
    return serializeAppearance(draft) !== snapshot;
  }, [draft, snapshot]);

  useEffect(() => {
    onDirtyChange(dirty);
  }, [dirty, onDirtyChange]);

  function patch(p: Partial<AppearanceDraft>) {
    setDraft((prev) => {
      if (!prev) return prev;
      const next = { ...prev, ...p };
      applyPreview(next);
      return next;
    });
  }

  // Expose save to the dialog shell (OK button) via an imperative handle.
  useImperativeHandle(ref, () => ({
    save: async () => {
      if (!draft) return false;
      setSaving(true);
      setError(null);
      setOk(false);
      try {
        commitDraft(draft);
        await saveSettings({
          theme: draft.theme,
        });
        setSnapshot(serializeAppearance(draft));
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

  function handleResetDefaults() {
    // Draft-only defaults (do not touch localStorage until Save).
    const d: AppearanceDraft = {
      theme: "dark",
      fontFamily: "system-ui",
      fontSize: 14,
      accentColor: "#22d3ee",
      borderColor: "#334155",
      textPrimaryColor: "#e2e8f0",
      textMutedColor: "#94a3b8",
      codeTextColor: "#e2e8f0",
      codeCommentColor: "#8b949e",
      codeKeywordColor: "#ff7b72",
      codeStringColor: "#a5d6ff",
      codeNumberColor: "#79c0ff",
      codeTitleColor: "#d2a8ff",
      codeVariableColor: "#ffa657",
    };
    setDraft(d);
    applyPreview(d);
  }

  if (!draft) {
    return (
      <div className="py-6 text-center text-xs text-[color:var(--text-muted)]">
        Loading appearance…
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            Appearance
          </h3>
          <button
            type="button"
            onClick={handleResetDefaults}
            className="flex items-center gap-1 text-xs text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            title="Reset theme, font, and all colors to defaults (still needs Save)"
          >
            <RotateCcw className="h-3 w-3" />
            Reset all
          </button>
        </div>

        <div className="space-y-1">
          <label className="text-sm text-[color:var(--text-primary)]">Theme</label>
          <div className="flex gap-2">
            <ThemeButton
              active={draft.theme === "dark"}
              onClick={() => patch({ theme: "dark" })}
              icon={<Moon className="h-4 w-4" />}
              label="Dark"
            />
            <ThemeButton
              active={draft.theme === "light"}
              onClick={() => patch({ theme: "light" })}
              icon={<Sun className="h-4 w-4" />}
              label="Light"
            />
            <ThemeButton
              active={draft.theme === "system"}
              onClick={() => patch({ theme: "system" })}
              icon={<Monitor className="h-4 w-4" />}
              label="System"
            />
          </div>
        </div>

        <div className="space-y-1">
          <label className="text-sm text-[color:var(--text-primary)]" htmlFor="settings-font-family">
            Font family
          </label>
          <select
            id="settings-font-family"
            value={draft.fontFamily}
            onChange={(e) => patch({ fontFamily: e.target.value })}
            className="w-full rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
          >
            {FONT_OPTIONS.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
        </div>

        <div className="space-y-1">
          <label className="text-sm text-[color:var(--text-primary)]" htmlFor="settings-font-size">
            Font size{" "}
            <span className="text-[color:var(--text-muted)]">({draft.fontSize}px)</span>
          </label>
          <input
            id="settings-font-size"
            type="range"
            min={10}
            max={24}
            step={1}
            value={draft.fontSize}
            onChange={(e) => {
              const n = Number(e.target.value);
              if (Number.isFinite(n)) patch({ fontSize: Math.max(10, Math.min(24, n)) });
            }}
            className="w-full accent-[color:var(--accent-color)]"
          />
        </div>
      </div>

      <div className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            Colors
          </h3>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() =>
                patch({
                  accentColor: "#22d3ee",
                  borderColor: "#334155",
                  textPrimaryColor: "#e2e8f0",
                  textMutedColor: "#94a3b8",
                  codeTextColor: "#e2e8f0",
                  codeCommentColor: "#8b949e",
                  codeKeywordColor: "#ff7b72",
                  codeStringColor: "#a5d6ff",
                  codeNumberColor: "#79c0ff",
                  codeTitleColor: "#d2a8ff",
                  codeVariableColor: "#ffa657",
                })
              }
              className="rounded border border-border px-1.5 py-0.5 text-[0.65rem] text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            >
              Default dark
            </button>
            <button
              type="button"
              onClick={() =>
                patch({
                  accentColor: "#0e7490",
                  borderColor: "#cbd5e1",
                  textPrimaryColor: "#0f172a",
                  textMutedColor: "#475569",
                  codeTextColor: "#0f172a",
                  codeCommentColor: "#64748b",
                  codeKeywordColor: "#b91c1c",
                  codeStringColor: "#0369a1",
                  codeNumberColor: "#1d4ed8",
                  codeTitleColor: "#6d28d9",
                  codeVariableColor: "#c2410c",
                })
              }
              className="rounded border border-border px-1.5 py-0.5 text-[0.65rem] text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            >
              Light preset
            </button>
          </div>
        </div>

        <ColorRow
          label="Accent"
          value={draft.accentColor}
          onChange={(c) => patch({ accentColor: c })}
          hint="Buttons, links, active states"
        />
        <ColorRow
          label="Border"
          value={draft.borderColor}
          onChange={(c) => patch({ borderColor: c })}
          hint="Dividers, card borders"
        />
        <ColorRow
          label="Text primary"
          value={draft.textPrimaryColor}
          onChange={(c) => patch({ textPrimaryColor: c })}
          hint="Main text color"
        />
        <ColorRow
          label="Text muted"
          value={draft.textMutedColor}
          onChange={(c) => patch({ textMutedColor: c })}
          hint="Secondary / placeholder text"
        />

        <div className="pt-2">
          <h4 className="text-[0.7rem] font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
            Code
          </h4>
        </div>
        <ColorRow
          label="Code text"
          value={draft.codeTextColor}
          onChange={(c) => patch({ codeTextColor: c })}
          hint="Base code block text"
        />
        <ColorRow
          label="Comment"
          value={draft.codeCommentColor}
          onChange={(c) => patch({ codeCommentColor: c })}
          hint="Comments & metadata"
        />
        <ColorRow
          label="Keyword"
          value={draft.codeKeywordColor}
          onChange={(c) => patch({ codeKeywordColor: c })}
          hint="Keywords, types, literals"
        />
        <ColorRow
          label="String"
          value={draft.codeStringColor}
          onChange={(c) => patch({ codeStringColor: c })}
          hint="Strings, attributes"
        />
        <ColorRow
          label="Number"
          value={draft.codeNumberColor}
          onChange={(c) => patch({ codeNumberColor: c })}
          hint="Numbers, attributes"
        />
        <ColorRow
          label="Title"
          value={draft.codeTitleColor}
          onChange={(c) => patch({ codeTitleColor: c })}
          hint="Function & class names"
        />
        <ColorRow
          label="Variable"
          value={draft.codeVariableColor}
          onChange={(c) => patch({ codeVariableColor: c })}
          hint="Variables"
        />
      </div>

      <div className="space-y-1">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[color:var(--text-muted)]">
          Preview
        </h3>
        <div
          className="space-y-2 rounded-lg border border-border bg-bg-primary px-3 py-3 text-[color:var(--text-primary)]"
          style={{
            fontFamily: "var(--app-font-family)",
            fontSize: "var(--app-font-size)",
          }}
        >
          <p>The quick brown fox jumps over the lazy dog.</p>
          <pre
            className="overflow-x-auto rounded border border-border px-3 py-2 text-[0.85em] leading-relaxed"
            style={{ color: "var(--code-text)" }}
          >
            <code>
              <span className="hljs-keyword">fn</span>{" "}
              <span className="hljs-title">main</span>() {"{"}
              {"\n  "}
              <span className="hljs-keyword">let</span>{" "}
              <span className="hljs-variable">msg</span> ={" "}
              <span className="hljs-string">"hello"</span>;
              {"\n  println!"}(<span className="hljs-string">{"{}"}</span>,{" "}
              <span className="hljs-variable">msg</span>);
              {"\n}"}
            </code>
          </pre>
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      )}
      {ok && (
        <div className="rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
          Appearance saved.
        </div>
      )}
    </div>
  );
  },
);

function ThemeButton({
  active,
  onClick,
  icon,
  label,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex flex-1 items-center justify-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors ${
        active
          ? "border-[color:var(--accent-color)] bg-[color:var(--accent-color)]/20 text-[color:var(--accent-color)]"
          : "border-border bg-bg-primary text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

const HEX_RE = /^#[0-9a-fA-F]{6}$/;

function ColorRow({
  label,
  value,
  onChange,
  hint,
}: {
  label: string;
  value: string;
  onChange: (c: string) => void;
  hint?: string;
}) {
  const id = `color-${label.replace(/\s+/g, "-").toLowerCase()}`;
  const [local, setLocal] = useState(value);
  useEffect(() => {
    setLocal(value);
  }, [value]);

  return (
    <div className="flex items-center gap-3">
      <input
        id={id}
        type="color"
        value={HEX_RE.test(value) ? value : "#000000"}
        onChange={(e) => onChange(e.target.value)}
        className="h-8 w-8 shrink-0 cursor-pointer rounded border border-border bg-bg-primary"
        title={label}
        aria-label={label}
      />
      <div className="flex flex-1 flex-col">
        <label htmlFor={`${id}-hex`} className="text-sm text-[color:var(--text-primary)]">
          {label}
        </label>
        {hint && <span className="text-xs text-[color:var(--text-muted)]">{hint}</span>}
      </div>
      <input
        id={`${id}-hex`}
        type="text"
        value={local}
        onChange={(e) => {
          const v = e.target.value;
          setLocal(v);
          if (HEX_RE.test(v)) onChange(v);
        }}
        onBlur={() => {
          if (HEX_RE.test(local)) onChange(local);
          else setLocal(value);
        }}
        className={`w-24 rounded-lg border bg-bg-primary px-2 py-1 text-sm text-[color:var(--text-primary)] focus:outline-none ${
          local === "" || HEX_RE.test(local)
            ? "border-border focus:border-[color:var(--accent-color)]"
            : "border-red-500/60"
        }`}
        spellCheck={false}
        aria-label={`${label} hex`}
        aria-invalid={local !== "" && !HEX_RE.test(local)}
      />
    </div>
  );
}

export type { Theme };

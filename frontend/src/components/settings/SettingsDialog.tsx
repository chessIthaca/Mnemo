// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Settings,
  X,
  Server,
  Palette,
  MessageSquare,
  Shield,
  GitBranch,
  Eye,
  Brain,
  DollarSign,
  SlidersHorizontal,
  Cpu,
  Search,
  Database,
  Volume2,
  Plug,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogTitle,
} from "../ui/dialog";
import { useAgentStore } from "../../hooks/useAgentStore";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";
import {
  SETTINGS_NAV,
  dirtySectionIds,
  isSettingsSectionId,
  type SettingsSectionHandle,
  type SettingsSectionId,
} from "./types";
import { AppearanceSection } from "./sections/AppearanceSection";
import { ChatSection } from "./sections/ChatSection";
import { SoundsSection } from "./sections/SoundsSection";
import { ProvidersSection } from "./sections/ProvidersSection";
import { VisionSection } from "./sections/VisionSection";
import { EmbeddingSection } from "./sections/EmbeddingSection";
import { MemorySection } from "./sections/MemorySection";
import { SafetySection } from "./sections/SafetySection";
import { GitSection } from "./sections/GitSection";
import { PricingSection } from "./sections/PricingSection";
import { ModelsSection } from "./sections/ModelsSection";
import { McpSection } from "./sections/McpSection";
import { AdvancedSection } from "./sections/AdvancedSection";

export interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
  /** Optional initial section (deep-link). */
  initialSection?: SettingsSectionId;
}

const NAV_ICONS: Record<SettingsSectionId, typeof Server> = {
  providers: Server,
  appearance: Palette,
  chat: MessageSquare,
  sounds: Volume2,
  safety: Shield,
  git: GitBranch,
  vision: Eye,
  embeddings: Brain,
  memory: Database,
  pricing: DollarSign,
  models: Cpu,
  mcp: Plug,
  advanced: SlidersHorizontal,
};

/**
 * Settings shell — wide dialog with left section nav, search filter,
 * scrollable content, and dirty-aware close for unsaved edits in any section.
 */
export function SettingsDialog({
  open,
  onClose,
  initialSection = "providers",
}: SettingsDialogProps) {
  const deepLink = useAgentStore((s) => s.settingsSection);
  const [section, setSection] = useState<SettingsSectionId>(initialSection);
  const [dirtyMap, setDirtyMap] = useState<Partial<Record<SettingsSectionId, boolean>>>({});
  const [query, setQuery] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Hide the native child WebView2 while this full-viewport modal is open
  // (it's a separate HWND composited above the app's HTML — see useBrowserOverlay).
  useBrowserOverlay(open);

  // Imperative refs to each section's save() handle, so the dialog-level OK
  // button can save every dirty section in one action.
  const providersRef = useRef<SettingsSectionHandle>(null);
  const appearanceRef = useRef<SettingsSectionHandle>(null);
  const chatRef = useRef<SettingsSectionHandle>(null);
  const soundsRef = useRef<SettingsSectionHandle>(null);
  const safetyRef = useRef<SettingsSectionHandle>(null);
  const gitRef = useRef<SettingsSectionHandle>(null);
  const visionRef = useRef<SettingsSectionHandle>(null);
  const embeddingsRef = useRef<SettingsSectionHandle>(null);
  const memoryRef = useRef<SettingsSectionHandle>(null);
  const pricingRef = useRef<SettingsSectionHandle>(null);
  const modelsRef = useRef<SettingsSectionHandle>(null);
  const mcpRef = useRef<SettingsSectionHandle>(null);
  const advancedRef = useRef<SettingsSectionHandle>(null);
  const sectionRefs: Partial<Record<SettingsSectionId, React.RefObject<SettingsSectionHandle>>> = {
    providers: providersRef,
    appearance: appearanceRef,
    chat: chatRef,
    sounds: soundsRef,
    safety: safetyRef,
    git: gitRef,
    vision: visionRef,
    embeddings: embeddingsRef,
    memory: memoryRef,
    pricing: pricingRef,
    models: modelsRef,
    mcp: mcpRef,
    advanced: advancedRef,
  };

  // Apply deep-link / initial section whenever the dialog opens.
  useEffect(() => {
    if (!open) return;
    if (deepLink && isSettingsSectionId(deepLink)) {
      setSection(deepLink);
    } else if (initialSection) {
      setSection(initialSection);
    }
  }, [open, deepLink, initialSection]);

  const setSectionDirty = useCallback((id: SettingsSectionId, dirty: boolean) => {
    setDirtyMap((prev) => (prev[id] === dirty ? prev : { ...prev, [id]: dirty }));
  }, []);

  const dirtySections = useMemo(
    () => (Object.keys(dirtyMap) as SettingsSectionId[]).filter((id) => dirtyMap[id]),
    [dirtyMap],
  );
  const anyDirty = dirtySections.length > 0;

  const filteredNav = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return SETTINGS_NAV;
    return SETTINGS_NAV.filter((item) => {
      const hay = [item.label, item.hint ?? "", ...(item.keywords ?? [])]
        .join(" ")
        .toLowerCase();
      return hay.includes(q);
    });
  }, [query]);

  function requestClose() {
    if (anyDirty) {
      const labels = dirtySections.map(
        (id) => SETTINGS_NAV.find((n) => n.id === id)?.label ?? id,
      );
      const ok = window.confirm(
        `You have unsaved changes in: ${labels.join(", ")}. Discard them and close Settings?`,
      );
      if (!ok) return;
    }
    setDirtyMap({});
    setQuery("");
    setSaveError(null);
    onClose();
  }

  // OK: save every dirty section, then close only if all succeeded.
  async function handleOk() {
    const ids = dirtySectionIds(dirtyMap);
    if (ids.length === 0) {
      setSaveError(null);
      onClose();
      return;
    }
    setSaving(true);
    setSaveError(null);
    let failures = 0;
    for (const id of ids) {
      const handle = sectionRefs[id]?.current;
      if (!handle) continue;
      const ok = await handle.save();
      if (!ok) failures += 1;
    }
    setSaving(false);
    if (failures > 0) {
      setSaveError(
        `${failures} section(s) failed to save — see their tabs for details.`,
      );
      return; // stay open so the user can fix + retry
    }
    setDirtyMap({});
    setQuery("");
    setSaveError(null);
    onClose();
  }

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen) requestClose();
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="mx-4 flex h-[min(720px,90vh)] w-full max-w-3xl flex-col overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-2xl"
        onEscapeKeyDown={(e: Event) => {
          e.preventDefault();
          requestClose();
        }}
        onInteractOutside={(e: Event) => {
          e.preventDefault();
          requestClose();
        }}
      >
        <div className="flex shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-3">
          <div className="flex min-w-0 items-center gap-2">
            <Settings className="h-5 w-5 shrink-0 text-[color:var(--accent-color)]" />
            <DialogTitle className="text-sm font-semibold text-[color:var(--text-primary)]">
              Settings
            </DialogTitle>
            {anyDirty && (
              <span
                className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[0.65rem] font-medium text-amber-300"
                aria-live="polite"
              >
                Unsaved changes
              </span>
            )}
          </div>
          <div className="flex min-w-0 flex-1 items-center justify-end gap-2">
            <label className="relative hidden min-w-0 max-w-[14rem] flex-1 sm:block">
              <span className="sr-only">Search settings</span>
              <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-[color:var(--text-muted)]" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search…"
                className="w-full rounded-lg border border-border bg-bg-primary py-1.5 pl-7 pr-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
              />
            </label>
            <button
              type="button"
              onClick={requestClose}
              aria-label="Close settings"
              className="rounded p-1 text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>

        <div className="flex min-h-0 flex-1">
          <nav
            className="flex w-44 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-border bg-bg-primary/40 px-2 py-3"
            aria-label="Settings sections"
          >
            <div className="mb-2 px-1 sm:hidden">
              <label className="relative block">
                <span className="sr-only">Search settings</span>
                <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-[color:var(--text-muted)]" />
                <input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="Search…"
                  className="w-full rounded-lg border border-border bg-bg-primary py-1.5 pl-7 pr-2 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
                />
              </label>
            </div>
            {filteredNav.length === 0 && (
              <p className="px-2 text-[0.7rem] text-[color:var(--text-muted)]">
                No matching sections.
              </p>
            )}
            {filteredNav.map((item) => {
              const Icon = NAV_ICONS[item.id];
              const active = section === item.id;
              const sectionDirty = !!dirtyMap[item.id];
              return (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => setSection(item.id)}
                  aria-current={active ? "page" : undefined}
                  className={`flex flex-col items-start gap-0.5 rounded-lg px-2.5 py-2 text-left transition-colors ${
                    active
                      ? "bg-bg-tertiary text-[color:var(--text-primary)]"
                      : "text-[color:var(--text-muted)] hover:bg-bg-tertiary/60 hover:text-[color:var(--text-primary)]"
                  }`}
                >
                  <span className="flex items-center gap-2 text-xs font-medium">
                    <Icon className="h-3.5 w-3.5 shrink-0" aria-hidden />
                    {item.label}
                    {sectionDirty && (
                      <span
                        className="h-1.5 w-1.5 rounded-full bg-amber-400"
                        title="Unsaved changes"
                        aria-hidden
                      />
                    )}
                  </span>
                  {item.hint && (
                    <span className="pl-5 text-[0.65rem] leading-tight opacity-70">
                      {item.hint}
                    </span>
                  )}
                </button>
              );
            })}
          </nav>

          <div className="min-h-0 min-w-0 flex-1 overflow-y-auto px-5 py-4">
            <div
              className={section === "providers" ? "block" : "hidden"}
              aria-hidden={section !== "providers"}
            >
              <ProvidersSection
                ref={providersRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("providers", d)}
              />
            </div>
            <div
              className={section === "appearance" ? "block" : "hidden"}
              aria-hidden={section !== "appearance"}
            >
              <AppearanceSection
                ref={appearanceRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("appearance", d)}
              />
            </div>
            <div
              className={section === "chat" ? "block" : "hidden"}
              aria-hidden={section !== "chat"}
            >
              <ChatSection
                ref={chatRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("chat", d)}
              />
            </div>
            <div
              className={section === "sounds" ? "block" : "hidden"}
              aria-hidden={section !== "sounds"}
            >
              <SoundsSection
                ref={soundsRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("sounds", d)}
              />
            </div>
            <div
              className={section === "safety" ? "block" : "hidden"}
              aria-hidden={section !== "safety"}
            >
              <SafetySection
                ref={safetyRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("safety", d)}
              />
            </div>
            <div
              className={section === "git" ? "block" : "hidden"}
              aria-hidden={section !== "git"}
            >
              <GitSection
                ref={gitRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("git", d)}
              />
            </div>
            <div
              className={section === "vision" ? "block" : "hidden"}
              aria-hidden={section !== "vision"}
            >
              <VisionSection
                ref={visionRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("vision", d)}
              />
            </div>
            <div
              className={section === "embeddings" ? "block" : "hidden"}
              aria-hidden={section !== "embeddings"}
            >
              <EmbeddingSection
                ref={embeddingsRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("embeddings", d)}
              />
            </div>
            <div
              className={section === "memory" ? "block" : "hidden"}
              aria-hidden={section !== "memory"}
            >
              <MemorySection
                ref={memoryRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("memory", d)}
              />
            </div>
            <div
              className={section === "pricing" ? "block" : "hidden"}
              aria-hidden={section !== "pricing"}
            >
              <PricingSection
                ref={pricingRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("pricing", d)}
              />
            </div>
            <div
              className={section === "models" ? "block" : "hidden"}
              aria-hidden={section !== "models"}
            >
              <ModelsSection
                ref={modelsRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("models", d)}
              />
            </div>
            <div
              className={section === "mcp" ? "block" : "hidden"}
              aria-hidden={section !== "mcp"}
            >
              <McpSection
                ref={mcpRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("mcp", d)}
              />
            </div>
            <div
              className={section === "advanced" ? "block" : "hidden"}
              aria-hidden={section !== "advanced"}
            >
              <AdvancedSection
                ref={advancedRef}
                active={open}
                onDirtyChange={(d) => setSectionDirty("advanced", d)}
              />
            </div>
          </div>
        </div>

        <div className="flex shrink-0 items-center justify-between gap-3 border-t border-border px-4 py-3">
          <div className="min-w-0">
            <p className="text-[0.7rem] text-[color:var(--text-muted)]">
              {anyDirty
                ? "OK saves all changes and closes; Cancel discards."
                : "OK saves any changes and closes; Cancel closes without saving."}
            </p>
            {saveError && (
              <p className="mt-0.5 text-[0.7rem] text-red-400">{saveError}</p>
            )}
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              onClick={requestClose}
              disabled={saving}
              className="rounded-lg border border-border px-4 py-1.5 text-sm text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)] disabled:opacity-40"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void handleOk()}
              disabled={!anyDirty || saving}
              className="rounded-lg bg-[color:var(--accent-color)] px-4 py-1.5 text-sm font-medium text-white hover:opacity-90 disabled:opacity-40"
            >
              {saving ? "Saving…" : "OK"}
            </button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

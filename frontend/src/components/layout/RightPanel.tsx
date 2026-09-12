// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { X } from "lucide-react";
import { useAgentStore } from "../../hooks/useAgentStore";
import type { RightPanelTab } from "../../hooks/useAgentStore";
import { RIGHT_PANEL_VIEWS } from "../../hooks/rightPanelViews";
import { Tabs, TabsList, TabsTrigger } from "../ui/tabs";

export function RightPanel() {
  const activeTab = useAgentStore((s) => s.rightPanelTab);
  const setRightPanelTab = useAgentStore((s) => s.setRightPanelTab);
  const toggleRightPanel = useAgentStore((s) => s.toggleRightPanel);
  const toggleTabAndReveal = useAgentStore((s) => s.toggleTabAndReveal);
  const disabledTabs = useAgentStore((s) => s.disabledTabs);
  const rightPanelWidth = useAgentStore((s) => s.rightPanelWidth);

  // Only enabled tools render as tabs. If the active tab was disabled, fall
  // back to the first enabled tab so a disabled tool can't render.
  const enabledViews = RIGHT_PANEL_VIEWS.filter((v) => !disabledTabs.includes(v.id));
  const activeStillEnabled = enabledViews.some((v) => v.id === activeTab);
  const shownTab: RightPanelTab | undefined = activeStillEnabled
    ? activeTab
    : enabledViews[0]?.id;

  // Re-clamp the persisted width against the current viewport so a stored
  // width from a larger monitor (or a smaller restore) can't push the main
  // column toward zero width. Falls back to flex-grow (undefined style) when
  // no explicit width is set.
  const clampedWidth =
    rightPanelWidth !== null
      ? Math.max(300, Math.min(window.innerWidth * 0.8, rightPanelWidth))
      : null;

  return (
    <div
      className="flex min-w-[300px] flex-col bg-bg-secondary"
      style={clampedWidth !== null ? { width: `${clampedWidth}px` } : undefined}
    >
      {/* Tab bar — Radix Tabs gives role="tablist"/"tab", aria-selected, and
          arrow-key navigation. The X close button stays outside the tablist. */}
      <div className="flex items-center border-b border-border">
        <Tabs
          value={shownTab}
          onValueChange={(v: string) => setRightPanelTab(v as RightPanelTab)}
          activationMode="automatic"
          className="flex-1 overflow-x-auto"
        >
          <TabsList className="flex">
            {enabledViews.map((view) => {
              const Icon = view.icon;
              const isActive = shownTab === view.id;
              return (
                <TabsTrigger
                  key={view.id}
                  value={view.id}
                  className={`group flex h-10 items-center gap-1.5 border-b-2 px-3 text-xs font-medium transition-colors ${
                    isActive
                      ? "border-cyan-500 text-cyan-400"
                      : "border-transparent text-slate-500 hover:text-slate-300"
                  }`}
                >
                  <Icon className="h-3.5 w-3.5" />
                  {view.label}
                  {/* Close (x) — disables this tool tab. Re-enable from the left
                      toolbar. A nested <button> is invalid inside Radix's
                      TabsTrigger (which renders a <button>), so this is a
                      span with role=button. stopPropagation on pointer-down
                      keeps the click from also selecting the tab. */}
                  <span
                    role="button"
                    tabIndex={0}
                    aria-label={`Disable ${view.label} tab`}
                    title="Disable (re-enable from the left toolbar)"
                    onPointerDown={(e) => {
                      e.stopPropagation();
                    }}
                    onClick={(e) => {
                      e.stopPropagation();
                      toggleTabAndReveal(view.id);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        e.stopPropagation();
                        toggleTabAndReveal(view.id);
                      }
                    }}
                    className="ml-0.5 flex h-3.5 w-3.5 items-center justify-center rounded-sm text-slate-500 opacity-0 transition-opacity hover:bg-bg-tertiary hover:text-slate-200 focus:opacity-100 group-hover:opacity-100"
                  >
                    <X className="h-3 w-3" />
                  </span>
                </TabsTrigger>
              );
            })}
          </TabsList>
        </Tabs>
        <button
          onClick={toggleRightPanel}
          aria-label="Close right panel"
          className="px-2 py-2 text-slate-500 hover:text-slate-300"
        >
          <X className="h-4 w-4" />
        </button>
      </div>
      {/* Tab content — applies the user-chosen font family + size as the
          inherited base so all views scale together, matching the chat. The
          active view's component is looked up from the single registry
          (Maint M6), replacing the per-id render ladder. */}
      <div
        className="flex-1 overflow-hidden"
        style={{
          fontFamily: "var(--app-font-family)",
          fontSize: "var(--app-font-size)",
        }}
      >
        {shownTab !== undefined &&
          (() => {
            const view = RIGHT_PANEL_VIEWS.find((v) => v.id === shownTab);
            if (!view) return null;
            const Body = view.component;
            return <Body />;
          })()}
        {shownTab === undefined && (
          <div className="flex h-full items-center justify-center p-4 text-center text-slate-500">
            All tools are off. Turn one on from the left toolbar.
          </div>
        )}
      </div>
    </div>
  );
}

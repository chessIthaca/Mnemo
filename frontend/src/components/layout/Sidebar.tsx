// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { PanelRight, Settings, Folder } from "lucide-react";
import { useState, memo } from "react";
import { useAgentStore, ALL_RIGHT_PANEL_TABS } from "../../hooks/useAgentStore";
import { RIGHT_PANEL_VIEWS } from "../../hooks/rightPanelViews";
import { MnemoLogo } from "../common/MnemoLogo";
import { ConfigDialog } from "./ConfigDialog";
import { AboutDialog } from "../about/AboutDialog";
import { ProjectPicker } from "../projects/ProjectPicker";

/** id -> view lookup for the per-tool toggle buttons. Derived from the single
 * right-panel registry (`RIGHT_PANEL_VIEWS`) so a new tab can't drift out of
 * sync with the sidebar again (previously a hand-copied TOOL_META map that
 * broke the build when `game` was added). */
const VIEW_BY_ID = new Map(RIGHT_PANEL_VIEWS.map((v) => [v.id, v]));

/**
 * Left sidebar — reserved for tools. Agents live in the top tab bar now.
 * Each right-panel tool gets a per-tool on/off toggle button; the panel as a
 * whole is shown/hidden via the bottom PanelRight toggle.
 *
 * Memoized (mem-perf review HIGH 1): prop-less + self-subscribing, so
 * App-level re-renders can't drag it along — it only re-renders when its
 * own store slices change.
 */
export const Sidebar = memo(function Sidebar() {
  const toggleRightPanel = useAgentStore((s) => s.toggleRightPanel);
  const disabledTabs = useAgentStore((s) => s.disabledTabs);
  const toggleTabAndReveal = useAgentStore((s) => s.toggleTabAndReveal);
  const settingsOpen = useAgentStore((s) => s.settingsOpen);
  const openSettings = useAgentStore((s) => s.openSettings);
  const closeSettings = useAgentStore((s) => s.closeSettings);
  /** True when at least one tool is enabled; the panel toggle is useless otherwise. */
  const anyToolsEnabled = disabledTabs.length < ALL_RIGHT_PANEL_TABS.length;
  const [projectSwitchOpen, setProjectSwitchOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);

  return (
    <div className="flex w-14 flex-col items-center gap-4 border-r border-border bg-bg-secondary py-4">
      <button
        type="button"
        onClick={() => setAboutOpen(true)}
        title="About Mnemo"
        aria-label="About Mnemo"
        className="flex h-10 w-10 items-center justify-center rounded-lg transition-opacity hover:opacity-90 focus:outline-none focus-visible:ring-2 focus-visible:ring-cyan-500"
      >
        <MnemoLogo className="h-10 w-10" />
      </button>
      <div className="h-px w-8 bg-border" />
      {/* Per-tool on/off toggles */}
      <div className="flex flex-col items-center gap-2 overflow-y-auto">
        {ALL_RIGHT_PANEL_TABS.map((tab) => {
          const meta = VIEW_BY_ID.get(tab)!;
          const Icon = meta.icon;
          const enabled = !disabledTabs.includes(tab);
          return (
            <button
              key={tab}
              onClick={() => toggleTabAndReveal(tab)}
              className={`flex h-10 w-10 items-center justify-center rounded-lg border-b-2 transition-colors ${
                enabled
                  ? "border-cyan-500 text-cyan-400 hover:bg-bg-tertiary"
                  : "border-transparent text-slate-600 line-through opacity-50 hover:bg-bg-tertiary hover:text-slate-400"
              }`}
              title={`${meta.label}: ${enabled ? "on (click to turn off)" : "off (click to turn on)"}`}
            >
              <Icon className="h-5 w-5" />
            </button>
          );
        })}
      </div>
      <div className="mt-auto flex flex-col items-center gap-2">
        <button
          onClick={() => setProjectSwitchOpen(true)}
          className="flex h-10 w-10 items-center justify-center rounded-lg text-slate-400 hover:bg-bg-tertiary hover:text-slate-200"
          title="Switch project"
        >
          <Folder className="h-5 w-5" />
        </button>
        <button
          onClick={() => openSettings()}
          className="flex h-10 w-10 items-center justify-center rounded-lg text-slate-400 hover:bg-bg-tertiary hover:text-slate-200"
          title="Settings"
        >
          <Settings className="h-5 w-5" />
        </button>
        <button
          onClick={toggleRightPanel}
          disabled={!anyToolsEnabled}
          className={`flex h-10 w-10 items-center justify-center rounded-lg ${
            anyToolsEnabled
              ? "text-slate-400 hover:bg-bg-tertiary hover:text-slate-200"
              : "cursor-not-allowed text-slate-600 opacity-50"
          }`}
          title={
            anyToolsEnabled
              ? "Show/hide all tools (right panel)"
              : "All tools are off — turn one on to use the panel"
          }
        >
          <PanelRight className="h-5 w-5" />
        </button>
      </div>
      <ConfigDialog open={settingsOpen} onClose={closeSettings} />
      <AboutDialog open={aboutOpen} onClose={() => setAboutOpen(false)} />
      {projectSwitchOpen && (
        <ProjectPicker onClose={() => setProjectSwitchOpen(false)} />
      )}
    </div>
  );
});

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// Right-panel view registry (Maint M6).
//
// A single source of truth for the right panel's tabs: each entry pairs the
// tab id (`RightPanelTab`) with its display metadata (label + icon) and the
// React component that renders its body. This replaces the three previously
// duplicated metadata sites — the `RightPanelTab` union, `ALL_RIGHT_PANEL_TABS`,
// and the `TABS`/render ladder in `RightPanel.tsx` — so adding a view is a
// one-place edit (append an entry here) instead of a four-place change.
//
// `RightPanelTab` + `ALL_RIGHT_PANEL_TABS` stay in `agentState.ts` (the store
// persists `disabledTabs: RightPanelTab[]`). `ALL_RIGHT_PANEL_TABS` is a
// hardcoded array there, kept in sync with this registry *by*
// `rightPanelViews.test.ts`, which asserts every `RightPanelTab` has a
// registry entry and vice-versa (plus equal length) — so drift is caught at
// test time, not prevented by derivation.

import type { ComponentType } from "react";
import { ListChecks, GitCompare, GitBranch, FolderTree, BarChart3, Activity, Inbox, Globe, Network, Brain, Gauge } from "lucide-react";

import type { RightPanelTab } from "./agentState";
import { FileViewer } from "../components/views/FileViewer";
import { PlanProgress } from "../components/views/PlanProgress";
import { DiffViewer } from "../components/views/DiffViewer";
import { GitView } from "../components/views/GitView";
import { StatsView } from "../components/views/StatsView";
import { LlmTraceView } from "../components/views/LlmTraceView";
import { BacklogView } from "../components/views/BacklogView";
import { BrowserView } from "../components/views/BrowserView";
import { GraphView } from "../components/views/GraphView";
import { DashboardView } from "../components/views/DashboardView";
import { MemoryDebugView } from "../components/views/MemoryDebugView";

/** The display + render metadata for one right-panel tab. */
export interface RightPanelView {
  /** The tab id (matches the `RightPanelTab` union + the store's persisted state). */
  id: RightPanelTab;
  /** The human-readable label shown on the tab trigger. */
  label: string;
  /** The lucide icon component rendered on the tab trigger. */
  icon: ComponentType<{ className?: string }>;
  /** The component rendered as the tab body when this tab is active. */
  component: ComponentType;
}

/**
 * The single right-panel view registry, in display order. Adding a view is a
 * one-place edit here (the store defaults + the UI render both derive from
 * this list). The order also defines the fallback order when the active tab is
 * disabled (`RightPanel.tsx` picks the first enabled entry).
 */
export const RIGHT_PANEL_VIEWS: RightPanelView[] = [
  { id: "plan", label: "Plan", icon: ListChecks, component: PlanProgress },
  { id: "diff", label: "Diff", icon: GitCompare, component: DiffViewer },
  { id: "git", label: "Git", icon: GitBranch, component: GitView },
  { id: "files", label: "Files", icon: FolderTree, component: FileViewer },
  { id: "stats", label: "Stats", icon: BarChart3, component: StatsView },
  { id: "trace", label: "Trace", icon: Activity, component: LlmTraceView },
  { id: "backlog", label: "Backlog", icon: Inbox, component: BacklogView },
  { id: "browser", label: "Browser", icon: Globe, component: BrowserView },
  { id: "graph", label: "Graph", icon: Network, component: GraphView },
  { id: "dashboard", label: "Dashboard", icon: Gauge, component: DashboardView },
  { id: "memory", label: "Memory", icon: Brain, component: MemoryDebugView },
];

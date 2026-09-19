// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// The Zustand store — frontend app state, updated by agent events.
//
// Thin facade (Maint H3): the heavy logic lives in focused pure modules —
// `agentState.ts` (per-agent types + helpers), `agentEventReducer.ts` (pure
// per-event reducers + `applyAgentEvent`), and `appearance.ts` (theme/font/
// color + localStorage). This file keeps the store interface ALL consumers
// import (`useAgentStore` + every type/helper re-export) and composes the
// slices into one Zustand store so nothing downstream changes.

import { create } from "zustand";
import type {
  AgentId,
  AgentInfo,
  AgentEventPayload,
  BacklogChangedPayload,
  BacklogItem,
  RunAllProgress,
  SafetyMode,
  SerializableAgentEvent,
  TranscriptEntry,
  WorkflowState,
} from "../lib/types";
import type { PricingEntry } from "../lib/tauri";
import { playSound, soundEnabled } from "../lib/sounds";
import { DEFAULT_HIDDEN_STEERING_NOTES, type SteeringNoteKey } from "../lib/delegationNotes";

import {
  DEFAULT_ACCENT_COLOR,
  DEFAULT_BORDER_COLOR,
  DEFAULT_CODE_COMMENT_COLOR,
  DEFAULT_CODE_KEYWORD_COLOR,
  DEFAULT_CODE_NUMBER_COLOR,
  DEFAULT_CODE_STRING_COLOR,
  DEFAULT_CODE_TEXT_COLOR,
  DEFAULT_CODE_TITLE_COLOR,
  DEFAULT_CODE_VARIABLE_COLOR,
  DEFAULT_FONT_FAMILY,
  DEFAULT_FONT_SIZE,
  DEFAULT_TEXT_MUTED_COLOR,
  DEFAULT_TEXT_PRIMARY_COLOR,
  LS_ACCENT_COLOR,
  LS_BORDER_COLOR,
  LS_CODE_COMMENT_COLOR,
  LS_CODE_KEYWORD_COLOR,
  LS_CODE_NUMBER_COLOR,
  LS_CODE_STRING_COLOR,
  LS_CODE_TEXT_COLOR,
  LS_CODE_TITLE_COLOR,
  LS_CODE_VARIABLE_COLOR,
  LS_TEXT_PRIMARY_COLOR,
  LS_TEXT_MUTED_COLOR,
  LS_FONT_FAMILY,
  LS_FONT_SIZE,
  LS_RIGHT_PANEL_WIDTH,
  LS_RIGHT_PANEL_WIDTH_FRAC,
  LS_SHOW_SHELL_PREVIEW,
  LS_SHOW_TOKEN_USAGE,
  LS_THEME,
  applyCodeColors,
  applyColorPrefs,
  applyColors,
  applyFontVars,
  applyTheme,
  defaultColorPrefs,
  readLs,
  readLsNumber,
  readLsNumberOrNull,
  readRightPanelWidthFrac,
  readShowShellPreview,
  readShowTokenUsage,
  readTheme,
  resolveTheme,
  setColor,
  writeColorPrefs,
  writeLs,
  type CodeColorKey,
  type ColorStateKey,
  type Theme,
} from "./appearance";
import type {
  ActivityEntry,
  AgentState,
  LastDiff,
  PendingApproval,
  PendingQuestion,
  RequestTiming,
  RightPanelTab,
  SteerEntry,
  TokenUsage,
} from "./agentState";
import {
  ALL_RIGHT_PANEL_TABS,
  RECENT_TIMING_WINDOW,
  aggregateTokPerSec,
  capActivityLog,
  capTranscript,
  didMainTurnEnd,
  emptyAgentState,
  getOrCreate,
  recentOutputTokPerSec,
  selectMainAgentId,
  MAX_ACTIVITY_ENTRIES,
  MAX_TRANSCRIPT_ENTRIES,
} from "./agentState";
import {
  STEER_LANDED_TTL_MS,
  MAX_CALLS_PER_TOOL_CARD,
  MAX_PLAN_DIFFS,
  appendAnswerEntry,
  appendLiveOutputTail,
  applyAgentEvent,
  type AppStateLike,
  type Effects,
  reduceQuestionAnswered,
} from "./agentEventReducer";
/** Monotonic id generator for steer backlog entries (facade-local). */
let nextSteerId = 1;

export type {
  ActivityEntry,
  AgentState,
  CodeColorKey,
  LastDiff,
  PendingApproval,
  PendingQuestion,
  RequestTiming,
  RightPanelTab,
  SteerEntry,
  Theme,
  TokenUsage,
};
export {
  ALL_RIGHT_PANEL_TABS,
  DEFAULT_ACCENT_COLOR,
  DEFAULT_BORDER_COLOR,
  DEFAULT_CODE_COMMENT_COLOR,
  DEFAULT_CODE_KEYWORD_COLOR,
  DEFAULT_CODE_NUMBER_COLOR,
  DEFAULT_CODE_STRING_COLOR,
  DEFAULT_CODE_TEXT_COLOR,
  DEFAULT_CODE_TITLE_COLOR,
  DEFAULT_CODE_VARIABLE_COLOR,
  DEFAULT_FONT_FAMILY,
  DEFAULT_FONT_SIZE,
  DEFAULT_TEXT_MUTED_COLOR,
  DEFAULT_TEXT_PRIMARY_COLOR,
  LS_ACCENT_COLOR,
  LS_BORDER_COLOR,
  LS_CODE_COMMENT_COLOR,
  LS_CODE_KEYWORD_COLOR,
  LS_CODE_NUMBER_COLOR,
  LS_CODE_STRING_COLOR,
  LS_CODE_TEXT_COLOR,
  LS_CODE_TITLE_COLOR,
  LS_CODE_VARIABLE_COLOR,
  LS_FONT_FAMILY,
  LS_FONT_SIZE,
  LS_RIGHT_PANEL_WIDTH,
  LS_RIGHT_PANEL_WIDTH_FRAC,
  LS_SHOW_SHELL_PREVIEW,
  LS_SHOW_TOKEN_USAGE,
  LS_THEME,
  MAX_ACTIVITY_ENTRIES,
  MAX_CALLS_PER_TOOL_CARD,
  MAX_PLAN_DIFFS,
  MAX_TRANSCRIPT_ENTRIES,
  RECENT_TIMING_WINDOW,
  aggregateTokPerSec,
  applyCodeColors,
  applyColors,
  applyFontVars,
  applyTheme,
  capActivityLog,
  capTranscript,
  didMainTurnEnd,
  emptyAgentState,
  readLs,
  readLsNumber,
  readLsNumberOrNull,
  readRightPanelWidthFrac,
  readShowShellPreview,
  readShowTokenUsage,
  readTheme,
  recentOutputTokPerSec,
  resolveTheme,
  selectMainAgentId,
  writeLs,
};

interface AppState extends AppStateLike {
  agents: Record<AgentId, AgentState>;
  /** Display names for agents (from AgentInfo.name), keyed by id. Fallback: `agent-<id>`. */
  agentNames: Record<AgentId, string>;
  /** Parent ids for agents (from AgentInfo.parent_id), keyed by id. `null`
   *  marks a parentless agent — the main agent is the parentless one with
   *  the smallest id. */
  agentParents: Record<AgentId, AgentId | null>;
  /** Per-agent model id (from AgentInfo.model), keyed by id. Absent for an
   *  agent whose model is unknown (e.g. it exited). Shown as a second line in
   *  the agent tab. */
  agentModels: Record<AgentId, string>;
  /** Per-agent serving endpoint name (from AgentInfo.provider /
   *  ModelChanged.provider), keyed by id — the backend names WHICH endpoint
   *  serves the agent's model, so the status bar never mislabels the
   *  provider when the same model id is listed under two endpoints
   *  (first-match resolution cannot disambiguate). Absent for an agent whose
   *  endpoint is unknown (mock-backed loops); the UI then resolves the model
   *  id against endpoints.toml. */
  agentProviders: Record<AgentId, string>;
  /** Per-agent reasoning effort last chosen in the toolbar, keyed by agent id.
   *  Models are agent-specific, so effort is too: each agent's provider was
   *  rebuilt with its own effort. Absent for an agent never switched — the UI
   *  falls back to its endpoint's configured default. */
  agentEfforts: Record<AgentId, string>;
  /** Per-agent WIRE-reported effective reasoning effort (from
   *  AgentInfo.reasoning_effort / ModelChanged.reasoning_effort), keyed by
   *  agent id — the backend's resolution of the model actually serving the
   *  agent's current context (per-context ModelRef override, else the
   *  model's default chain), in UI vocabulary. The status bar prefers this
   *  over the toolbar echo / endpoint default (backlog 51dab4da: the
   *  displayed effort must track the auto-chosen model). Absent when the
   *  backend doesn't know (mock-backed loops) — the UI falls back. */
  agentWireEfforts: Record<AgentId, string>;
  activeAgent: AgentId | null;
  /** Per-agent workflow phase, keyed by agent id. Each agent owns its own
   *  workflow, so the phase must be tracked per agent — a single global value
   *  goes stale (and a subagent's 'planning' can bleed into the main display). */
  workflowStates: Record<AgentId, WorkflowState>;
  model: string;
  provider: string;
  /** Current reasoning effort (toolbar dropdown): "max"/"high"/"medium"/"low"/"minimal"/"off". */
  reasoningEffort: string;
  /** Per-model pricing entries (from endpoints.toml), for cost estimation. */
  pricing: PricingEntry[];
  gitBranch: string;
  rightPanelVisible: boolean;
  /**
   * Right-panel width as a fraction of the window width (0–1), or null to
   * use the default flex-grow width. Set by dragging the divider between
   * the main column and the right panel; persisted to localStorage as a
   * fraction so it tracks window resizes and can never pin at a cap on a
   * small restored window.
   */
  rightPanelWidthFrac: number | null;
  rightPanelTab: RightPanelTab;
  /** Right-panel tool tabs the user has toggled off (per-tool on/off in the left sidebar). */
  disabledTabs: RightPanelTab[];
  /**
   * A file the user asked to open in the Files tab (via a tool-card file
   * link), held in the store so the FileViewer can consume it on mount/change
   * — a fire-and-forget event would race the viewer's mount and be lost.
   * `null` when there is no pending open. `line` is the 1-based line to
   * scroll the opened file to (read_files links pass the line the read
   * started at; null = top of file).
   */
  pendingFileOpen: { path: string; line: number | null } | null;
  /**
   * The http(s) URL a chat link asked the Browser tab to load, held in the
   * store so BrowserView can consume it on mount/change. It travels through
   * the store because the CHILD WEBVIEW'S RECT lives in BrowserView: the chat
   * has no rect to hand `browserWebviewEnsure`, and calling ensure with a
   * guessed one would MOVE an already-created child to the wrong place.
   * `null` when there is no pending open.
   */
  pendingBrowserUrl: string | null;
  /**
   * The child webview's CURRENT URL, written by the module-scope
   * `browser://url-changed` listener (useAgentEvents) from the child's
   * page-load events — agent-steered CDP navigations and in-child link
   * clicks alike. BrowserView's URL box syncs from it (on change AND on
   * mount — the store tracks the URL even while the Browser tab is
   * hidden, so there is no mount-timing gap). `""` when no URL is known.
   */
  browserUrl: string;
  safetyMode: SafetyMode;
  fontFamily: string;
  fontSize: number;
  theme: Theme;
  /** Whether InflightBar shows token counts (also persisted to config.toml [ui]). */
  showTokenUsage: boolean;
  /** Whether the live shell-output preview renders in running tool cards
   *  (persisted to localStorage mh.showShellPreview — backlog 6f25fb7e). */
  showShellPreview: boolean;
  /** Whether images from image commands (the image_* vision tools and the
   *  browser screenshot tools) render inline below their tool results in the
   *  agent chat (persisted to config.toml [ui].show_tool_images). */
  showToolImages: boolean;
  /** Whether agent-activity cards (tool calls, memory reads/writes, vision
   *  image-parsing, skill announcements) render in the chat transcript
   *  (persisted to config.toml [ui].show_tool_activity, default on — tool
   *  results show by default; turn the toggle off to hide them). GUI-only
   *  display filter: the transcript store always contains every entry and the
   *  model's context echo is unaffected; the Output tab still logs every tool
   *  result. */
  showToolActivity: boolean;
  /** Whether knowledge-access activity cards (graph_* tool calls, memory
   *  tool calls, auto-recall entries) render in the chat transcript
   *  (persisted to config.toml [ui].show_knowledge_activity, default on).
   *  GUI-only display filter: the transcript store always contains every
   *  entry and the model's context echo is unaffected; the console still
   *  logs every tool result. A subset of isActivityEntry visible by default
   *  so the user sees graph + memory + auto-recall activity without enabling
   *  the full show_tool_activity toggle. */
  showKnowledgeActivity: boolean;
  /** Which steering-note kinds are HIDDEN from the chat ToolCard render
   *  (persisted to config.toml [ui].steering_notes, one toggle per kind).
   *  GUI-only display filter: the tool result text (the model's context,
   *  including a note's re-issue escape-hatch hint) is unaffected — only the
   *  chat ToolCard's rendering hides the line. Hydrated from the resolved
   *  IPC flags; an absent field (older configs) falls back to the registry
   *  defaults — only `auto_delegated` hidden, i.e. the pre-existing behavior. */
  hiddenSteeringNotes: SteeringNoteKey[];
  /** Whether a vertical thread line is drawn along consecutive activity
   *  cards in the chat transcript (persisted to config.toml
   *  [ui].chat_thread_line, default on). GUI-only display filter. */
  chatThreadLine: boolean;
  /** Whether assistant prose is capped at ~100 columns — code blocks,
   *  diffs, and tool outputs stay full width (persisted to config.toml
   *  [ui].chat_prose_cap, default on). GUI-only display filter. */
  chatProseCap: boolean;
  /** Whether a faint background band alternates per conversation turn in
   *  the chat transcript (persisted to config.toml [ui].chat_turn_tint,
   *  default on). GUI-only display filter. */
  chatTurnTint: boolean;
  /** Whether transcript entries show their creation time as a native hover
   *  tooltip (persisted to config.toml [ui].chat_hover_timestamps, default
   *  on). GUI-only display filter. */
  chatHoverTimestamps: boolean;
  /** Whether the completion ding plays when an agent's plan reaches Complete
   *  (persisted to config.toml [ui].sound_complete). */
  soundComplete: boolean;
  /** Whether the needs-input ping plays when an agent requests approval or
   *  asks a question (persisted to config.toml [ui].sound_input_needed). */
  soundInput: boolean;
  /** Whether the doom tone plays when repeated errors stop an agent
   *  (persisted to config.toml [ui].sound_stopped_errors). */
  soundDoom: boolean;
  /** Configurable accent color (drives all cyan-* classes via CSS overrides). */
  accentColor: string;
  /** Configurable border color (--border-color). */
  borderColor: string;
  /** Configurable primary text color (--text-primary). */
  textPrimaryColor: string;
  /** Configurable muted text color (--text-muted). */
  textMutedColor: string;
  /** Configurable base code block text color (--code-text). */
  codeTextColor: string;
  /** Configurable code comment color (--code-comment). */
  codeCommentColor: string;
  /** Configurable code keyword color (--code-keyword). */
  codeKeywordColor: string;
  /** Configurable code string color (--code-string). */
  codeStringColor: string;
  /** Configurable code number color (--code-number). */
  codeNumberColor: string;
  /** Configurable code title/function color (--code-title). */
  codeTitleColor: string;
  /** Configurable code variable color (--code-variable). */
  codeVariableColor: string;
  /**
   * The most recent completed file-edit/file-write, so the DiffViewer can
   * keep showing the diff after the approval resolves.
   */
  lastDiff: LastDiff | null;
  /**
   * Captured diffs of the current top-level plan, newest first, one entry per
   * path (a re-edit of the same path replaces its earlier snapshot). Fed by
   * every completed file_edit/file_write/file_append; reset automatically when
   * a new top-level plan starts (the root plan id changes). Drives the Diff
   * tab's changed-file dropdown.
   */
  planDiffs: LastDiff[];
  /**
   * The root plan id carried by `workflow_state_changed` events (null when no
   * plan). The Diff-tab changed-file list resets exactly when this id changes
   * for the main agent — a new top-level plan started.
   */
  topPlanId: string | null;
  /**
   * The user's explicit Diff-tab dropdown pick (a path from `planDiffs`).
   * `null` = follow the latest/pending diff. Cleared when a new top-level
   * plan starts (the list resets).
   */
  selectedDiffPath: string | null;
  /**
   * Increments whenever the plan changes (created, step completed, state
   * transitioned). Components that display the plan subscribe to this and
   * re-fetch when it bumps, so the plan window updates in real time.
   */
  planVersion: number;
  /**
   * Increments whenever the global config (endpoints/models/keys) changes via
   * the Settings → Endpoints tab save. The StatusBar subscribes to this and
   * re-syncs its model/provider/reasoning-effort labels + endpoint list from
   * the backend so the toolbar reflects the new default without a restart.
   */
  configVersion: number;
  /** The backlog list (persisted backend-side in `.coding/backlog.json`). */
  backlog: BacklogItem[];
  /** Idle auto-feed toggle (session-only, default off). */
  autoFeed: boolean;
  /** Parallel run-all toggle (session-only, default off; plan ffd7a86f) —
   *  gates run-all concurrency only; auto-feed stays sequential. */
  parallelRunAll: boolean;
  /** Run-All ("overnight") loop progress. */
  runAll: RunAllProgress;
  /** Whether the Settings dialog is open. */
  settingsOpen: boolean;
  /**
   * Deep-link section for Settings (e.g. "pricing"). Applied when the dialog
   * opens; null means use the dialog's default.
   */
  settingsSection: string | null;

  // Actions.
  setActiveAgent: (id: AgentId) => void;
  /** Record an agent's workflow phase (from workflow_state_changed or a fetch). */
  setWorkflowState: (id: AgentId, state: WorkflowState) => void;
  /** Register an agent's display name (from AgentInfo). */
  setAgentName: (id: AgentId, name: string) => void;
  /** Record an agent's reasoning effort (from a successful toolbar switch). */
  setAgentEffort: (id: AgentId, effort: string) => void;
  /** Clear all per-agent effort records (after a config save reset every
   *  agent's provider to the new default effort). */
  clearAgentEfforts: () => void;
  /** Seed agent states + names from the backend's listAgents() result. */
  registerAgents: (infos: AgentInfo[]) => void;
  /** Seed each agent's context-window max from the backend's context_caps()
   *  result (startup + model-swap re-sync). Preserves the existing `used`
   *  count (a live turn's value is never clobbered) and ignores unknown ids. */
  seedContextCaps: (caps: [number, number][]) => void;
  /** The main agent's id — the parentless agent (parent_id === null) with the
   *  smallest id. Falls back to the smallest registered id while parent info
   *  is still loading. `null` when no agents are registered. */
  mainAgentId: () => AgentId | null;
  setModel: (m: string) => void;
  setProvider: (p: string) => void;
  setReasoningEffort: (e: string) => void;
  /** Bump configVersion so the StatusBar re-syncs from the backend after a
   *  config save (endpoints/models/keys changed). */
  bumpConfigVersion: () => void;
  setPricing: (p: PricingEntry[]) => void;
  setGitBranch: (b: string) => void;
  toggleRightPanel: () => void;
  /** Set the right-panel width fraction. Pass null to restore the default flex-grow width. */
  setRightPanelWidthFrac: (f: number | null) => void;
  /**
   * Update the right-panel width fraction in-memory only (no localStorage
   * write). Used during an active drag so the panel follows the cursor
   * without a synchronous localStorage write per pointermove.
   */
  setRightPanelWidthFracLive: (f: number) => void;
  setRightPanelTab: (tab: RightPanelTab) => void;
  /**
   * Set the user's explicit Diff-tab dropdown pick (a path from `planDiffs`).
   * Pass null to follow the latest/pending diff again.
   */
  selectDiffPath: (path: string | null) => void;
  /**
   * Auto-reveal the plan in the right panel when plan activity occurs (a plan
   * is created, or a step completes). Reveals the panel and selects the Plan
   * tab ONLY when the user has not manually hidden the panel and the Plan tab
   * is enabled — never yanks the panel away from a user's manual toggle.
   */
  autoRevealPlan: () => void;
  /** Auto-reveal the Diff tab for file tool approvals (file_edit/file_write). */
  autoRevealDiff: () => void;
  /** Toggle a single right-panel tool tab on/off. Disabling the active tab
   *  falls back to the first still-enabled tab. */
  toggleTab: (tab: RightPanelTab) => void;
  /** Toggle a tool tab like `toggleTab`, plus panel side-effects: enabling a
   *  tool reveals the right panel; disabling the last enabled tool hides the
   *  now-empty panel. */
  toggleTabAndReveal: (tab: RightPanelTab) => void;
  /** Reveal a right-panel tab without toggling: enable it (if disabled),
   *  select it, and make the panel visible. Used by deep-links (e.g. a
   *  tool-card file name opening the file in the Files tab). Unlike
   *  `toggleTabAndReveal`, calling this on an already-enabled tab NEVER
   *  disables it. */
  revealRightPanelTab: (tab: RightPanelTab) => void;
  /**
   * Request that a file be opened in the Files tab: reveal + select that tab
   * and record the path (+ optional 1-based reveal line) in `pendingFileOpen`
   * for the FileViewer to consume. Race-free (no event) — the viewer reads
   * the pending open whenever it is mounted, so it works even when the Files
   * tab was disabled/unmounted.
   */
  requestFileOpen: (path: string, line?: number | null) => void;
  /** Clear the pending file-open path (called by the FileViewer once loaded). */
  clearPendingFileOpen: () => void;
  /**
   * Request that the Browser tab load `url` (http(s)): reveal + select that
   * tab and record the URL in `pendingBrowserUrl` for BrowserView to consume.
   * Race-free (no event) — the view reads the pending URL whenever it is
   * mounted, so it works even when the Browser tab was disabled/unmounted.
   */
  requestBrowserOpen: (url: string) => void;
  /** Clear the pending browser URL (called by BrowserView once loaded). */
  clearPendingBrowserUrl: () => void;
  /** Record the child webview's current URL (called by the module-scope
   * `browser://url-changed` listener — see `browserUrl`). */
  setBrowserUrl: (url: string) => void;
  /** True if a right-panel tool tab is currently enabled (not in disabledTabs). */
  isTabEnabled: (tab: RightPanelTab) => boolean;
  setSafetyMode: (m: SafetyMode) => void;
  setShowTokenUsage: (show: boolean) => void;
  setShowShellPreview: (show: boolean) => void;
  setShowToolImages: (show: boolean) => void;
  setShowToolActivity: (show: boolean) => void;
  setShowKnowledgeActivity: (show: boolean) => void;
  setHiddenSteeringNotes: (keys: SteeringNoteKey[]) => void;
  setChatThreadLine: (on: boolean) => void;
  setChatProseCap: (on: boolean) => void;
  setChatTurnTint: (on: boolean) => void;
  setChatHoverTimestamps: (on: boolean) => void;
  setSoundComplete: (on: boolean) => void;
  setSoundInput: (on: boolean) => void;
  setSoundDoom: (on: boolean) => void;
  /** Open Settings, optionally jumping to a section id. */
  openSettings: (section?: string) => void;
  /** Close Settings and clear the deep-link section. */
  closeSettings: () => void;
  setFontFamily: (f: string) => void;
  setFontSize: (n: number) => void;
  setTheme: (t: Theme) => void;
  setAccentColor: (c: string) => void;
  setBorderColor: (c: string) => void;
  setTextPrimaryColor: (c: string) => void;
  setTextMutedColor: (c: string) => void;
  setCodeTextColor: (c: string) => void;
  setCodeCommentColor: (c: string) => void;
  setCodeKeywordColor: (c: string) => void;
  setCodeStringColor: (c: string) => void;
  setCodeNumberColor: (c: string) => void;
  setCodeTitleColor: (c: string) => void;
  setCodeVariableColor: (c: string) => void;
  /** Reset all colors (UI + code block) to their defaults. */
  resetColors: () => void;
  /**
   * Reset ALL appearance preferences to defaults: theme, font family, font
   * size, and every UI + code-block color. Writes the defaults to
   * localStorage and re-applies the CSS variables so the change is live.
   */
  resetAppearance: () => void;
  handleAgentEvent: (payload: AgentEventPayload) => void;
  /**
   * Append buffered streaming text to an agent's `streamingText` in a single
   * store update. Used by `useAgentEvents` to batch `text_delta` events via
   * `requestAnimationFrame` — one store update per frame instead of one per
   * token.
   */
  appendStreamingText: (id: AgentId, text: string) => void;
  /**
   * Append a coalesced batch of reasoning deltas to an agent's `streamingReasoning`
   * and activityLog (reasoning entry) in a single store update.
   * This is the batched/rAF equivalent of reduceReasoningDelta; the single
   * `text` is the accumulation of one or more `reasoning_delta` fragments for
   * this agent since the last flush.
   */
  appendStreamingReasoning: (id: AgentId, text: string) => void;
  /**
   * Apply one or more tool_call_arg_delta fragments for an agent in a *single*
   * store update. Clones the transcript array once, then applies all fragments
   * (which may target different in-flight calls) by index. This is the batched
   * equivalent of repeated reduceToolCallArgDelta calls.
   */
  applyToolCallArgDeltas: (
    id: AgentId,
    deltas: Array<{ index: number; fragment: string }>,
  ) => void;
  /**
   * Apply one or more `tool_output_delta` chunks for an agent in a *single*
   * store update (the batched equivalent of repeated `reduceToolOutputDelta`
   * calls): clones the transcript once, then appends every chunk to its
   * running call's `liveOutput` tail under the same head-drop cap. This is a
   * DISPLAY-ONLY live view — the complete text still arrives with the call's
   * result, which is what the model consumes.
   */
  applyToolOutputDeltas: (
    id: AgentId,
    deltas: Array<{ tool_call_id: string; text: string }>,
  ) => void;
  /**
   * Clear an agent's conversation: wipe the transcript, streaming text,
   * streaming reasoning, activity log, pending approval, and steers. This is
   * a PURE frontend store mutation — it does NOT stop the backend agent.
   *
   * Callers that clear MID-STREAM (while the agent is running) MUST interrupt
   * the backend first (see `interrupt` in lib/tauri.ts) AND drain the rAF
   * text-delta buffer (see `clearStreamingBuffer` in useAgentEvents).
   * Otherwise the agent keeps emitting events that repopulate the cleared
   * transcript and leave a dangling `pendingApproval` (its tool card was just
   * wiped), crashing the app. The `/clear` slash command in InputBar does
   * both. `pendingApproval` is cleared here too so a mid-approval clear
   * doesn't leave the ApprovalPrompt referring to a wiped tool card.
   */
  clearConversation: (id: AgentId) => void;
  /**
   * Add a steer (mid-work guidance) to an agent's backlog. Called when the
   * user sends a steer while the agent is running. The entry starts as
   * `pending` and is marked `landed` when a `suggestion_injected` event
   * arrives for that agent.
   */
  addSteer: (id: AgentId, text: string, images?: string[]) => void;
  /**
   * Remove a single pending steer by id — the "x" on a backlog entry. The
   * caller also sends a backend `cancel_suggestion` so the queued steer is
   * dropped before injection (a frontend-only removal would hide the bubble
   * but the backend would still inject the steer). Landed steers are
   * auto-removed by the TTL timer, so this is only meaningful for pending
   * ones.
   */
  removeSteer: (id: AgentId, steerId: number) => void;
  /** Remove all steers for an agent (e.g. on clear conversation). */
  clearSteers: (id: AgentId) => void;
  /**
   * Record that the user answered a pending `ask_user` question for an agent.
   * Appends a `qa` transcript entry (question + answer text) so the exchange
   * persists in the conversation, sets `pendingQuestionAnswered` so the live
   * `QuestionPrompt` collapses immediately, and TERMINATES the pending-question
   * display state (`pendingQuestion` is cleared — the backend resumes the same
   * turn and never emits a fresh `started` mid-turn, so without this the
   * InflightBar would stay on "waiting for your answer…"). Stashes `lastAnswer`.
   */
  recordQuestionAnswer: (
    id: AgentId,
    payload: { questionId: string; question: string; answer: string },
  ) => void;
  /**
   * Enter (or exit) freeform-answer mode for a pending question: the user
   * picked the "💬 Let's talk about it" numbered choice and will type their
   * answer in the standard InputBar. Pass the questionId to enter, null to
   * exit (cancel).
   */
  setFreeformQuestion: (id: AgentId, questionId: string | null) => void;
  /** Replace the backlog list. */
  setBacklog: (items: BacklogItem[]) => void;
  /** Set the idle auto-feed toggle. */
  setAutoFeed: (enabled: boolean) => void;
  setParallelRunAll: (enabled: boolean) => void;
  /** Set the Run-All loop progress. */
  setRunAll: (progress: RunAllProgress) => void;
  /**
   * Apply a `backlog://changed` event payload — sets the backlog list,
   * auto-feed toggle, and run-all progress in one store update.
   */
  applyBacklogChanged: (payload: BacklogChangedPayload) => void;
  /**
   * The in-progress Backlog-tab input text. Stored in the store (not component
   * state) so switching away from the Backlog tab — which unmounts
   * `BacklogInput` via the conditional render in `RightPanel.tsx` — does not
   * wipe a half-typed prompt. Cleared after a successful `backlogAdd`.
   */
  backlogDraft: string;
  /** Set the in-progress Backlog-tab input text. */
  setBacklogDraft: (text: string) => void;
  /**
   * The in-progress Backlog-tab attached images (base64 data URLs). Persisted
   * in the store for the same reason as `backlogDraft` — tab switches unmount
   * the input and would otherwise drop pasted/dropped images.
   */
  backlogDraftImages: string[];
  /** Set the in-progress Backlog-tab attached images. */
  setBacklogDraftImages: (images: string[]) => void;
}

export const useAgentStore = create<AppState>((set, get) => ({
  agents: {},
  agentNames: {},
  agentParents: {},
  agentModels: {},
  agentProviders: {},
  agentEfforts: {},
  agentWireEfforts: {},
  activeAgent: null,
  workflowStates: {},
  model: "",
  provider: "",
  reasoningEffort: "max",
  pricing: [],
  gitBranch: "",
  // At startup the right panel is open on the Plan tool — the only tool
  // enabled by default (all others start disabled; turn them on from the
  // left toolbar).
  rightPanelVisible: true,
  rightPanelWidthFrac: readRightPanelWidthFrac(),
  rightPanelTab: "plan",
  disabledTabs: ALL_RIGHT_PANEL_TABS.filter((t) => t !== "plan"),
  pendingFileOpen: null,
  pendingBrowserUrl: null,
  browserUrl: "",
  safetyMode: "approve-each-action",
  fontFamily: readLs(LS_FONT_FAMILY, DEFAULT_FONT_FAMILY),
  fontSize: readLsNumber(LS_FONT_SIZE, DEFAULT_FONT_SIZE),
  theme: readTheme(),
  showTokenUsage: readShowTokenUsage(),
  showShellPreview: readShowShellPreview(),
  showToolImages: true,
  // Agent-activity cards are shown by default (backlog 57687857 — user
  // request 2027-01-13: tool results visible out of the box); hydrated from
  // config.toml [ui] by the App bootstrap (getSettings). No localStorage
  // mirror — config.toml is the single persisted source (same pattern as the
  // sound flags below); the default only covers the brief pre-hydration
  // startup window.
  showToolActivity: true,
  // Knowledge-access cards (graph + memory + auto-recall) are visible by
  // default (backlog 68c4c9a5); hydrated from config.toml [ui] by the App
  // bootstrap (getSettings). No localStorage mirror — config.toml is the
  // single persisted source (same pattern as showToolActivity above); the
  // default only covers the brief pre-hydration startup window.
  showKnowledgeActivity: true,
  // Steering notes are model guidance, not end-user information, so the
  // AUTO-DELEGATED family is hidden by default (the registry supplies the
  // default set); hydrated from config.toml [ui.steering_notes] by the App
  // bootstrap (getSettings). No localStorage mirror — config.toml is the
  // single persisted source (same pattern as the flags above); the default
  // only covers the brief pre-hydration startup window.
  hiddenSteeringNotes: [...DEFAULT_HIDDEN_STEERING_NOTES],
  // Chat-readability affordances (thread line, prose cap, turn tint, hover
  // timestamps — plan afa81f0a): default ON, hydrated from config.toml [ui]
  // by the App bootstrap (getSettings). No localStorage mirror — config.toml
  // is the single persisted source (same pattern as the flags above); the
  // defaults only cover the brief pre-hydration startup window.
  chatThreadLine: true,
  chatProseCap: true,
  chatTurnTint: true,
  chatHoverTimestamps: true,
  // Notification-sound flags: default ON, hydrated from config.toml [ui] by
  // the App bootstrap (getSettings). No localStorage mirror — config.toml is
  // the single persisted source; the defaults only cover the brief
  // pre-hydration startup window, before any agent can be running.
  soundComplete: true,
  soundInput: true,
  soundDoom: true,
  accentColor: readLs(LS_ACCENT_COLOR, DEFAULT_ACCENT_COLOR),
  borderColor: readLs(LS_BORDER_COLOR, DEFAULT_BORDER_COLOR),
  textPrimaryColor: readLs(LS_TEXT_PRIMARY_COLOR, DEFAULT_TEXT_PRIMARY_COLOR),
  textMutedColor: readLs(LS_TEXT_MUTED_COLOR, DEFAULT_TEXT_MUTED_COLOR),
  codeTextColor: readLs(LS_CODE_TEXT_COLOR, DEFAULT_CODE_TEXT_COLOR),
  codeCommentColor: readLs(LS_CODE_COMMENT_COLOR, DEFAULT_CODE_COMMENT_COLOR),
  codeKeywordColor: readLs(LS_CODE_KEYWORD_COLOR, DEFAULT_CODE_KEYWORD_COLOR),
  codeStringColor: readLs(LS_CODE_STRING_COLOR, DEFAULT_CODE_STRING_COLOR),
  codeNumberColor: readLs(LS_CODE_NUMBER_COLOR, DEFAULT_CODE_NUMBER_COLOR),
  codeTitleColor: readLs(LS_CODE_TITLE_COLOR, DEFAULT_CODE_TITLE_COLOR),
  codeVariableColor: readLs(LS_CODE_VARIABLE_COLOR, DEFAULT_CODE_VARIABLE_COLOR),
  lastDiff: null,
  planDiffs: [],
  topPlanId: null,
  selectedDiffPath: null,
  planVersion: 0,
  configVersion: 0,
  backlog: [],
  autoFeed: false,
  parallelRunAll: false,
  runAll: { active: false, done: 0, total: 0, compacting: false, concurrency: 1, spawned: [], note: null },
  backlogDraft: "",
  backlogDraftImages: [],
  settingsOpen: false,
  settingsSection: null,

  setActiveAgent: (id) => set({ activeAgent: id }),
  setWorkflowState: (id, state) =>
    set((s) => ({ workflowStates: { ...s.workflowStates, [id]: state } })),
  setAgentName: (id, name) =>
    set((s) => ({ agentNames: { ...s.agentNames, [id]: name } })),
  setAgentEffort: (id, effort) =>
    set((s) => ({ agentEfforts: { ...s.agentEfforts, [id]: effort } })),
  clearAgentEfforts: () => set({ agentEfforts: {} }),
  registerAgents: (infos) =>
    set((s) => {
      const agents = { ...s.agents };
      const agentNames = { ...s.agentNames };
      const agentParents = { ...s.agentParents };
      const agentModels = { ...s.agentModels };
      const agentProviders = { ...s.agentProviders };
      const agentWireEfforts = { ...s.agentWireEfforts };
      for (const info of infos) {
        if (!agents[info.id]) {
          agents[info.id] = emptyAgentState();
        }
        agentNames[info.id] = info.name;
        agentParents[info.id] = info.parent_id;
        // Only overwrite the model when the backend reports one — an absent
        // model (agent exited) must not clobber a previously-known model, and
        // a present model always wins (it's the live provider's model).
        if (info.model) {
          agentModels[info.id] = info.model;
        }
        // Same presence-wins semantics for the serving endpoint: a reported
        // name always wins; an absent name (mock-backed loop) never clobbers
        // a previously-known one.
        if (info.provider) {
          agentProviders[info.id] = info.provider;
        }
        // And for the wire-reported effective effort (backlog 51dab4da): a
        // reported effort always wins; an absent one (mock-backed loop)
        // never clobbers a previously-known value.
        if (info.reasoning_effort) {
          agentWireEfforts[info.id] = info.reasoning_effort;
        }
      }
      return {
        agents,
        agentNames,
        agentParents,
        agentModels,
        agentProviders,
        agentWireEfforts,
      };
    }),
  seedContextCaps: (caps) =>
    set((s) => {
      const agents = { ...s.agents };
      for (const [id, max] of caps) {
        const agent = agents[id as AgentId];
        // Skip unknown ids (an agent that exited between listAgents and this
        // call) and preserve the existing used count — a live turn's
        // ContextUsage event may already have reported it, and a late seed
        // must not clobber it with a stale 0.
        if (agent) {
          agents[id as AgentId] = {
            ...agent,
            contextUsage: { used: agent.contextUsage.used, max },
          };
        }
      }
      return { agents };
    }),
  mainAgentId: () => selectMainAgentId(get().agentParents, get().agents),
  setModel: (m) => set({ model: m }),
  setProvider: (p) => set({ provider: p }),
  setReasoningEffort: (e) => set({ reasoningEffort: e }),
  bumpConfigVersion: () => set((s) => ({ configVersion: s.configVersion + 1 })),
  setPricing: (p) => set({ pricing: p }),
  setGitBranch: (b) => set({ gitBranch: b }),
  toggleRightPanel: () =>
    set((s) => ({ rightPanelVisible: !s.rightPanelVisible })),
  setRightPanelWidthFrac: (f) => {
    if (f === null) {
      writeLs(LS_RIGHT_PANEL_WIDTH_FRAC, "");
    } else {
      writeLs(LS_RIGHT_PANEL_WIDTH_FRAC, String(f));
    }
    set({ rightPanelWidthFrac: f });
  },
  /**
   * Update the right-panel width fraction in-memory only (no localStorage
   * write). Used during an active drag so the panel follows the cursor at
   * 60+ Hz without a synchronous localStorage write per pointermove. The
   * final fraction is persisted via `setRightPanelWidthFrac` on drag-end.
   */
  setRightPanelWidthFracLive: (f) => set({ rightPanelWidthFrac: f }),
  setRightPanelTab: (tab) =>
    set((s) =>
      // Ignore attempts to select a disabled tab — the content switch falls
      // back to the first enabled tab, and rightPanelTab must not disagree.
      s.disabledTabs.includes(tab) ? s : { rightPanelTab: tab },
    ),
  selectDiffPath: (path) => set({ selectedDiffPath: path }),
  toggleTab: (tab) =>
    set((s) => {
      const disabled = s.disabledTabs.includes(tab);
      const disabledTabs = disabled
        ? s.disabledTabs.filter((t) => t !== tab)
        : [...s.disabledTabs, tab];
      // If we just disabled the active tab, fall back to the first enabled tab.
      let rightPanelTab = s.rightPanelTab;
      if (!disabled && rightPanelTab === tab) {
        const firstEnabled = ALL_RIGHT_PANEL_TABS.find((t) => !disabledTabs.includes(t));
        if (firstEnabled) rightPanelTab = firstEnabled;
      }
      return { disabledTabs, rightPanelTab };
    }),
  toggleTabAndReveal: (tab) =>
    set((s) => {
      const wasDisabled = s.disabledTabs.includes(tab);
      const disabledTabs = wasDisabled
        ? s.disabledTabs.filter((t) => t !== tab)
        : [...s.disabledTabs, tab];
      // If we just disabled the active tab, fall back to the first enabled tab.
      let rightPanelTab = s.rightPanelTab;
      if (!wasDisabled && rightPanelTab === tab) {
        const firstEnabled = ALL_RIGHT_PANEL_TABS.find((t) => !disabledTabs.includes(t));
        if (firstEnabled) rightPanelTab = firstEnabled;
      }
      // Panel side-effects: turning a tool on reveals the panel so the change
      // is visible; turning off the last enabled tool hides the empty panel.
      const anyEnabled = disabledTabs.length < ALL_RIGHT_PANEL_TABS.length;
      let rightPanelVisible = s.rightPanelVisible;
      if (wasDisabled) {
        rightPanelTab = tab;
        rightPanelVisible = true;
      } else if (!anyEnabled) {
        rightPanelVisible = false;
      }
      return { disabledTabs, rightPanelTab, rightPanelVisible };
    }),
  isTabEnabled: (tab) => !get().disabledTabs.includes(tab),
  revealRightPanelTab: (tab) =>
    set((s) => ({
      // Enable the tab if it was disabled, select it, and reveal the panel.
      // Never disables — a deep-link click should always end on this tab.
      disabledTabs: s.disabledTabs.filter((t) => t !== tab),
      rightPanelTab: tab,
      rightPanelVisible: true,
    })),
  requestFileOpen: (path, line) =>
    set((s) => ({
      // Reveal + select the Files tab (mirrors revealRightPanelTab) and hold
      // the path (+ optional 1-based reveal line) so the FileViewer consumes
      // it on mount/change — no event, so there's no mount race to lose.
      disabledTabs: s.disabledTabs.filter((t) => t !== "files"),
      rightPanelTab: "files",
      rightPanelVisible: true,
      pendingFileOpen: { path, line: line ?? null },
    })),
  clearPendingFileOpen: () => set({ pendingFileOpen: null }),
  requestBrowserOpen: (url) =>
    set((s) => ({
      // Reveal + select the Browser tab (mirrors requestFileOpen) and hold the
      // URL so BrowserView consumes it on mount/change — that view owns the
      // child webview's rect, so ensure + navigate must happen there.
      disabledTabs: s.disabledTabs.filter((t) => t !== "browser"),
      rightPanelTab: "browser",
      rightPanelVisible: true,
      pendingBrowserUrl: url,
    })),
  clearPendingBrowserUrl: () => set({ pendingBrowserUrl: null }),
  setBrowserUrl: (url) => set({ browserUrl: url }),
  autoRevealPlan: () =>
    set((s) => {
      // Only auto-reveal when the panel is currently hidden AND the Plan tab
      // is enabled. If the user has the panel open on another tab, leave it
      // alone — don't yank them away. If the Plan tab is disabled, do nothing.
      if (s.rightPanelVisible || s.disabledTabs.includes("plan")) return s;
      return { rightPanelVisible: true, rightPanelTab: "plan" };
    }),
  autoRevealDiff: () =>
    set((s) => {
      // Only auto-reveal when the panel is hidden and the Diff tab is enabled.
      // Do not yank the user away from a manually chosen tab or visible panel.
      if (s.rightPanelVisible || s.disabledTabs.includes("diff")) return s;
      return { rightPanelVisible: true, rightPanelTab: "diff" };
    }),
  setSafetyMode: (m) => set({ safetyMode: m }),
  setShowTokenUsage: (show) => {
    writeLs(LS_SHOW_TOKEN_USAGE, show ? "true" : "false");
    set({ showTokenUsage: show });
  },
  setShowShellPreview: (show) => {
    writeLs(LS_SHOW_SHELL_PREVIEW, show ? "true" : "false");
    set({ showShellPreview: show });
  },
  setShowToolImages: (show) => set({ showToolImages: show }),
  setShowToolActivity: (show) => set({ showToolActivity: show }),
  setShowKnowledgeActivity: (show) => set({ showKnowledgeActivity: show }),
  setHiddenSteeringNotes: (keys) => set({ hiddenSteeringNotes: keys }),
  setChatThreadLine: (on) => set({ chatThreadLine: on }),
  setChatProseCap: (on) => set({ chatProseCap: on }),
  setChatTurnTint: (on) => set({ chatTurnTint: on }),
  setChatHoverTimestamps: (on) => set({ chatHoverTimestamps: on }),
  setSoundComplete: (on) => set({ soundComplete: on }),
  setSoundInput: (on) => set({ soundInput: on }),
  setSoundDoom: (on) => set({ soundDoom: on }),
  openSettings: (section) =>
    set({
      settingsOpen: true,
      settingsSection: section ?? null,
    }),
  closeSettings: () => set({ settingsOpen: false, settingsSection: null }),
  setBacklog: (items) => set({ backlog: items }),
  setAutoFeed: (enabled) => set({ autoFeed: enabled }),
  setParallelRunAll: (enabled) => set({ parallelRunAll: enabled }),
  setRunAll: (progress) => set({ runAll: progress }),
  applyBacklogChanged: (payload) =>
    set({
      backlog: payload.items,
      autoFeed: payload.auto_feed,
      parallelRunAll: payload.parallel,
      runAll: payload.run_all,
    }),
  setBacklogDraft: (text) => set({ backlogDraft: text }),
  setBacklogDraftImages: (images) => set({ backlogDraftImages: images }),
  setFontFamily: (f) => {
    writeLs(LS_FONT_FAMILY, f);
    applyFontVars(f, get().fontSize);
    set({ fontFamily: f });
  },
  setFontSize: (n) => {
    writeLs(LS_FONT_SIZE, String(n));
    applyFontVars(get().fontFamily, n);
    set({ fontSize: n });
  },
  setTheme: (t) => {
    writeLs(LS_THEME, t);
    applyTheme(t);
    set({ theme: t });
  },
  setAccentColor: (c) => setColor("accentColor", c, get, set),
  setBorderColor: (c) => setColor("borderColor", c, get, set),
  setTextPrimaryColor: (c) => setColor("textPrimaryColor", c, get, set),
  setTextMutedColor: (c) => setColor("textMutedColor", c, get, set),
  setCodeTextColor: (c) => setColor("codeTextColor", c, get, set),
  setCodeCommentColor: (c) => setColor("codeCommentColor", c, get, set),
  setCodeKeywordColor: (c) => setColor("codeKeywordColor", c, get, set),
  setCodeStringColor: (c) => setColor("codeStringColor", c, get, set),
  setCodeNumberColor: (c) => setColor("codeNumberColor", c, get, set),
  setCodeTitleColor: (c) => setColor("codeTitleColor", c, get, set),
  setCodeVariableColor: (c) => setColor("codeVariableColor", c, get, set),
  resetColors: () => {
    const d = defaultColorPrefs();
    writeColorPrefs(d);
    applyColorPrefs(d);
    set(d);
  },
  resetAppearance: () => {
    // Theme + font.
    writeLs(LS_THEME, "dark");
    writeLs(LS_FONT_FAMILY, DEFAULT_FONT_FAMILY);
    writeLs(LS_FONT_SIZE, String(DEFAULT_FONT_SIZE));
    applyTheme("dark");
    applyFontVars(DEFAULT_FONT_FAMILY, DEFAULT_FONT_SIZE);
    // All UI + code-block colors (table-driven).
    const d = defaultColorPrefs();
    writeColorPrefs(d);
    applyColorPrefs(d);
    set({ theme: "dark", fontFamily: DEFAULT_FONT_FAMILY, fontSize: DEFAULT_FONT_SIZE, ...d });
  },

  clearConversation: (id) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      return {
        agents: {
          ...s.agents,
          [id]: {
            ...agent,
            transcript: [],
            streamingText: "",
            streamingReasoning: "",
            activityLog: [],
            // A cleared conversation is a fresh start — the previous failure
            // state (and the Continue affordance) no longer applies.
            failed: false,
            parked: null,
            pendingApproval: null,
            pendingQuestion: null,
            pendingQuestionAnswered: null,
            freeformQuestionId: null,
            steers: [],
            // A cleared conversation starts a fresh session: reset the
            // session-level timing accumulator (totals + the rolling last-3
            // window) + the context breakdown so the status-bar tok/sec +
            // ctx popup reflect the new session.
            sessionTiming: {
              ttft_ms_total: 0,
              generation_ms_total: 0,
              timed_requests: 0,
              prompt_tokens: 0,
              completion_tokens: 0,
              recentTiming: [],
            },
            lastContextBreakdown: null,
          },
        },
      };
    }),

  addSteer: (id, text, images) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      const entry: SteerEntry = {
        id: nextSteerId++,
        text,
        status: "pending",
        timestamp: Date.now(),
        ...(images && images.length > 0 ? { images } : {}),
      };
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, steers: [...agent.steers, entry] },
        },
      };
    }),

  removeSteer: (id, steerId) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, steers: agent.steers.filter((e) => e.id !== steerId) },
        },
      };
    }),

  clearSteers: (id) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, steers: [] },
        },
      };
    }),

  recordQuestionAnswer: (id, payload) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      // Reuse the pure reducer so the qa transcript entry + answered marker
      // logic lives in one testable place (agentEventReducer.ts).
      const { agent: next } = reduceQuestionAnswered(agent, payload);
      return {
        agents: {
          ...s.agents,
          [id]: next,
        },
      };
    }),

  setFreeformQuestion: (id, questionId) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, freeformQuestionId: questionId },
        },
      };
    }),

  appendStreamingText: (id, text) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      return {
        agents: {
          ...s.agents,
          [id]: {
            ...agent,
            streamingText: agent.streamingText + text,
            // Mirror the answer into the reasoning box's activity log as it
            // streams (user request 2026-08-22) — same rule as the reducer
            // path (appendAnswerEntry), so both dispatch routes stay in sync.
            activityLog: appendAnswerEntry(agent.activityLog, text),
          },
        },
      };
    }),

  appendStreamingReasoning: (id, text) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      const streamingReasoning = agent.streamingReasoning + text;
      let activityLog: ActivityEntry[];
      if (
        agent.activityLog.length === 0 ||
        agent.activityLog[agent.activityLog.length - 1].kind !== "reasoning"
      ) {
        activityLog = [
          { kind: "reasoning" as const, text, timestamp: Date.now() },
        ];
      } else {
        const log = [...agent.activityLog];
        log[log.length - 1] = {
          ...log[log.length - 1],
          text: log[log.length - 1].text + text,
        };
        activityLog = log;
      }
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, streamingReasoning, activityLog: capActivityLog(activityLog) },
        },
      };
    }),

  applyToolOutputDeltas: (id, deltas) => {
    if (deltas.length === 0) return;
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      // Clone transcript once for the whole batch.
      const transcript = [...agent.transcript];
      let matched = false;
      for (const delta of deltas) {
        for (let i = transcript.length - 1; i >= 0; i--) {
          const entry = transcript[i];
          if (entry.kind !== "tool") continue;
          const callIdx = entry.calls.findIndex(
            (c) => c.id === delta.tool_call_id && c.result === null,
          );
          if (callIdx === -1) continue;
          const calls = [...entry.calls];
          calls[callIdx] = {
            ...calls[callIdx],
            liveOutput: appendLiveOutputTail(calls[callIdx].liveOutput, delta.text),
          };
          transcript[i] = { ...entry, calls };
          matched = true;
          break;
        }
      }
      // Nothing matched — a chunk for an unknown or already-finished call, the
      // routine case for a batch the dispatcher flushed just ahead of its
      // `tool_result`. Return the CURRENT state object: zustand treats it as
      // "no change" and skips notifying subscribers, so a stray chunk costs no
      // render. The reducer path carries the same no-op contract.
      if (!matched) return s;
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, transcript },
        },
      };
    });
  },

  applyToolCallArgDeltas: (id, deltas) =>
    set((s) => {
      const agent = getOrCreate(s.agents, id);
      if (deltas.length === 0) {
        return { agents: s.agents };
      }
      // Clone transcript once for the whole batch.
      const transcript = [...agent.transcript];
      for (const delta of deltas) {
        for (let i = transcript.length - 1; i >= 0; i--) {
          const entry = transcript[i];
          if (entry.kind !== "tool") continue;
          const callIdx = entry.calls.findIndex(
            (c) => c.index === delta.index && c.result === null,
          );
          if (callIdx !== -1) {
            const calls = [...entry.calls];
            calls[callIdx] = {
              ...calls[callIdx],
              args: calls[callIdx].args + delta.fragment,
            };
            transcript[i] = { ...entry, calls };
            break;
          }
        }
      }
      return {
        agents: {
          ...s.agents,
          [id]: { ...agent, transcript },
        },
      };
    }),

  handleAgentEvent: ({ agent_id, event }) => {
    // Capture the reducer's effects so side effects (like the landed-steer
    // auto-removal timer) use the EXACT steer the reducer marked landed —
    // not a re-query that could mis-target a different landed steer when two
    // land in quick succession.
    let effects: Effects | undefined;
    set((s) => {
      const r = applyAgentEvent(s, agent_id, event);
      effects = r.effects;
      return r.next;
    });
    // The reducer is pure, so the landed-steer auto-removal timer (a side
    // effect) is scheduled here, outside the state computation, using the
    // steerId the reducer computed.
    const steerId = effects?.scheduleSteerRemoval?.steerId;
    if (event.kind === "suggestion_injected" && steerId !== undefined) {
        setTimeout(() => {
          useAgentStore.setState((st) => {
            const a = st.agents[agent_id];
            if (!a) return st;
            return {
              ...st,
              agents: {
                ...st.agents,
                [agent_id]: {
                  ...a,
                  steers: a.steers.filter((e) => e.id !== steerId),
                },
              },
            };
          });
        }, STEER_LANDED_TTL_MS);
    }
    // Notification sound (completion ding / needs-input ping / doom): the
    // reducer requested it via Effects.sound; the per-sound enable flags
    // (Settings → Sounds, persisted in config.toml [ui]) gate it here.
    // Reading the flags from getState AFTER the set above so a just-saved
    // toggle applies to the very next event.
    const sound = effects?.sound;
    if (sound !== undefined) {
      const st = useAgentStore.getState();
      if (
        soundEnabled(sound, {
          soundComplete: st.soundComplete,
          soundInput: st.soundInput,
          soundDoom: st.soundDoom,
        })
      ) {
        playSound(sound);
      }
    }
  },
}));

export type { AppState, Effects };

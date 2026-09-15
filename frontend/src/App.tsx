// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow, availableMonitors } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { useAgentEvents } from "./hooks/useAgentEvents";
import { useAgentStore, applyFontVars, applyTheme, applyColors, applyCodeColors, didMainTurnEnd } from "./hooks/useAgentStore";
import {
  getGitBranch,
  getStartupSnapshot,
  getSafetyMode,
  getStartupError,
  getSettings,
  getNeedsProject,
  getEmbedderStatus,
  onEmbedderStatus,
  onReconcileEvent,
} from "./lib/tauri";
import type { ReconcileEvent, InstanceConflict } from "./lib/tauri";
import type { WorkflowState } from "./lib/types";
import { fmtPct } from "./lib/format";
import { hiddenKeysFromConfig } from "./lib/delegationNotes";
import { clampRestoredGeometry } from "./lib/windowRestore";
import { Sidebar } from "./components/layout/Sidebar";
import { MainPanel } from "./components/layout/MainPanel";
import { RightPanel } from "./components/layout/RightPanel";
import { StatusBar } from "./components/layout/StatusBar";
import { InputBar } from "./components/layout/InputBar";
import { InflightBar } from "./components/chat/InflightBar";
import { ProjectPicker } from "./components/projects/ProjectPicker";
import { InstanceConflictDialog } from "./components/projects/InstanceConflictDialog";
import { IndexingOverlay } from "./components/projects/IndexingOverlay";
import { BootSplash } from "./components/common/BootSplash";
import { SplashCard, SplashProgress } from "./components/common/SplashCard";
import { useBrowserOverlay } from "./hooks/useBrowserOverlay";
import { AlertTriangle, RefreshCw } from "lucide-react";

/** localStorage key for the persisted window geometry (x, y, w, h, maximized). */
const LS_WINDOW_GEOMETRY = "mh.windowGeometry";

/**
 * The persisted window geometry, in LOGICAL px. `width`/`height` are the OUTER
 * frame size (the save path reads `outerSize()`); the restore converts them to
 * an inner size via the measured chrome — see lib/windowRestore.ts.
 */
interface PersistedGeometry {
  x: number;
  y: number;
  width: number;
  height: number;
  maximized: boolean;
}

/** Read persisted window geometry from localStorage (null if absent/invalid). */
function readWindowGeometry(): PersistedGeometry | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(LS_WINDOW_GEOMETRY);
    if (!raw) return null;
    const g = JSON.parse(raw) as Partial<PersistedGeometry>;
    if (
      typeof g.x !== "number" ||
      typeof g.y !== "number" ||
      typeof g.width !== "number" ||
      typeof g.height !== "number"
    ) {
      return null;
    }
    return {
      x: g.x,
      y: g.y,
      width: g.width,
      height: g.height,
      maximized: typeof g.maximized === "boolean" ? g.maximized : false,
    };
  } catch {
    return null;
  }
}

/** Persist the current window geometry to localStorage. */
function writeWindowGeometry(g: PersistedGeometry): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(LS_WINDOW_GEOMETRY, JSON.stringify(g));
  } catch {
    // ignore quota / privacy-mode errors
  }
}

export default function App() {
  useAgentEvents();
  const activeAgent = useAgentStore((s) => s.activeAgent);
  const rightPanelVisible = useAgentStore((s) => s.rightPanelVisible);
  const rightPanelWidth = useAgentStore((s) => s.rightPanelWidth);
  const setRightPanelWidth = useAgentStore((s) => s.setRightPanelWidth);
  const setRightPanelWidthLive = useAgentStore((s) => s.setRightPanelWidthLive);
  const setGitBranch = useAgentStore((s) => s.setGitBranch);
  const setActiveAgent = useAgentStore((s) => s.setActiveAgent);
  const setModel = useAgentStore((s) => s.setModel);
  const setProvider = useAgentStore((s) => s.setProvider);

  const [startupError, setStartupError] = useState<string | null>(null);
  const [needsProject, setNeedsProject] = useState(false);
  const [checkedStartup, setCheckedStartup] = useState(false);
  const [embedderStatus, setEmbedderStatus] = useState<string | Record<string, unknown> | null>(null);
  const [embedderBannerDismissed, setEmbedderBannerDismissed] = useState(false);
  // Same-project instance conflict (startup snapshot): another LIVE mnemo
  // instance holds this project — the dialog asks before opening. The
  // [Choose another project] flow opens the picker on top of it (the picker
  // restarts the app into the chosen project).
  const [instanceConflict, setInstanceConflict] = useState<InstanceConflict | null>(null);
  const [instanceConflictPicker, setInstanceConflictPicker] = useState(false);
  // The startup reconciliation dialog: `null` = no reconcile running (silent
  // when the corpus is in sync — no events, no dialog). A `done`/`failed`
  // event leaves the dialog visible until the user dismisses it (the
  // summary/error line doubles as the result line); `started` + `progress`
  // drive the bar.
  const [reconcile, setReconcile] = useState<
    | { phase: "running"; done: number; total: number }
    | { phase: "done"; summary: string }
    | { phase: "failed"; error: string }
    | null
  >(null);

  // The reconcile dialog and the instance-conflict dialog are full-viewport
  // modals — hide the native child WebView2 while either is visible (the
  // Browser-tab HWND would otherwise punch through), like IndexingOverlay and
  // every other modal. Called with the hooks at the top (before the
  // early-return branches) per rules of hooks.
  useBrowserOverlay(reconcile !== null || instanceConflict !== null);

  // Subscribe to the startup reconciliation stream once. The backend emits
  // ONLY when the corpus drifted (git merge / edit / deleted DB) — an
  // in-sync startup stays completely silent.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    onReconcileEvent((e: ReconcileEvent) => {
      if (disposed) return;
      switch (e.type) {
        case "started":
          setReconcile({ phase: "running", done: 0, total: 0 });
          break;
        case "progress":
          setReconcile((prev) =>
            prev?.phase === "running" || prev === null
              ? { phase: "running", done: e.done, total: e.total }
              : prev,
          );
          break;
        case "done":
          setReconcile({ phase: "done", summary: e.summary });
          break;
        case "failed":
          setReconcile({ phase: "failed", error: e.error });
          break;
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // On mount: check for a startup error first. If the brain failed to build,
  // show an error screen instead of the normal UI.
  useEffect(() => {
    // Restore persisted font + theme + color preferences before first paint.
    const {
      fontFamily,
      fontSize,
      theme,
      accentColor,
      borderColor,
      textPrimaryColor,
      textMutedColor,
      codeTextColor,
      codeCommentColor,
      codeKeywordColor,
      codeStringColor,
      codeNumberColor,
      codeTitleColor,
      codeVariableColor,
    } = useAgentStore.getState();
    applyFontVars(fontFamily, fontSize);
    applyTheme(theme);
    // Keep "system" theme in sync with OS preference changes.
    const mq =
      typeof window !== "undefined"
        ? window.matchMedia("(prefers-color-scheme: light)")
        : null;
    const onScheme = () => {
      if (useAgentStore.getState().theme === "system") applyTheme("system");
    };
    mq?.addEventListener?.("change", onScheme);
    applyColors(accentColor, borderColor, textPrimaryColor, textMutedColor);
    applyCodeColors(
      codeTextColor,
      codeCommentColor,
      codeKeywordColor,
      codeStringColor,
      codeNumberColor,
      codeTitleColor,
      codeVariableColor,
    );

    (async () => {
      // Check for the needs-project state FIRST: if the app started outside
      // any project, show the project picker instead of the normal UI (and
      // skip the startup-error check + normal state fetches below).
      try {
        const needs = await getNeedsProject();
        if (needs) {
          setNeedsProject(true);
          setCheckedStartup(true);
          return;
        }
      } catch (e) {
        console.error("failed to check needs_project:", e);
      }
      try {
        const err = await getStartupError();
        if (err) {
          setStartupError(err);
          setCheckedStartup(true);
          return;
        }
      } catch (e) {
        console.error("failed to check startup error:", e);
      }
      setCheckedStartup(true);

      // Normal startup — fetch the startup snapshot (agents, context caps,
      // ALL workflow states, backlog, embedder status) in ONE call. This
      // replaces 5 sequential invoke round-trips and closes the
      // stale-non-active-workflowStates gap (the old startup fetched only the
      // active agent's state).
      try {
        const snap = await getStartupSnapshot();
        if (snap.agents.length > 0) {
          useAgentStore.getState().registerAgents(snap.agents);
          if (activeAgent === null) setActiveAgent(snap.agents[0].id);
        }
        useAgentStore.getState().seedContextCaps(snap.context_caps);
        // Seed EVERY agent's workflow state (not just the active one).
        for (const [id, state] of snap.workflow_states) {
          useAgentStore.getState().setWorkflowState(id, state as WorkflowState);
        }
        // Seed the embedder status from the snapshot.
        setEmbedderStatus(snap.embedder_status);
        // Same-project instance conflict (backend field, resolved from
        // `.coding/instance.json` at startup before this instance's marker
        // overwrote the incumbent's). `?? null` also tolerates an older
        // backend that predates the field.
        setInstanceConflict(snap.instance_conflict ?? null);
        // Seed the backlog from the snapshot (L2 — spec required this; the
        // backlog_changed event + backlogList() poll still catch later updates).
        useAgentStore.getState().setBacklog(snap.backlog);
      } catch (e) {
        console.error("failed to fetch startup snapshot:", e);
      }
      try {
        const branch = await getGitBranch();
        setGitBranch(branch);
      } catch (e) {
        console.error("failed to get git branch:", e);
      }
      try {
        // E5: get_settings is a strict superset of the old get_config
        // (general.default_model/provider, endpoints, pricing all present).
        // Consolidated the old getConfig + getSettings into one call.
        const settings = await getSettings();
        if (settings.general?.default_model) setModel(settings.general.default_model);
        // Show the resolved default provider's name (fall back to the first
        // endpoint), not just its kind.
        const defaultEndpoint =
          settings.endpoints?.find((e) => e.name === settings.general?.default_provider) ??
          settings.endpoints?.[0];
        if (defaultEndpoint) {
          setProvider(defaultEndpoint.name);
          // Seed the reasoning-effort dropdown from the endpoint's config
          // (default "max" when unset; "off" when the endpoint does not
          // support the parameter). The backend builds the initial provider
          // with the same value, so the dropdown always reflects what's
          // actually being sent.
          const setReasoningEffortStore = useAgentStore.getState().setReasoningEffort;
          setReasoningEffortStore(
            defaultEndpoint.supports_reasoning_effort === false
              ? "off"
              : (defaultEndpoint.reasoning_effort ?? "max"),
          );
        }
        // Load pricing entries into the store (for cost estimation in Stats).
        if (settings.pricing) {
          useAgentStore.getState().setPricing(settings.pricing);
        }
        // Prefer backend [ui] when localStorage never set a preference
        // (localStorage still wins for theme if the user picked one).
        if (settings.ui) {
          const store = useAgentStore.getState();
          // Only apply backend theme if localStorage has no mh.theme key.
          try {
            if (!window.localStorage.getItem("mh.theme") && settings.ui.theme) {
              const t = settings.ui.theme;
              if (t === "dark" || t === "light" || t === "system") {
                store.setTheme(t);
              }
            }
          } catch {
            /* ignore */
          }
          try {
            if (!window.localStorage.getItem("mh.showTokenUsage")) {
              store.setShowTokenUsage(!!settings.ui.show_token_usage);
            }
          } catch {
            /* ignore */
          }
          // Agent-activity cards: config.toml [ui] is the single persisted
          // source (no localStorage mirror), so hydration always applies —
          // an absent field (older configs) reads as OFF (default).
          try {
            store.setShowToolActivity(!!settings.ui.show_tool_activity);
          } catch {
            /* ignore */
          }
          // Knowledge-activity cards: config.toml [ui] is the single persisted
          // source (no localStorage mirror), so hydration always applies — an
          // absent field (older configs) reads as ON (default).
          try {
            store.setShowKnowledgeActivity(settings.ui.show_knowledge_activity !== false);
          } catch {
            /* ignore */
          }
          // Steering notes: config.toml [ui] is the single persisted source
          // (no localStorage mirror), so hydration always applies — an absent
          // field (older configs) reads the registry defaults (only
          // auto-delegated hidden), seeded by the legacy single toggle so a
          // `show_delegation_notes = true` preference survives an upgrade.
          try {
            store.setHiddenSteeringNotes(
              hiddenKeysFromConfig(
                settings.ui.steering_notes,
                settings.ui.show_delegation_notes,
              ),
            );
          } catch {
            /* ignore */
          }
          // Chat-readability affordances (thread line, prose cap, turn tint,
          // hover timestamps): config.toml [ui] is the single persisted source
          // (no localStorage mirror), so hydration always applies — an absent
          // field (older configs) reads as ON (default).
          try {
            store.setChatThreadLine(settings.ui.chat_thread_line !== false);
            store.setChatProseCap(settings.ui.chat_prose_cap !== false);
            store.setChatTurnTint(settings.ui.chat_turn_tint !== false);
            store.setChatHoverTimestamps(settings.ui.chat_hover_timestamps !== false);
          } catch {
            /* ignore */
          }
          // Inline tool images: config.toml [ui] is the single persisted
          // source (no localStorage mirror), so hydration always applies —
          // an absent field (older configs) reads as ON (default).
          try {
            store.setShowToolImages(settings.ui.show_tool_images !== false);
          } catch {
            /* ignore */
          }
          // Notification-sound flags: config.toml [ui] is the single
          // persisted source (no localStorage mirror), so hydration always
          // applies — unlike the two flags above, there is no local override
          // to respect.
          try {
            store.setSoundComplete(!!settings.ui.sound_complete);
            store.setSoundInput(!!settings.ui.sound_input_needed);
            store.setSoundDoom(!!settings.ui.sound_stopped_errors);
          } catch {
            /* ignore */
          }
        }
      } catch (e) {
        console.error("failed to get settings ui:", e);
      }
      try {
        const mode = await getSafetyMode();
        useAgentStore.setState({ safetyMode: mode });
      } catch (e) {
        console.error("failed to get safety mode:", e);
      }
    })();
    // Subscribe to live embedder status updates (startup probe result,
    // circuit-breaker transitions). Set up BEFORE the initial fetch above so
    // no emit is missed in the gap between fetch-read and subscribe-complete.
    let unlisten: (() => void) | null = null;
    onEmbedderStatus((status) => {
      setEmbedderStatus(status);
      // Re-show the banner when the status changes to a non-ready state.
      if (status !== "ready") setEmbedderBannerDismissed(false);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      mq?.removeEventListener?.("change", onScheme);
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Poll the embedder status every 10s to catch post-startup circuit-breaker
  // transitions (the event subscription only catches the startup probe emit;
  // the circuit breaker updates the shared Arc directly without emitting).
  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const status = await getEmbedderStatus();
        if (!cancelled) {
          setEmbedderStatus(status);
          if (status !== "ready") setEmbedderBannerDismissed(false);
        }
      } catch (e) {
        console.error("failed to poll embedder status:", e);
      }
    };
    const interval = window.setInterval(refresh, 10_000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // ...mount, but the branch can change afterward (e.g. an external `git checkout`
  // while the app is open, or a commit made by the agent itself). Re-read it on
  // a slow 60 s poll, on window focus, and when the main agent's turn resolves
  // (running true→false — the agent may have committed/checked out mid-run).
  // F1b (2026-04-19 freeze diagnosis): the old 5 s poll launched a git
  // subprocess 12× more often than needed; the turn-end refresh below keeps
  // post-commit freshness without the constant churn.
  const refreshGitBranch = useCallback(async () => {
    try {
      setGitBranch(await getGitBranch());
    } catch (e) {
      console.error("failed to refresh git branch:", e);
    }
  }, []);
  useEffect(() => {
    // Unmount guard: a late getGitBranch() resolution must not setState (and
    // its IPC is skipped entirely) after the effect tears down.
    let disposed = false;
    const refresh = () => {
      if (!disposed) void refreshGitBranch();
    };
    const interval = window.setInterval(refresh, 60_000);
    window.addEventListener("focus", refresh);
    return () => {
      disposed = true;
      window.clearInterval(interval);
      window.removeEventListener("focus", refresh);
    };
  }, [refreshGitBranch]);
  // Turn-end refresh: watch the MAIN agent's `running` flag; on a true→false
  // edge the turn resolved — re-read the branch immediately so run-driven
  // commits/checkouts show without waiting for the next poll tick.
  useEffect(() => {
    const unsub = useAgentStore.subscribe((s, prev) => {
      if (didMainTurnEnd(prev, s)) void refreshGitBranch();
    });
    return unsub;
  }, [refreshGitBranch]);

  // Persist + restore the window's size + position across restarts. On mount,
  // if we saved geometry last time and it wasn't maximized, re-apply it via the
  // Tauri window API — CLAMPED to be usable on the current monitor layout
  // (lib/windowRestore.ts: minimum size + fully on screen; a save from another
  // DPI / resolution / monitor set must never come back tiny or off screen).
  // Then listen for the window's own resize/move events (debounced) and save
  // the current bounds — but skip saving while maximized so the last *normal*
  // bounds are kept.
  useEffect(() => {
    const win = getCurrentWindow();
    let restoreDone = false;
    // The last-known logical bounds, cached from the onMoved/onResized
    // payloads. `beforeunload` can't await an IPC round-trip (the browser
    // won't wait for the promise), so we write these cached bounds
    // synchronously on close instead of re-querying the window.
    let lastBounds: PersistedGeometry | null = null;

    // Restore saved geometry once on mount.
    (async () => {
      try {
        const saved = readWindowGeometry();
        if (saved && !saved.maximized) {
          // Guard against stale geometry: the saved coords come from a
          // previous session and may not exist on the current monitor layout
          // (monitor disconnected, resolution/DPI changed). Two failure modes,
          // both handled by clamping instead of trusting the save:
          //  - size: a save from another DPI/resolution comes back unusably
          //    small. The Rust-side min_inner_size is no help — it bounds only
          //    user-driven resizing (tao applies it as a WM_GETMINMAXINFO
          //    tracking size), while a programmatic setSize goes straight
          //    through to SetWindowPos unclamped.
          //  - position: coords that intersect no monitor make the window
          //    invisible (it still shows a taskbar button + a live preview,
          //    and clicking that cannot bring it back), and a rect clipping a
          //    monitor edge shows partly off screen.
          // clampRestoredGeometry handles both: the size is clamped into
          // [800×560 … the anchor monitor's work area], and the position is
          // pulled fully inside that work area — re-centered in it when the
          // saved rect can't clear the usability gate. Only a total
          // monitor-enumeration failure leaves the position to the OS default.
          // Startup only: this never constrains interactive resizing.
          //
          // Each probe degrades on its own: an unreadable scale factor reads
          // as 1 and a failed monitor enumeration as none (the size is then
          // bounded by the minimum only — never shrunk), so one unavailable
          // API cannot restore garbage.
          const factor = await win.scaleFactor().catch((e) => {
            console.error("failed to read the window scale factor; assuming 1:", e);
            return 1;
          });
          const monitors = await availableMonitors().catch((e) => {
            console.error("failed to enumerate monitors; bounding the size only:", e);
            return [];
          });
          // Chrome = outer − inner. The save records OUTER bounds while
          // setSize takes an inner (client) size that tao expands to the frame,
          // so without this correction the window would GROW by the
          // title-bar/border height on every restart.
          const chrome = await Promise.all([win.outerSize(), win.innerSize()])
            .then(([outer, inner]) => ({
              width: Math.max(0, outer.width - inner.width),
              height: Math.max(0, outer.height - inner.height),
            }))
            .catch((e) => {
              console.error("failed to measure the window chrome; assuming none:", e);
              return { width: 0, height: 0 };
            });
          const restored = clampRestoredGeometry({ saved, factor, monitors, chrome });
          if (restored.x !== null && restored.y !== null) {
            await win.setPosition(new LogicalPosition(restored.x, restored.y));
          } else {
            console.warn("no monitors reported; leaving the window at the OS default", saved);
          }
          await win.setSize(new LogicalSize(restored.width, restored.height));
        }
      } catch (e) {
        console.error("failed to restore window geometry:", e);
      } finally {
        restoreDone = true;
      }
    })();

    let saveTimer: number | null = null;
    const save = async () => {
      if (!restoreDone) return;
      try {
        const maximized = await win.isMaximized();
        // Keep the last normal bounds when maximized — don't overwrite them.
        if (maximized) return;
        const pos = await win.outerPosition();
        const size = await win.outerSize();
        // outerPosition/outerSize are physical pixels; convert to logical so
        // they round-trip correctly across DPI changes.
        const factor = await win.scaleFactor();
        const bounds: PersistedGeometry = {
          x: pos.x / factor,
          y: pos.y / factor,
          width: size.width / factor,
          height: size.height / factor,
          maximized: false,
        };
        lastBounds = bounds;
        writeWindowGeometry(bounds);
      } catch (e) {
        console.error("failed to save window geometry:", e);
      }
    };
    const debouncedSave = () => {
      if (saveTimer !== null) window.clearTimeout(saveTimer);
      saveTimer = window.setTimeout(save, 400);
    };

    // Tauri emits `tauri://resize` and `tauri://move` on the window. Use a
    // `cancelled` flag so that if the effect cleanup runs before a
    // registration promise resolves (React StrictMode dev double-invoke, or a
    // fast unmount), the listener is unlistened immediately instead of
    // leaking against a dead closure.
    let cancelled = false;
    let unlistenResize: (() => void) | null = null;
    let unlistenMove: (() => void) | null = null;
    win
      .onResized(() => debouncedSave())
      .then((u) => {
        if (cancelled) u();
        else unlistenResize = u;
      })
      .catch(() => {});
    win
      .onMoved(() => debouncedSave())
      .then((u) => {
        if (cancelled) u();
        else unlistenMove = u;
      })
      .catch(() => {});

    // Save on close. `beforeunload` can't await an IPC round-trip, so write
    // the last-known cached bounds synchronously (no IPC) — this captures the
    // final position/size even when the close happens within the 400 ms
    // debounce window.
    const onBeforeUnload = () => {
      if (lastBounds) writeWindowGeometry(lastBounds);
    };
    window.addEventListener("beforeunload", onBeforeUnload);

    return () => {
      cancelled = true;
      if (saveTimer !== null) window.clearTimeout(saveTimer);
      if (unlistenResize) unlistenResize();
      if (unlistenMove) unlistenMove();
      window.removeEventListener("beforeunload", onBeforeUnload);
    };
  }, []);

  // Boot splash — the startup checks (needs_project / startup snapshot)
  // wait on the backend while build_brain finishes; without this, App's
  // empty main-UI shell flashes during the pre-index boot phases (backlog
  // 486955d5). The static twin in index.html covers the pre-React window.
  // The tracked reconcile state rides along: a slow startup derived-index
  // reconcile (drifted memory DB) renders its phase + bar inside the
  // splash instead of a generic spinner — the reconcile is spawned inside
  // build_brain before it returns, and the checks cannot resolve before
  // build_brain returns, so the boot window always covers the reconcile's
  // start; one that outlives it hands off to the post-boot reconcile dialog.
  if (!checkedStartup) {
    return <BootSplash reconcile={reconcile} />;
  }

  // Project picker — the app started outside any project. Shown before the
  // startup-error screen (a needs-project state is not an error). Full-screen,
  // no close button: the only exit is picking/creating a project (which
  // restarts the app into it).
  if (checkedStartup && needsProject) {
    return <ProjectPicker />;
  }

  // Startup error screen — the brain failed to build, but the window is open.
  if (checkedStartup && startupError) {
    return (
      <div className="flex h-screen w-screen flex-col items-center justify-center gap-4 bg-bg-primary p-8 text-center">
        <AlertTriangle className="h-12 w-12 text-red-400" />
        <h1 className="text-lg font-semibold text-red-400">
          The agent brain failed to start
        </h1>
        <p className="max-w-xl text-sm text-slate-400">
          The window is still open, but the agent can't run until this is fixed.
          Fix the code and restart the app.
        </p>
        <pre className="max-w-2xl overflow-auto rounded-lg border border-red-600/40 bg-red-950/20 p-4 text-left text-xs text-red-300">
          {startupError}
        </pre>
        <button
          onClick={() => window.location.reload()}
          className="mt-2 flex items-center gap-2 rounded-lg bg-cyan-600 px-4 py-2 text-sm font-medium text-white hover:bg-cyan-500"
        >
          <RefreshCw className="h-4 w-4" />
          Reload
        </button>
      </div>
    );
  }

  return (
    <div className="flex h-screen w-screen bg-bg-primary text-slate-200">
      {/* Startup reconciliation wait dialog — visible only while (or right
          after) the derived index syncs with the on-disk truth. */}
      {reconcile && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
          <SplashCard>
            <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-slate-200">
              <RefreshCw
                className={`h-4 w-4 text-cyan-400 ${
                  reconcile.phase === "running" ? "animate-spin" : ""
                }`}
              />
              Rebuilding the semantic memory index
            </div>
            {reconcile.phase === "running" && (
              <SplashProgress
                label="Reconciling sources…"
                right={
                  reconcile.total > 0
                    ? `${reconcile.done}/${reconcile.total}`
                    : "…"
                }
                pct={
                  reconcile.total > 0
                    ? Math.round((reconcile.done / reconcile.total) * 100)
                    : null
                }
              />
            )}
            {reconcile.phase === "done" && (
              <div className="mb-3 text-xs text-green-400">{reconcile.summary}</div>
            )}
            {reconcile.phase === "failed" && (
              <div className="mb-3 text-xs text-red-400">{reconcile.error}</div>
            )}
            {reconcile.phase !== "running" && (
              <button
                onClick={() => setReconcile(null)}
                className="mt-2 w-full rounded-lg bg-cyan-600 px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-cyan-500"
              >
                Dismiss
              </button>
            )}
          </SplashCard>
        </div>
      )}
      {/* Same-project instance conflict — a second instance on a LIVE
          project: ask before opening (two instances on one project are
          supported now, but two agents on the same files can step on each
          other). The picker variant runs on top until a project is chosen
          (it restarts the app into it). */}
      {instanceConflict !== null && !instanceConflictPicker && (
        <InstanceConflictDialog
          conflict={instanceConflict}
          onDismiss={() => setInstanceConflict(null)}
          onChooseProject={() => setInstanceConflictPicker(true)}
        />
      )}
      {instanceConflictPicker && (
        <ProjectPicker onClose={() => setInstanceConflictPicker(false)} />
      )}
      {/* Indexing progress overlay — visible only while the startup/create
          code-index pass runs (self-subscribing; hidden otherwise). */}
      <IndexingOverlay />
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        {embedderStatus &&
          embedderStatus !== "ready" &&
          !embedderBannerDismissed && (
            <div
              className={`flex items-center gap-2 px-3 py-1.5 text-xs ${
                embedderStatus === "failed"
                  ? "border-b border-red-500/30 bg-red-500/10 text-red-300"
                  : typeof embedderStatus === "object" && "downloading" in embedderStatus
                    ? "border-b border-cyan-500/30 bg-cyan-500/10 text-cyan-300"
                    : "border-b border-amber-500/30 bg-amber-500/10 text-amber-300"
              }`}
            >
              <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
              <span className="flex-1">
                {embedderStatus === "fallback" &&
                  "Memory embeddings fell back to keyword-only — the embedding service is unreachable. Recall still works; semantic ranking is off."}
                {embedderStatus === "pulling" &&
                  "Pulling the embedding model in the background — semantic recall will activate once the download completes."}
                {embedderStatus === "checking" &&
                  "Checking the embedding service…"}
                {embedderStatus === "failed" &&
                  "Embedding service failed — check your endpoint + model in Settings → Embeddings."}
                {typeof embedderStatus === "object" &&
                  "downloading" in embedderStatus &&
                  `Downloading embedding model ${
                    (embedderStatus as { downloading: { model: string; progress: number } }).downloading.model
                  }… ${fmtPct(
                    (embedderStatus as { downloading: { model: string; progress: number } }).downloading.progress * 100,
                  )}%`}
              </span>
              <button
                type="button"
                onClick={() => setEmbedderBannerDismissed(true)}
                className="shrink-0 text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--text-primary)]"
                title="Dismiss"
              >
                ✕
              </button>
            </div>
          )}
        <MainPanel />
        <InflightBar agentId={activeAgent} />
        <InputBar />
        <StatusBar />
      </div>
      {rightPanelVisible && (
        <>
          <ResizeHandle
            width={rightPanelWidth}
            onWidthChangeLive={setRightPanelWidthLive}
            onWidthCommit={setRightPanelWidth}
          />
          <RightPanel />
        </>
      )}
    </div>
  );
}

/**
 * A vertical drag handle between the main column and the right tools panel.
 * Dragging left/right resizes the panel; the width is clamped to
 * [300, window.innerWidth * 0.8]. The store state updates live during the
 * drag (so the panel follows the cursor), but localStorage is only written on
 * drag-end (via the store setter) to avoid a synchronous write per
 * pointermove. A `useEffect` cleans up any in-flight drag listeners if the
 * component unmounts mid-drag (e.g. the panel is hidden).
 *
 * The handle shows the app's standard resize-affordance motif — a centered
 * grip pill plus a subtle cyan hover wash — matching the InflightBar and
 * FileViewer resize handles. It also carries 1px border-x hairlines in the
 * standard border color so the agent-column and tools-panel regions read as
 * separated even at rest (the panels themselves draw no border here). The
 * `w-1.5` column it occupies (wide enough that the pill stays visually
 * distinct from the hairlines) plus a wider invisible hit area keep it easy
 * to grab.
 */
function ResizeHandle({
  width,
  onWidthChangeLive,
  onWidthCommit,
}: {
  width: number | null;
  /** Live (in-memory only) width update — called on every pointermove. */
  onWidthChangeLive: (w: number) => void;
  /** Persist the final width (writes localStorage) — called on drag-end. */
  onWidthCommit: (w: number | null) => void;
}) {
  // Track the active drag's window listeners so a `useEffect` cleanup can
  // tear them down if the component unmounts mid-drag.
  const cleanupRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    return () => {
      // Unmount mid-drag: remove any leaked window listeners.
      cleanupRef.current?.();
      cleanupRef.current = null;
    };
  }, []);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    // The panel's current pixel width (fall back to measuring it from the DOM
    // when the user hasn't set an explicit width yet).
    const panel = (e.currentTarget.nextElementSibling as HTMLElement) ?? null;
    const startWidth = width ?? panel?.getBoundingClientRect().width ?? 480;
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);

    const clamp = (px: number) => {
      const max = window.innerWidth * 0.8;
      return Math.max(300, Math.min(max, px));
    };

    const onMove = (ev: PointerEvent) => {
      // Dragging left grows the panel (the panel is on the right edge).
      const delta = startX - ev.clientX;
      onWidthChangeLive(Math.round(clamp(startWidth + delta)));
    };
    const endDrag = (ev: PointerEvent) => {
      try {
        handle.releasePointerCapture(ev.pointerId);
      } catch {
        // Capture may already be released (e.g. pointercancel) — ignore.
      }
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
      cleanupRef.current = null;
      // Persist the final width (the store setter writes to localStorage).
      const delta = startX - ev.clientX;
      onWidthCommit(Math.round(clamp(startWidth + delta)));
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", endDrag);
    // pointercancel fires when the OS interrupts the pointer (touch gesture,
    // system event) instead of pointerup — route it to the same cleanup so
    // the listeners don't leak.
    window.addEventListener("pointercancel", endDrag);
    cleanupRef.current = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endDrag);
      window.removeEventListener("pointercancel", endDrag);
    };
  };

  return (
    <div
      onPointerDown={onPointerDown}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize tools panel"
      title="Drag to resize the tools panel"
      className="group relative flex w-1.5 shrink-0 cursor-col-resize items-center justify-center border-x border-border bg-bg-secondary transition-colors hover:bg-cyan-500/20"
    >
      {/* Wider invisible hit area so the handle is easy to grab; the grip
          pill below + the hover wash are the visible affordance. */}
      <div className="absolute inset-y-0 -left-1 -right-1" />
      {/* Centered grip pill — the small highlight line shared with the
          InflightBar and FileViewer resize handles. The w-1.5 column keeps
          a gap between the pill and the border-x hairlines so the caret
          reads as a distinct mark, not part of the border; it paints the
          chrome-band background (bg-bg-secondary) rather than staying
          transparent — every resize handle in the app paints
          bg-bg-secondary between its hairlines so the seam color is the
          token, not whatever surface happens to sit behind the strip (see
          resizeHandleMotif.test.ts). */}
      <div className="pointer-events-none h-8 w-0.5 rounded-full bg-slate-500 group-hover:bg-slate-400" />
    </div>
  );
}

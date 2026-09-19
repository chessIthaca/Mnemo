// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState } from "react";
import { FolderPlus, FolderOpen, Trash2, X, AlertTriangle } from "lucide-react";
import {
  listProjects,
  createProject,
  switchProject,
  removeProject,
  pickDirectory,
  type ProjectInfo,
} from "../../lib/tauri";
import { flushWindowGeometry } from "../../lib/windowGeometryFlush";
import {
  createButtonLabel,
  openButtonLabel,
  type BusyPhase,
} from "./projectPickerPhases";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "../ui/dialog";
import { IndexingOverlay } from "./IndexingOverlay";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";

/**
 * The project picker — choose an existing registered project or create a new
 * one by picking a directory (which scaffolds `.coding/` + `agent.md` and
 * registers it). Switching projects restarts the app into the chosen project.
 *
 * Two modes:
 * - **Startup** (`onClose` omitted): full-screen, no close button. Shown when
 *   the app starts outside any project. The only way out is to pick/create a
 *   project (which restarts) — there is no "cancel into an empty app".
 * - **Switch** (`onClose` provided): a centered Dialog with a close button,
 *   opened from the Sidebar's switch-project control while a project is open.
 */
export function ProjectPicker({ onClose }: { onClose?: () => void }) {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Hide the native child WebView2 while this full-viewport modal is open
  // (it's a separate HWND composited above the app's HTML — see useBrowserOverlay).
  // ProjectPicker is mounted/unmounted (no `open` prop), so it's always "open"
  // while mounted — pass true so the overlay enter/exit brackets the mount.
  useBrowserOverlay(true);

  // Create-new-project form state.
  const [chosenPath, setChosenPath] = useState<string | null>(null);
  const [name, setName] = useState<string>("");
  // The busy phase (backlog 486955d5): which long-running action is in
  // flight — surfaced on the action buttons via createButtonLabel /
  // openButtonLabel instead of a bare "Working…".
  const [phase, setPhase] = useState<BusyPhase | null>(null);
  const busy = phase !== null;

  const startup = onClose === undefined;

  async function refresh() {
    try {
      setProjects(await listProjects());
    } catch (e) {
      setError(`Failed to load projects: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function handleOpen(path: string) {
    setPhase("restarting");
    setError(null);
    try {
      // Persist the current window bounds before the hard restart — the
      // process exit never runs beforeunload, so the 400 ms save debounce
      // would otherwise lose a just-made move/resize.
      await flushWindowGeometry();
      await switchProject(path);
      // switchProject restarts the process; this line is unreachable on
      // success. If it returns, surface the error.
    } catch (e) {
      setError(`Failed to switch project: ${e}`);
      setPhase(null);
    }
  }

  async function handleRemove(name: string) {
    setPhase("removing");
    setError(null);
    try {
      await removeProject(name);
      await refresh();
    } catch (e) {
      setError(`Failed to remove project: ${e}`);
    } finally {
      setPhase(null);
    }
  }

  async function handlePickDir() {
    setError(null);
    try {
      const picked = await pickDirectory();
      if (picked) {
        setChosenPath(picked);
        // Default the name to the directory's basename.
        const base = picked.replace(/[/\\]+$/, "").split(/[/\\]/).pop();
        setName(base ?? "");
      }
    } catch (e) {
      setError(`Failed to pick directory: ${e}`);
    }
  }

  async function handleCreate() {
    if (!chosenPath) {
      setError("Choose a directory first.");
      return;
    }
    setPhase("preparing");
    setError(null);
    try {
      // createProject scaffolds .coding/ + agent.md + registers it.
      await createProject(chosenPath, name || undefined);
      // Then restart into the new project.
      setPhase("restarting");
      // Flush the window bounds before the hard restart (see handleOpen).
      await flushWindowGeometry();
      await switchProject(chosenPath);
    } catch (e) {
      setError(`Failed to create project: ${e}`);
      setPhase(null);
    }
  }

  const body = (
    <div className="flex flex-col gap-5">
      {/* Registered projects list */}
      <section>
        <h2 className="mb-2 text-sm font-semibold text-[color:var(--text-primary)]">
          Your projects
        </h2>
        {loading ? (
          <p className="text-xs text-[color:var(--text-muted)]">Loading…</p>
        ) : projects.length === 0 ? (
          <p className="text-xs text-[color:var(--text-muted)]">
            No projects registered yet. Create one below.
          </p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {projects.map((p) => (
              <li
                key={p.path}
                className="flex items-center gap-2 rounded-lg border border-border bg-bg-primary px-3 py-2"
              >
                <FolderOpen className="h-4 w-4 shrink-0 text-cyan-400" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium text-[color:var(--text-primary)]">
                    {p.name}
                  </div>
                  <div className="truncate text-[0.7rem] text-[color:var(--text-muted)]">
                    {p.path}
                  </div>
                </div>
                <button
                  type="button"
                  onClick={() => void handleOpen(p.path)}
                  disabled={busy}
                  className="shrink-0 rounded-lg bg-[color:var(--accent-color)] px-3 py-1 text-xs font-medium text-white hover:opacity-90 disabled:opacity-40"
                >
                  {openButtonLabel(phase)}
                </button>
                <button
                  type="button"
                  onClick={() => void handleRemove(p.name)}
                  disabled={busy}
                  title="Remove from registry (does not delete files)"
                  className="shrink-0 rounded-lg p-1.5 text-[color:var(--text-muted)] hover:bg-bg-tertiary hover:text-red-400 disabled:opacity-40"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <div className="h-px w-full bg-border" />

      {/* Create new project */}
      <section>
        <h2 className="mb-2 text-sm font-semibold text-[color:var(--text-primary)]">
          Create a new project
        </h2>
        <p className="mb-3 text-[0.7rem] text-[color:var(--text-muted)]">
          Pick a directory — it will be scaffolded with a{" "}
          <code className="rounded bg-bg-tertiary px-1">.coding/</code> subfolder
          and an <code className="rounded bg-bg-tertiary px-1">agent.md</code>{" "}
          constitution, then registered.
        </p>
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => void handlePickDir()}
              disabled={busy}
              className="flex shrink-0 items-center gap-1.5 rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-xs text-[color:var(--text-primary)] hover:bg-bg-tertiary disabled:opacity-40"
            >
              <FolderPlus className="h-3.5 w-3.5" />
              Choose directory…
            </button>
            <span className="min-w-0 flex-1 truncate text-xs text-[color:var(--text-muted)]">
              {chosenPath ?? "No directory selected"}
            </span>
          </div>
          <label className="flex items-center gap-2">
            <span className="shrink-0 text-xs text-[color:var(--text-muted)]">
              Name
            </span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="defaults to the folder name"
              className="min-w-0 flex-1 rounded-lg border border-border bg-bg-primary px-2 py-1.5 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
            />
          </label>
          <button
            type="button"
            onClick={() => void handleCreate()}
            disabled={busy || !chosenPath}
            className="mt-1 flex items-center justify-center gap-1.5 rounded-lg bg-[color:var(--accent-color)] px-4 py-2 text-sm font-medium text-white hover:opacity-90 disabled:opacity-40"
          >
            {createButtonLabel(phase)}
          </button>
        </div>
      </section>

      {error && (
        <div className="flex items-start gap-2 rounded-lg border border-red-600/40 bg-red-950/20 p-3 text-xs text-red-300">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}
    </div>
  );

  // Startup mode: full-screen, no close button (mirrors the startup-error
  // screen styling). The only exit is picking/creating a project.
  if (startup) {
    return (
      <div className="flex h-screen w-screen flex-col items-center justify-center bg-bg-primary p-8">
        {/* Covers the create-project first index (streams while the picker
            is the whole UI); self-hides when the pass completes. */}
        <IndexingOverlay />
        <div className="w-full max-w-lg">
          <h1 className="mb-1 text-center text-lg font-semibold text-[color:var(--text-primary)]">
            Choose a project
          </h1>
          <p className="mb-6 text-center text-sm text-[color:var(--text-muted)]">
            Select a registered project or create a new one to get started.
          </p>
          {body}
        </div>
      </div>
    );
  }

  // Switch mode: centered Dialog with a close button. The App-tree
  // IndexingOverlay already covers the create flow here (a switch-mode
  // picker only renders with a project open, so that instance is mounted) —
  // mounting a second copy inside this Dialog would stack the failed card
  // and need two Dismiss clicks (review LOW 1).
  return (
    <Dialog open={true} onOpenChange={(o) => { if (!o) onClose?.(); }}>
      <DialogContent className="mx-4 flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-2xl">
        <div className="flex shrink-0 items-center justify-between border-b border-border px-4 py-3">
          <DialogTitle className="text-sm font-semibold text-[color:var(--text-primary)]">
            Switch project
          </DialogTitle>
          <DialogDescription className="sr-only">
            Choose an existing project to open, or create a new one by picking a directory.
          </DialogDescription>
          <button
            type="button"
            onClick={() => onClose?.()}
            aria-label="Close"
            className="rounded p-1 text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">{body}</div>
      </DialogContent>
    </Dialog>
  );
}

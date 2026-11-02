// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Same-project instance conflict dialog — shown at startup when another
 * mnemo instance launched on this project (backend: the startup snapshot's
 * `instance_conflict` field, resolved from `<project>/.coding/instance.json`
 * before this instance's marker overwrote the incumbent's). Two instances on
 * one project are supported (per-instance WebView2 profiles), so the dialog
 * ASKS instead of blocking: switch to another project, or open it anyway.
 * The conflict is a launch-time snapshot — the copy never asserts CURRENT
 * occupancy, and App.tsx re-checks the incumbent's liveness and retracts
 * this dialog once it exits (backlog ea9576d7).
 */
import { AlertTriangle, FolderOpen } from "lucide-react";
import { type InstanceConflict } from "../../lib/tauri";
import { SplashCard } from "../common/SplashCard";

export function InstanceConflictDialog({
  conflict,
  onDismiss,
  onChooseProject,
}: {
  conflict: InstanceConflict;
  onDismiss: () => void;
  onChooseProject: () => void;
}) {
  // HH:MM — a launch-time stamp, never a live "last seen": the copy never
  // asserts current occupancy (SPEC
  // 2027-01-11-same-project-instance-warning-is-a-launch-time-s.md).
  const launched = new Date(conflict.started_at * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-xs">
      <SplashCard>
        <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-amber-400">
          <AlertTriangle className="h-4 w-4" />
          Another mnemo instance launched on this project
        </div>
        <p className="max-w-md text-xs text-slate-300">
          Another mnemo instance (PID {conflict.pid}) launched on this
          project at {launched}. If it is still running, two instances on one
          project can edit the same files and step on each other — choose
          another project or open it here anyway.
        </p>
        <div className="mt-4 flex gap-2">
          <button
            onClick={onChooseProject}
            className="flex items-center gap-1.5 rounded-lg bg-cyan-600 px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-cyan-500"
          >
            <FolderOpen className="h-4 w-4" />
            Choose another project
          </button>
          <button
            onClick={onDismiss}
            className="rounded-lg bg-slate-700 px-4 py-1.5 text-sm font-medium text-slate-200 transition-colors hover:bg-slate-600"
          >
            Open it anyway
          </button>
        </div>
      </SplashCard>
    </div>
  );
}

// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Same-project instance conflict dialog — shown at startup when another LIVE
 * mnemo instance already holds this project (backend: the startup snapshot's
 * `instance_conflict` field, resolved from `<project>/.coding/instance.json`
 * before this instance's marker overwrote the incumbent's). Two instances on
 * one project are supported (per-instance WebView2 profiles), so the dialog
 * ASKS instead of blocking: switch to another project, or open it anyway.
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
  const launched = new Date(conflict.started_at * 1000).toLocaleTimeString();
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <SplashCard>
        <div className="mb-2 flex items-center gap-2 text-sm font-semibold text-amber-400">
          <AlertTriangle className="h-4 w-4" />
          This project is already open
        </div>
        <p className="max-w-md text-xs text-slate-300">
          Another mnemo instance (PID {conflict.pid}, launched {launched}) is
          already working on this project. Two instances on one project can
          edit the same files and step on each other — choose another project
          or open it here anyway.
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

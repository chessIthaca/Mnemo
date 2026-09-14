// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { GitMerge, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "../ui/dialog";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";

interface MergeToMainDialogProps {
  open: boolean;
  /** The branch that will be merged into main (the current branch). */
  sourceBranch: string;
  /** Busy state while the merge runs. */
  merging: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Confirmation dialog for the "Merge to main" action. This dialog IS the
 * approval gate for the UI-initiated merge — confirming enters the
 * `merge_to_main` skill, where the agent drives the merge itself (commit,
 * checkout main, sync with origin, merge, resolve conflicts, delete branch). Built on Radix Dialog
 * (role="dialog", aria-modal, focus trap, Escape-to-close).
 */
export function MergeToMainDialog({
  open,
  sourceBranch,
  merging,
  onConfirm,
  onCancel,
}: MergeToMainDialogProps) {
  // Hide the native child WebView2 while this full-viewport modal is open
  // (it's a separate HWND composited above the app's HTML — see useBrowserOverlay).
  useBrowserOverlay(open);
  return (
    <Dialog open={open} onOpenChange={(o: boolean) => { if (!o) onCancel(); }}>
      <DialogContent
        className="mx-4 w-full max-w-md rounded-lg border border-border bg-bg-secondary shadow-2xl"
        onEscapeKeyDown={onCancel}
        onPointerDownOutside={onCancel}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <div className="flex items-center gap-2">
            <GitMerge className="h-5 w-5 text-cyan-400" />
            <DialogTitle className="text-sm font-semibold text-slate-200">
              Merge into main?
            </DialogTitle>
          </div>
          <button
            onClick={onCancel}
            aria-label="Close dialog"
            className="text-slate-500 hover:text-slate-300"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
        {/* Body */}
        <DialogDescription asChild>
          <div className="px-4 py-4 text-sm text-slate-300">
            <p className="mb-3">
              This starts the <span className="font-medium text-cyan-400">merge_to_main</span>{" "}
              skill. The agent will drive the merge of{" "}
              <span className="font-medium text-purple-400">{sourceBranch}</span> into{" "}
              <span className="font-medium text-cyan-400">main</span> itself:
            </p>
            <ul className="mb-3 space-y-1 pl-4 text-slate-400">
              <li>• Commit the branch's work, then merge it into <code className="inline-code">main</code> (the skill never stashes)</li>
              <li>• Sync <code className="inline-code">main</code> with origin first, and resolve any conflicts</li>
              <li>• Verify both builds, delete the merged branch, return to Planning</li>
            </ul>
            <p className="text-slate-400">
              Core git operations (merge, push) always require approval, even in
              Autonomous mode. You can interrupt or steer the agent mid-skill to
              help resolve conflicts.
            </p>
          </div>
        </DialogDescription>
        {/* Actions */}
        <div className="flex justify-end gap-2 border-t border-border px-4 py-3">
          <button
            onClick={onCancel}
            disabled={merging}
            className="rounded-lg border border-border px-4 py-1.5 text-sm text-slate-300 hover:bg-bg-tertiary disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            disabled={merging}
            className="rounded-lg bg-cyan-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-cyan-500 disabled:opacity-50"
          >
            {merging ? "Starting…" : "Start merge skill"}
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

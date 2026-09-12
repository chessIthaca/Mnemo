// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { AlertTriangle, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "../ui/dialog";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";

interface SafetyToggleDialogProps {
  open: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Warning dialog shown when the user tries to enter Autonomous (auto-approve)
 * mode. Confirms they understand all mutations will run without asking.
 *
 * Built on Radix Dialog: provides role="dialog", aria-modal, a focus trap,
 * Escape-to-close, and focus restoration on close for free.
 */
export function SafetyToggleDialog({ open, onConfirm, onCancel }: SafetyToggleDialogProps) {
  // Hide the native child WebView2 while this full-viewport modal is open
  // (it's a separate HWND composited above the app's HTML — see useBrowserOverlay).
  useBrowserOverlay(open);
  return (
    <Dialog open={open} onOpenChange={(o: boolean) => { if (!o) onCancel(); }}>
      <DialogContent
        className="mx-4 w-full max-w-md rounded-lg border border-red-600/50 bg-bg-secondary shadow-2xl"
        // Keep cancel semantics on Escape / outside pointer interactions.
        onEscapeKeyDown={onCancel}
        onPointerDownOutside={onCancel}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <div className="flex items-center gap-2">
            <AlertTriangle className="h-5 w-5 text-red-400" />
            <DialogTitle className="text-sm font-semibold text-red-400">
              Enable auto-approve (dangerous)?
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
              This switches the agent to{" "}
              <span className="font-medium text-red-400">autonomous</span> mode. All
              mutating actions will run <span className="font-medium">without asking</span>:
            </p>
            <ul className="mb-3 space-y-1 pl-4 text-slate-400">
              <li>• File edits and writes</li>
              <li>• Shell commands</li>
              <li>• Git operations</li>
            </ul>
            <p className="text-slate-400">
              Use this only for trusted, reviewed work. You can switch back any time.
            </p>
          </div>
        </DialogDescription>
        {/* Actions */}
        <div className="flex justify-end gap-2 border-t border-border px-4 py-3">
          <button
            onClick={onCancel}
            className="rounded-lg border border-border px-4 py-1.5 text-sm text-slate-300 hover:bg-bg-tertiary"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            className="rounded-lg bg-red-600 px-4 py-1.5 text-sm font-medium text-white hover:bg-red-500"
          >
            Enable auto-approve
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

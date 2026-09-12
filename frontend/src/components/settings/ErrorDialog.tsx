// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "../ui/dialog";

export interface ErrorDialogProps {
  /** Whether the dialog is open. */
  open: boolean;
  /** Close callback (OK button, Escape, or overlay click). */
  onClose: () => void;
  /** Dialog title (default "Save failed"). */
  title?: string;
  /** The FULL error text to show — wrapped + scrollable, never clipped. */
  message: string;
}

/**
 * Settings error dialog — shows the complete error text in a scrollable
 * block. The inline strips in the settings sections clip long multi-line
 * provider errors (validation chains, HTTP bodies); this dialog is where the
 * full text is readable. Reuses the Radix-backed ui/dialog primitives
 * (focus trap, Escape-to-close, aria roles).
 */
export function ErrorDialog({
  open,
  onClose,
  title = "Save failed",
  message,
}: ErrorDialogProps) {
  return (
    <Dialog open={open} onOpenChange={(o) => (!o ? onClose() : undefined)}>
      <DialogContent className="w-[min(34rem,calc(100vw-2rem))] rounded-xl border border-border bg-bg-secondary p-4 shadow-2xl">
        <DialogTitle className="text-sm font-semibold text-[color:var(--text-primary)]">
          {title}
        </DialogTitle>
        <DialogDescription className="mt-3 max-h-72 overflow-y-auto whitespace-pre-wrap break-words rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs leading-relaxed text-red-200">
          {message}
        </DialogDescription>
        <div className="mt-4 flex justify-end">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-border bg-bg-primary px-3 py-1.5 text-xs text-[color:var(--text-primary)] transition-colors hover:border-[color:var(--accent-color)]/60"
          >
            OK
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { useEffect, useState } from "react";
import { X, ExternalLink } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { MnemoLogo } from "../common/MnemoLogo";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
} from "../ui/dialog";
import { useBrowserOverlay } from "../../hooks/useBrowserOverlay";
import { openExternal } from "../../lib/openExternal";
import {
  DEPENDENCY_GROUPS,
  APP_LICENSE_URL,
  type DependencyEntry,
  type DependencyGroup,
} from "./dependencies";

interface AboutDialogProps {
  open: boolean;
  onClose: () => void;
}


/**
 * About dialog â€” credits Mnemo's direct dependencies and lists each one's
 * license with a link to show it. Opened by clicking the app logo in the
 * Sidebar. Built on Radix Dialog (focus trap, Escape-to-close, focus restore).
 *
 * The dependency list is static data sourced from `./dependencies.ts` (the
 * direct deps of the three manifests: the library crate, the Tauri shell, and
 * the frontend). The app version is fetched live from the Tauri app API.
 */
export function AboutDialog({ open, onClose }: AboutDialogProps) {
  // The app version (from tauri.conf.json). Fetched when the dialog opens;
  // falls back to the manifest version on any error.
  const [version, setVersion] = useState<string>("0.1.1");

  // Hide the native child WebView2 while this full-viewport modal is open
  // (it's a separate HWND composited above the app's HTML — see useBrowserOverlay).
  useBrowserOverlay(open);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    (async () => {
      try {
        const v = await getVersion();
        if (!cancelled) setVersion(v);
      } catch (e) {
        console.error("failed to get app version:", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open]);

  return (
    <Dialog open={open} onOpenChange={(o: boolean) => { if (!o) onClose(); }}>
      <DialogContent
        className="mx-4 flex max-h-[90vh] w-full max-w-lg flex-col overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-2xl"
      >
        {/* Header */}
        <div className="flex shrink-0 items-center justify-between gap-3 border-b border-border px-4 py-3">
          <div className="flex min-w-0 items-center gap-2">
            <MnemoLogo className="h-6 w-6 shrink-0" />
            <DialogTitle className="text-sm font-semibold text-[color:var(--text-primary)]">
              About Mnemo
            </DialogTitle>
            <span className="shrink-0 rounded-full bg-bg-tertiary px-2 py-0.5 text-[0.65rem] font-medium text-[color:var(--text-muted)]">
              v{version}
            </span>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close about dialog"
            className="shrink-0 rounded p-1 text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Body */}
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden px-4 py-3">
          <DialogDescription asChild>
            <p className="mb-3 text-sm text-slate-300">
              Mnemo is licensed under the{" "}
              <button
                type="button"
                onClick={() => void openExternal(APP_LICENSE_URL)}
                className="text-cyan-400 underline-offset-2 hover:underline"
              >
                MIT License
              </button>
              . Built on these open-source
              projects:
            </p>
          </DialogDescription>

          <div className="min-h-0 flex-1 overflow-y-auto pr-1">
            {DEPENDENCY_GROUPS.map((group: DependencyGroup) => (
              <div key={group.title} className="mb-4 last:mb-0">
                <h3 className="mb-1.5 text-[0.7rem] font-semibold uppercase tracking-wide text-slate-500">
                  {group.title}
                </h3>
                <ul className="space-y-1">
                  {group.entries.map((dep: DependencyEntry) => (
                    <li
                      key={`${group.title}-${dep.name}`}
                      className="flex items-center gap-2 text-xs"
                    >
                      <button
                        type="button"
                        onClick={() => void openExternal(dep.registry)}
                        className="min-w-0 truncate text-cyan-400 underline-offset-2 hover:underline"
                        title={`Open ${dep.name} on its registry`}
                      >
                        {dep.name}
                      </button>
                      <button
                        type="button"
                        onClick={() => void openExternal(dep.licenseUrl)}
                        className="shrink-0 rounded bg-bg-tertiary px-1.5 py-0.5 text-[0.65rem] text-slate-400 transition-colors hover:text-cyan-400"
                        title={`Show the ${dep.license} license text`}
                      >
                        {dep.license}
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>

          <p className="mt-3 flex shrink-0 items-center gap-1 text-[0.7rem] text-[color:var(--text-muted)]">
            <ExternalLink className="h-3 w-3 shrink-0" />
            Click a name or license to open its page in your browser.
          </p>
        </div>

        {/* Footer */}
        <div className="flex shrink-0 justify-end border-t border-border px-4 py-3">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-border px-4 py-1.5 text-sm text-[color:var(--text-muted)] hover:text-[color:var(--text-primary)]"
          >
            Close
          </button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

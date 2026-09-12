// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { RefreshCw } from "lucide-react";

/**
 * Live model-picker dropdown — models from the endpoint's OpenAI-compatible
 * `/models` list, filtered by query. Used in focus mode (replace row) and
 * add mode (append deduped model). Parent owns fetch state + cache + query.
 *
 * Stale-while-revalidate: during a refetch the previous list stays visible
 * (only the refresh icon spins to signal activity); the "Fetching models…"
 * placeholder appears only when there is no prior list to show.
 */
export function ModelPickerDropdown({
  autoFocusFilter,
  status,
  error,
  models,
  query,
  onQueryChange,
  onRefresh,
  onPick,
  visionCapableIds,
}: {
  /** True only in add mode (no row input to type in). */
  autoFocusFilter?: boolean;
  status: "idle" | "loading" | "ready" | "error";
  error: string;
  models: string[];
  query: string;
  onQueryChange: (q: string) => void;
  onRefresh: () => void;
  onPick: (id: string) => void;
  /**
   * Optional set of model ids the provider reports as vision-capable. When
   * present, those rows render a "vision" badge. Models are never hidden based
   * on this — it is purely an annotation so the user can still pick any model.
   */
  visionCapableIds?: Set<string>;
}) {
  return (
    <div
      className="absolute left-0 top-full z-50 mt-1 max-h-60 w-full overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-2xl"
      role="listbox"
      onMouseDown={(e) => {
        if (e.target instanceof HTMLElement && e.target.closest("button, [role='option']")) {
          e.preventDefault();
        }
      }}
      onKeyDown={(e) => {
        if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Enter") return;
        const options = Array.from(
          e.currentTarget.querySelectorAll<HTMLButtonElement>("[role='option']"),
        );
        if (options.length === 0) return;
        if (e.key === "Enter") {
          const active = document.activeElement as HTMLButtonElement | null;
          if (active && options.includes(active)) {
            e.preventDefault();
            active.click();
          }
          return;
        }
        e.preventDefault();
        const current = document.activeElement as HTMLElement | null;
        const idx = current ? options.indexOf(current as HTMLButtonElement) : -1;
        const next =
          e.key === "ArrowDown"
            ? (idx + 1) % options.length
            : (idx - 1 + options.length) % options.length;
        options[next]?.focus();
      }}
    >
      <div className="flex items-center gap-1 border-b border-border px-2 py-1">
        <input
          autoFocus={autoFocusFilter}
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          placeholder="filter models…"
          spellCheck={false}
          className="flex-1 rounded border border-border bg-bg-primary px-2 py-1 text-xs text-[color:var(--text-primary)] focus:border-[color:var(--accent-color)] focus:outline-none"
        />
        <button
          type="button"
          onClick={onRefresh}
          title="Re-fetch the model list from the server"
          aria-label="Refresh model list"
          className="shrink-0 rounded p-1 text-[color:var(--text-muted)] transition-colors hover:text-[color:var(--text-primary)]"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${status === "loading" ? "animate-spin" : ""}`} />
        </button>
      </div>

      <div className="max-h-44 overflow-y-auto">
        {status === "loading" && models.length === 0 && (
          <div className="px-3 py-2 text-xs text-[color:var(--text-muted)]">Fetching models…</div>
        )}
        {status === "error" && (
          <div className="px-3 py-2 text-xs text-red-300">{error || "Failed to fetch models."}</div>
        )}
        {status === "ready" && models.length === 0 && (
          <div className="px-3 py-2 text-xs text-[color:var(--text-muted)]">
            {query.trim() ? "No models match the filter." : "No models available."}
          </div>
        )}
        {models.length > 0 && status !== "error" &&
          models.map((m) => (
            <button
              key={m}
              type="button"
              role="option"
              aria-selected="false"
              onClick={() => onPick(m)}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-[color:var(--text-primary)] transition-colors hover:bg-bg-tertiary focus:bg-bg-tertiary focus:outline-none"
            >
              <span className="min-w-0 truncate">{m}</span>
              {visionCapableIds?.has(m) && (
                <span className="ml-auto shrink-0 rounded bg-[color:var(--accent-color)]/15 px-1.5 py-0.5 text-[0.6rem] font-medium text-[color:var(--accent-color)]">
                  vision
                </span>
              )}
            </button>
          ))}
      </div>
    </div>
  );
}

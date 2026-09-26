// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Pure helpers for the Savings section's model combobox (backlog 82dd66fc,
 * user-reported): the model field of each [[pricing]] row offers a dropdown
 * of the models available from the configured endpoints in addition to free
 * typing. Pricing is keyed by exact model name (Config::pricing_for,
 * src/config/endpoints.rs), so picking from the list prevents the
 * silently-never-applies typo; free text stays available for models the
 * configured lists don't carry yet. Node-safe (no React) so the vitest
 * suite can test the data layer directly.
 */

import type { EndpointInfo } from "../../../lib/tauri";

/** One dropdown entry: a distinct model id plus every endpoint serving it. */
export interface ModelOption {
  model: string;
  endpoints: string[];
}

/**
 * Build the dropdown's options from the configured endpoints: every model
 * id listed by any endpoint, grouped — a model served by several endpoints
 * appears once, annotated with all of them — and sorted by model id.
 * Blank model strings are skipped (a malformed endpoints.toml list entry
 * must not produce an empty dropdown row).
 */
export function buildModelOptions(endpoints: EndpointInfo[]): ModelOption[] {
  const byModel = new Map<string, string[]>();
  for (const ep of endpoints) {
    for (const model of ep.models) {
      const trimmed = model.trim();
      if (!trimmed) continue;
      const list = byModel.get(trimmed);
      if (list) {
        if (!list.includes(ep.name)) list.push(ep.name);
      } else {
        byModel.set(trimmed, [ep.name]);
      }
    }
  }
  return [...byModel.entries()]
    .map(([model, eps]) => ({ model, endpoints: eps }))
    .sort((a, b) => a.model.localeCompare(b.model));
}

/**
 * The combobox's filter rule: an empty value shows everything; a value that
 * exactly matches an option shows everything (reopening the dropdown after
 * a selection lets you pick an alternative instead of a one-entry list);
 * anything else filters by case-insensitive substring. Free text that
 * matches nothing yields an empty list — the input still accepts it (no
 * forced validation; save behavior is unchanged).
 */
export function filterModelOptions(
  options: ModelOption[],
  value: string,
): ModelOption[] {
  const v = value.trim();
  if (!v) return options;
  if (options.some((o) => o.model === v)) return options;
  const lower = v.toLowerCase();
  return options.filter((o) => o.model.toLowerCase().includes(lower));
}

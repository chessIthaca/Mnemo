// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import type { EndpointInfo } from "./tauri";

/**
 * Resolve the endpoint that serves a model id — the active agent's endpoint
 * when models are agent-specific (the toolbar picker switches only the active
 * agent, so its model may live on a different endpoint than the configured
 * default).
 *
 * The picker's per-model list is the source of truth: the first endpoint
 * whose `models` array contains `modelId` wins. Returns `null` when no
 * endpoint lists the id (e.g. a default model that was edited out of the
 * endpoint list, or an empty model id before the first settings load) — the
 * caller falls back to the global default provider.
 *
 * This is the FALLBACK resolution only: the backend now carries the serving
 * endpoint per agent on the wire (AgentInfo.provider / ModelChanged.provider
 * — backlog 2980ca67), and the status bar prefers that wire value, so a
 * model id listed under two endpoints is labeled by the endpoint that
 * actually serves it. This scan still serves mock-backed agents (empty
 * provider name on the wire) and any surface without the wire field.
 */
export function endpointForModel(
  endpoints: EndpointInfo[],
  modelId: string,
): string | null {
  if (!modelId) return null;
  for (const ep of endpoints) {
    if (ep.models.includes(modelId)) return ep.name;
  }
  return null;
}

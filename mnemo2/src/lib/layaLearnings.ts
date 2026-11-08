// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * The window's side of Laya's learnings offers.
 *
 * A shipped copy of Laya's routing decisions that this computer disagrees with
 * waits in the native host's offer list. The host emits the FULL list on
 * `LAYA_LEARNINGS_CHANNEL` whenever it changed, and both commands answer with
 * that same full list — `laya_learnings_decide` never emits, so its return value
 * is the only update after a decision. An empty list is a valid payload: it
 * closes the question.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/** The channel carrying the full list of pending learnings offers. */
export const LAYA_LEARNINGS_CHANNEL = "mnemo2://laya-learnings";

/** One shipped copy the user has to answer for. */
export interface LearningsOffer {
  /** Which bundle the offer is about — the project bundle or the installer's. */
  source_key: string;
  /** Where the copy came from, spelled for the question. */
  source_label: string;
  /** The bundle version: the sha256 of its bytes. */
  sha256: string;
  /** Rows the copy carries that this computer's log lacks. */
  new_rows: number;
  /** Rows this computer has that the copy lacks. */
  local_only_rows: number;
}

export interface LayaLearningsBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

/**
 * The sheet's I/O. Both commands return the store's FULL remaining offer list
 * in the host's order, so the sheet replaces its state instead of patching a
 * local copy.
 */
export interface LayaLearningsTransport {
  offers(): Promise<LearningsOffer[]>;
  decide(offer: LearningsOffer, accept: boolean): Promise<LearningsOffer[]>;
  onOffers(handler: (offers: LearningsOffer[]) => void): Promise<UnlistenFn>;
}

export function createTauriLayaLearningsTransport(
  bindings: LayaLearningsBindings,
): LayaLearningsTransport {
  return {
    offers: () => bindings.invoke<LearningsOffer[]>("laya_learnings_offers"),
    decide: (offer, accept) =>
      bindings.invoke<LearningsOffer[]>("laya_learnings_decide", {
        sourceKey: offer.source_key,
        sha256: offer.sha256,
        accept,
      }),
    onOffers: (handler) =>
      bindings.listen<LearningsOffer[]>(LAYA_LEARNINGS_CHANNEL, (event) => handler(event.payload)),
  };
}

/** The native learnings transport, or `null` in a plain browser. */
export function createLayaLearningsTransport(
  bindings?: LayaLearningsBindings,
): LayaLearningsTransport | null {
  if (bindings) return createTauriLayaLearningsTransport(bindings);
  if (isTauriRuntime()) return createTauriLayaLearningsTransport({ invoke, listen });
  return null;
}

/** The sentence the sheet shows. */
export function offerMessage(offer: LearningsOffer): string {
  const rows = offer.new_rows === 1 ? "1 routing decision" : `${offer.new_rows} routing decisions`;
  return (
    `${offer.source_label} has ${rows} this computer doesn't have, and this computer has ` +
    `${offer.local_only_rows} of its own that the copy doesn't. ` +
    "Updating adds the new ones and removes nothing."
  );
}

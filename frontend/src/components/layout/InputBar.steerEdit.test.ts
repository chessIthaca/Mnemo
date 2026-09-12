// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Steer-bubble affordances contract (user request 2026-12-30: click-to-edit
 * a pending steer).
 *
 * The InputBar's steer backlog renders one bubble per steer (pending =
 * waiting to be injected; landed = injected, fades out). Two affordances on
 * PENDING steers only:
 *   1. click-to-edit (draft mode) — the text is a button: clicking loads
 *      the steer text into the main input (the edit surface) and cancels
 *      the pending steer FULL-STACK immediately (the exact X flow);
 *      sending re-queues the edited text as a fresh steer via the normal
 *      steer branch. Cancel-at-click means nothing can land mid-edit and
 *      no edit state survives injection.
 *   2. the X-to-delete (shipped earlier, plan 7da676df / commit 5d9e253)
 *      still renders and wires the full-stack cancel.
 * LANDED steers are NOT editable — the steer already took effect, and the
 * bubble auto-removes after fading out.
 *
 * Pinned as source contracts (the runtime wiring is store-driven, so static
 * source is the stable surface — same style as App.shellRender.test.ts).
 * Sources are imported via Vite's `?raw` (typed by `vite/client`, so this
 * compiles under `tsc` too).
 */
import { describe, expect, it } from "vitest";
import inputBarSource from "./InputBar.tsx?raw";

describe("steer bubble affordances (click-to-edit + X-to-delete)", () => {
  it("a pending steer's text is a click-to-edit button (draft mode)", () => {
    // The edit affordance: the title announces it, the text carries a
    // Pencil hover hint, and the handler loads the steer text into the
    // main input (the edit surface).
    expect(inputBarSource).toContain('title="Edit this steer');
    expect(inputBarSource).toContain("<Pencil className=");
    // The Pencil sits OUTSIDE the truncating span (review LOW 1): the
    // button is a flex row whose inner span truncates, so the hint stays
    // visible even when the steer text is clipped.
    const editButtonTail = inputBarSource.slice(
      inputBarSource.indexOf('title="Edit this steer'),
      inputBarSource.indexOf('title="Cancel this steer"'),
    );
    const truncatingSpanAt = editButtonTail.indexOf(
      '<span className="truncate">',
    );
    expect(truncatingSpanAt).toBeGreaterThan(-1);
    expect(truncatingSpanAt).toBeLessThan(editButtonTail.indexOf("<Pencil"));
  });

  it("the edit click cancels the pending steer full-stack immediately", () => {
    // Draft mode: the edit handler runs the exact X flow via the shared
    // cancelPendingSteer helper (removeSteer + cancelSuggestion) — once the
    // backend processes the cancel, nothing can land mid-edit and no edit
    // state survives injection. Structural anchors (bfbab877 guidance —
    // no prose comments): every handler line's first occurrence precedes
    // the edit button's title in the JSX.
    const editTitleAt = inputBarSource.indexOf('title="Edit this steer');
    expect(editTitleAt).toBeGreaterThan(-1);
    for (const handlerLine of [
      "setText(steer.text);",
      "cancelPendingSteer(steer);",
      "resetNavigation();",
      "textareaRef.current?.focus();",
    ]) {
      const at = inputBarSource.indexOf(handlerLine);
      expect(at, `anchor not found: ${handlerLine}`).toBeGreaterThan(-1);
      expect(at, `${handlerLine} must sit inside the edit handler`).toBeLessThan(
        editTitleAt,
      );
    }
    // The shared helper is the full-stack cancel (the X flow).
    expect(inputBarSource).toContain(
      "function cancelPendingSteer(steer: SteerEntry)",
    );
    expect(inputBarSource).toContain("removeSteer(activeAgent, steer.id);");
    expect(inputBarSource).toContain(
      "cancelSuggestion(activeAgent, steer.text)",
    );
  });

  it("the X-to-delete still renders and wires the full-stack cancel", () => {
    // The shipped half (plan 7da676df, commit 5d9e253) — the user asked
    // for it as if missing; this pins that it stays wired.
    expect(inputBarSource).toContain('title="Cancel this steer"');
    expect(inputBarSource).toContain('<X className="h-3 w-3" />');
  });

  it("landed steers are NOT editable", () => {
    // Once a steer lands (suggestion_injected) editing must no longer be
    // possible: the landed branch renders a plain span — no button, no
    // onClick, no cancel. The bubble auto-removes after fading out, so
    // both affordances disappear naturally. Structural anchor (bfbab877
    // guidance — no prose comments): the landed branch is the first arm
    // of the text ternary, located by its condition.
    const landedTernaryAt = inputBarSource.indexOf(
      'steer.status === "landed" ? (',
    );
    expect(landedTernaryAt).toBeGreaterThan(-1);
    const landedBranch = inputBarSource.slice(
      landedTernaryAt,
      inputBarSource.indexOf(") : (", landedTernaryAt),
    );
    expect(landedBranch).toContain("<InlineMarkdown text={steer.text} />");
    expect(landedBranch).not.toContain("<button");
    expect(landedBranch).not.toContain("cancelPendingSteer");
  });
});

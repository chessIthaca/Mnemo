// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * BacklogItemCard headline rendering (backlog 40763a24, user-reported
 * 2027-01-05): "The format in the backlog should render the headline as a
 * headline? Make it prettier." The first line of an item's text is the
 * headline (the backlog_add ToolCard chip convention) and must render as a
 * styled headline — status chip beside it, semibold — with the rest as a
 * collapsible body (rotating chevron, long bodies collapsed by default),
 * mirroring the plan-steps pattern (PlanStepRow). Markup pinned via
 * renderToStaticMarkup (the PlanStepRow/MnemoLogo pattern — vitest runs in
 * a node environment, no React DOM test infra). BacklogItemCard reads
 * useAgentStore, whose initial empty state is fine under SSR — no store
 * mocking needed.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { BacklogItemCard, backlogItemTemplate, moveIdToBack, moveIdToFront, planChipTarget, splitHeadline } from "./BacklogView";
import source from "./BacklogView.tsx?raw";
import type { BacklogItem } from "../../lib/types";

function item(text: string, overrides: Partial<BacklogItem> = {}): BacklogItem {
  return {
    id: "test-1",
    text,
    images: [],
    status: "pending",
    created_at: Date.now(),
    note: null,
    ...overrides,
  };
}

function renderCard(text: string, index = 0, total = 1): string {
  return renderToStaticMarkup(
    <BacklogItemCard item={item(text)} index={index} total={total} />,
  );
}

/** Extract one button's full markup by its title attribute. */
function buttonMarkup(html: string, title: string): string {
  const at = html.indexOf(`title="${title}"`);
  expect(at).toBeGreaterThanOrEqual(0);
  const start = html.lastIndexOf("<button", at);
  const end = html.indexOf("</button>", at) + "</button>".length;
  return html.slice(start, end);
}

describe("BacklogItemCard headline rendering (backlog 40763a24)", () => {
  const longBody =
    "Problem: the models tab lets the user choose a model, but there is no way to set the reasoning effort there. " +
    "Fix direction: add a control. Investigation pointers follow below with more detail than two hundred characters so the body counts as long and collapses by default.";

  it("renders the first line as a semibold headline with the status chip beside it", () => {
    const html = renderCard("Per-model reasoning-effort selector\n" + longBody);
    expect(html).toContain("Per-model reasoning-effort selector");
    expect(html).toContain("font-semibold");
    // The status chip sits in the headline row (pending style), polished
    // uppercase (backlog 40763a24).
    expect(html).toContain("bg-slate-700/50");
    expect(html).toContain("uppercase tracking-wide");
  });

  it("collapses long bodies by default (chevron, aria-expanded=false, body absent)", () => {
    const html = renderCard("Headline\n" + longBody);
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("rotate-90");
    // The body is collapsed away — its marker text is absent.
    expect(html).not.toContain("Investigation pointers");
  });

  it("shows short bodies by default", () => {
    const html = renderCard("Headline\nA short body.");
    expect(html).toContain("A short body.");
    expect(html).toContain('aria-expanded="true"');
  });

  it("renders headline-only items without a toggle", () => {
    const html = renderCard("Just a headline");
    expect(html).toContain("Just a headline");
    expect(html).not.toContain("aria-expanded");
  });

  it("renders a send-to-top button — enabled mid-queue, disabled at the top", () => {
    // User request 2027-01-06: one-click jump to the top of the queue
    // instead of N move-ups. Same index-0 gate as move-up.
    const mid = buttonMarkup(renderCard("Headline\nbody", 1, 2), "Send to top");
    expect(mid).not.toContain('disabled=""');
    const top = buttonMarkup(renderCard("Headline\nbody", 0, 2), "Send to top");
    expect(top).toContain('disabled=""');
  });
});

describe("splitHeadline (backlog 40763a24)", () => {
  it("splits on the first newline: first line = headline, rest = body", () => {
    expect(splitHeadline("Headline\nline two\nline three")).toEqual({
      headline: "Headline",
      body: "line two\nline three",
    });
  });

  it("returns an empty body for single-line items", () => {
    expect(splitHeadline("Just a headline")).toEqual({
      headline: "Just a headline",
      body: "",
    });
  });

  it("handles CRLF newlines", () => {
    expect(splitHeadline("Headline\r\nline two")).toEqual({
      headline: "Headline",
      body: "line two",
    });
  });

  it("keeps a leading-blank-line item degenerate but lossless (empty headline, body preserved)", () => {
    expect(splitHeadline("\nbody starts here")).toEqual({
      headline: "",
      body: "body starts here",
    });
  });
});

describe("moveIdToFront — send to top (user request 2027-01-06)", () => {
  it("moves a middle id to the front, preserving the rest", () => {
    expect(moveIdToFront(["a", "b", "c", "d"], "c")).toEqual(["c", "a", "b", "d"]);
  });

  it("returns an equal order when the id is already first", () => {
    expect(moveIdToFront(["a", "b", "c"], "a")).toEqual(["a", "b", "c"]);
  });

  it("returns an equal order for an absent id (stale-render no-op)", () => {
    expect(moveIdToFront(["a", "b"], "zzz")).toEqual(["a", "b"]);
  });
});

describe("moveIdToBack — send to bottom (user request 2027-01-07)", () => {
  it("moves a middle id to the back, preserving the rest", () => {
    expect(moveIdToBack(["a", "b", "c", "d"], "b")).toEqual(["a", "c", "d", "b"]);
  });

  it("returns an equal order when the id is already last", () => {
    expect(moveIdToBack(["a", "b", "c"], "c")).toEqual(["a", "b", "c"]);
  });

  it("returns an equal order for an absent id (stale-render no-op)", () => {
    expect(moveIdToBack(["a", "b"], "zzz")).toEqual(["a", "b"]);
  });

  it("mirrors moveIdToFront on a two-element order", () => {
    expect(moveIdToBack(["a", "b"], "a")).toEqual(["b", "a"]);
    expect(moveIdToFront(["a", "b"], "b")).toEqual(["b", "a"]);
  });

  it("the card wires a Send-to-bottom button and the deferred checkbox lives in the toolbar", () => {
    // Source contract (the node harness cannot fire onClick): the
    // send-to-bottom button rides after move-down, disabled at the last
    // index, and the deferred checkbox moved from beside the status chip
    // into the card toolbar (user request 2027-01-07).
    expect(source).toContain("function handleSendToBottom()");
    expect(source).toContain("moveIdToBack(");
    expect(source).toContain('title="Send to bottom"');
    expect(source).toContain("ArrowDownToLine");
    expect(source).toContain("disabled={index === total - 1}");
    // The checkbox's old home (beside the status chip) is gone; its new
    // comment names the toolbar move.
    expect(source).toContain("lives in the card toolbar");
    expect(source).not.toContain("a checkbox beside the status chip");
  });
});

describe("BacklogItemCard plan chip (backlog f45513b2)", () => {
  const sha = "b3befe9ec4e55b5b2359bab49cbb97663e78257b";

  it("renders the plan title + short-id chip; the raw sha never headlines", () => {
    const html = renderToStaticMarkup(
      <BacklogItemCard
        item={item("Task\nbody", {
          status: "in_flight",
          plan_id: "593f4a4e-1111-2222-3333-444455556666",
          plan_title: "Fix the login flow",
          note: `${sha} | dispatched (run-all) — checkpoint before work`,
          checkpoint_sha: sha,
        })}
        index={0}
        total={1}
      />,
    );
    // Primary identifier: the plan TITLE.
    expect(html).toContain("Fix the login flow");
    // Secondary: the short plan-id chip (the tool layer's 8-char prefix).
    expect(html).toContain("593f4a4e");
    // The raw sha is NEVER visible text — not the headline, not the note.
    // (It legitimately lives in the detail's title tooltip attribute —
    // the copyable anchor — which is exactly where the task wants it.)
    expect(html).not.toContain(`>${sha}<`);
    expect(html).toContain(`pre-work checkpoint ${sha.slice(0, 8)}…`);
    // The note renders the reason, sha head stripped.
    expect(html).toContain("dispatched (run-all) — checkpoint before work");
  });

  it("renders no chip and no checkpoint detail without a plan linkage", () => {
    const html = renderCard("Task\nbody");
    expect(html).not.toContain("pre-work checkpoint");
  });

  it("falls back to the short id alone when the title is missing; a sha-only note renders no note block", () => {
    const html = renderToStaticMarkup(
      <BacklogItemCard
        item={item("Task", {
          status: "in_flight",
          plan_id: "593f4a4e-abcd",
          checkpoint_sha: sha,
          note: sha,
        })}
        index={0}
        total={1}
      />,
    );
    expect(html).toContain("593f4a4e");
    // The note block is absent (the note was exactly the sha).
    expect(html).not.toContain("whitespace-pre-wrap");
  });

  it("chip is clickable while an agent works the plan, inert otherwise", () => {
    // The decision logic. The node harness cannot fire onClick, and
    // zustand's SSR snapshot reads the INITIAL store state (setState is
    // invisible to renderToStaticMarkup), so the clickable markup case
    // cannot be rendered here — the wiring is pinned by the markup's
    // conditional styling (the inert case below), the planChipTarget
    // unit tests, and the BacklogView.test.ts source contract.
    expect(planChipTarget("in_flight", 7)).toBe(7);
    expect(planChipTarget("done", 7)).toBeNull();
    expect(planChipTarget("in_flight", null)).toBeNull();
    // Markup: not in flight (or no main agent — the SSR store is empty)
    // → inert styling + the inert title.
    const done = renderToStaticMarkup(
      <BacklogItemCard
        item={item("T", { status: "done", plan_id: "abcd1234-ef", plan_title: "P" })}
        index={0}
        total={1}
      />,
    );
    expect(done).toContain("cursor-default");
    expect(done).toContain("no agent currently working it");
    // In flight but no main agent in the (empty SSR) store → inert too.
    const inflightNoAgent = renderToStaticMarkup(
      <BacklogItemCard
        item={item("T", { status: "in_flight", plan_id: "abcd1234-ef", plan_title: "P" })}
        index={0}
        total={1}
      />,
    );
    expect(inflightNoAgent).toContain("cursor-default");
  });

  it("the checkpoint detail keeps the full sha copyable", () => {
    const html = renderToStaticMarkup(
      <BacklogItemCard
        item={item("T", {
          status: "in_flight",
          plan_id: "abcd1234",
          checkpoint_sha: sha,
          note: `${sha} | reason`,
        })}
        index={0}
        total={1}
      />,
    );
    // The detail row's span carries the FULL sha in its title (the copy
    // handler's source) — the manual resume/rollback anchor stays
    // copyable.
    const btn = buttonMarkup(html, "Copy the full checkpoint sha (resume/rollback anchor)");
    expect(btn).toContain("copy");
  });
});

/**
 * Deferred (skip Run-All) — backlog f2d2809b, user request 2027-01-07: a
 * checkbox on each card beside the status chip, persisting immediately,
 * plus a visible Deferred badge. The node harness cannot fire onChange —
 * the toggle wiring is pinned by the BacklogView.test.ts source contract.
 */
describe("BacklogItemCard deferred (skip Run-All) — backlog f2d2809b", () => {
  it("renders an unchecked checkbox and no badge when not deferred", () => {
    const html = renderCard("Task\nbody");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain('aria-label="Deferred (skip Run-All)"');
    expect(html).not.toContain("checked=");
    expect(html).not.toContain(">Deferred<");
  });

  it("treats an absent deferred field as false (old payloads)", () => {
    // The backend omits the field when unset — the card must render the
    // plain (unchecked) state, never crash on undefined.
    const html = renderCard("Task\nbody");
    expect(html).not.toContain("checked=");
    expect(html).not.toContain(">Deferred<");
  });

  it("renders a checked checkbox and the Deferred badge when deferred", () => {
    const html = renderToStaticMarkup(
      <BacklogItemCard
        item={item("Task\nbody", { deferred: true })}
        index={0}
        total={1}
      />,
    );
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("checked=");
    expect(html).toContain(">Deferred<");
    expect(html).toContain("text-violet-400");
  });
});

/**
 * The item template (2027-01-07 detail bar): the composer's Template
 * button pre-fills the 5-section skeleton so a hand-written item passes
 * the backend's detail bar (≥160-char body naming at least one file
 * path, or an explicit 'no-code research' marker).
 */
describe("backlogItemTemplate (2027-01-07 detail bar)", () => {
  it("carries the five sections, a headline first line, and a path placeholder", () => {
    const t = backlogItemTemplate();
    // The five detail-bar sections…
    expect(t).toContain("Problem (date + how to reproduce):");
    expect(t).toContain("Files/symbols (exact paths):");
    expect(t).toContain("Fix direction:");
    expect(t).toContain("Acceptance / how to verify:");
    expect(t).toContain("Related (memories, commits, reviews):");
    // …a headline first line followed by a blank line (the shape the
    // backend enforces)…
    expect(t.startsWith("Short headline\n\n")).toBe(true);
    // …and a path placeholder in the files section (the structural
    // marker the detail bar requires).
    expect(t).toContain("src/...");
  });

  it("the Template button fills only an empty composer — never clobbers typed text", () => {
    // The node harness cannot fire onClick — the wiring is pinned by the
    // source contract (the deferred-checkbox pattern): the handler guards
    // on non-empty text, and the button is disabled while text is present.
    expect(source).toContain("function handleTemplate()");
    expect(source).toContain("if (text.trim()) return;");
    expect(source).toContain("onClick={handleTemplate}");
    expect(source).toContain("disabled={!!text.trim()}");
  });
});

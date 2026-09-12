// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * BacklogView reset-done regression test (user request 2026-08-20):
 * "we need a way for me to reset tasks you mark as done." The re-queue
 * button must also appear on DONE cards — resetting sends the item back to
 * pending via backlog_retry (backend `BacklogStore::requeue` accepts all
 * terminal statuses and refuses pending/in-flight, so a stale click is a
 * safe no-op). Static source-contract test in the style of
 * InflightBar.test.ts (vitest runs in `node` env — no React DOM infra).
 */

import { describe, expect, it } from "vitest";
import source from "./BacklogView.tsx?raw";

describe("BacklogView reset-done affordance", () => {
  it("offers re-queue on done cards (not just failed/cant_resolve)", () => {
    // The gate condition must cover the done status alongside
    // failed/cant_resolve.
    expect(source).toContain('item.status === "done"');
    expect(source).toContain('item.status === "failed"');
    expect(source).toContain('item.status === "cant_resolve"');
  });

  it("labels the done-card action as a reset that keeps git history", () => {
    expect(source).toContain(
      "Reset to pending — re-run this task (does not touch git history)",
    );
  });

  it("still routes through the backlog_retry IPC", () => {
    expect(source).toContain("backlogRetry(item.id)");
  });
});

/**
 * The "finished" notion (user ask 2027-01-07): done, failed, and
 * cant_resolve are treated identically everywhere — the clear-finished
 * gating (hasFinished) must cover all three, mirroring the retry gate
 * above.
 */
describe("BacklogView clear-finished gating (user ask 2027-01-07)", () => {
  it("gates on all three terminal statuses, not just done", () => {
    expect(source).toContain(
      'b.status === "done" || b.status === "failed" || b.status === "cant_resolve"',
    );
  });
});

/**
 * Plan-chip click wiring (backlog f45513b2): the node harness cannot fire
 * onClick — the switch-to-the-working-agent wiring is pinned on source.
 */
describe("BacklogView plan chip (backlog f45513b2)", () => {
  it("the chip click switches to the working agent's chat tab", () => {
    expect(source).toContain("setActiveAgent(chipTarget)");
  });

  it("the chip renders from the structured payload fields, not the note text", () => {
    expect(source).toContain("item.plan_title");
    expect(source).toContain("item.plan_id.slice(0, 8)");
  });
});

/**
 * Backlog display line-break regression test (user request 2026-08-21):
 * multi-line prompts used to collapse newlines to spaces in the card list.
 * Originally fixed with `whitespace-pre-wrap`; since the backlog-markdown
 * plan (2026-12) the card renders through the shared Markdown pipeline with
 * `remark-breaks`, which turns a single `\n` into `<br>` — no literal
 * newline survives into the rendered output, so the markdown display divs
 * don't need pre-wrap (it would be a no-op there, not a double-break). The
 * lazy-load fallback keeps newlines visible via its own whitespace-pre-wrap
 * classes (Markdown.tsx). The failure NOTE stays plain text (pre-wrap).
 */
describe("BacklogView line-break display", () => {
  it("renders the card body through Markdown with remark-breaks (\\n → <br>)", () => {
    // Since the headline split (backlog 40763a24) the first line renders as
    // a plain semibold headline; the BODY keeps the markdown pipeline.
    expect(source).toContain(
      "<Markdown remarkPlugins={[remarkBreaks]}>{body}</Markdown>",
    );
  });

  it("has no hover preview — expanding details superseded it (user request 2027-01-07)", () => {
    // The hover popup was removed: long text is reachable via the expand
    // affordance (pinned below) and images via the always-rendered
    // thumbnail strip — no portal, no hover machinery left.
    expect(source).not.toContain("previewOpen");
    expect(source).not.toContain("showPreviewDelayed");
    expect(source).not.toContain("hidePreview");
    expect(source).not.toContain("createPortal");
  });

  it("keeps image thumbnails visible on the card (no hover needed)", () => {
    // The always-rendered small-thumbnail strip — images stay reachable
    // after the hover preview's removal (the popup's larger h-16 view is
    // the accepted loss; the user judged the expanding details
    // sufficient).
    expect(source).toContain("{!editing && item.images.length > 0 && (");
  });

  it("renders newlines in the failure note (plain text, pre-wrap)", () => {
    expect(source).toContain(
      "mt-2 whitespace-pre-wrap break-words rounded border border-border",
    );
  });

  it("keeps an expand/collapse affordance on long items", () => {
    // The headline split (backlog 40763a24) replaced the 200-char truncation
    // with a headline + collapsible body: long bodies collapse by default,
    // the chevron toggle expands them.
    expect(source).toContain("const isLongBody = body.length > 200;");
    expect(source).toContain("useState(!isLongBody)");
    expect(source).toContain("setExpanded((v) => !v)");
    expect(source).toContain("aria-expanded={expanded}");
  });
});

/**
 * Backlog markdown rendering (plan 2026-12): item text renders through the
 * same markdown stack as the plan display (react-markdown + remark-gfm +
 * rehype-highlight via Markdown/MarkdownImpl) on every read-only surface,
 * while the inline editor stays a raw textarea (no live markdown preview).
 * The rendered-output contract (**bold** → <strong>, single \n → <br>) is
 * proven in markdownRendering.test.ts; this pins the wiring in BacklogView.
 */
describe("BacklogView markdown rendering", () => {
  it("uses the shared Markdown component + remark-breaks", () => {
    expect(source).toContain('import { Markdown } from "../chat/Markdown";');
    expect(source).toContain('import remarkBreaks from "remark-breaks";');
  });

  it("styles the card text with the plan display's prose classes", () => {
    // Same prose wrapper as PlanProgress so lists/headings/code render
    // consistently with the plan display.
    expect(source).toContain("prose prose-invert prose-sm");
  });

  it("keeps the inline editor a raw textarea (no markdown preview)", () => {
    expect(source).toContain("value={editText}");
    expect(source).toContain(
      'placeholder="Edit the prompt… (paste or drop images to attach)"',
    );
  });
});

/**
 * Backlog image-editing regression test (user request 2026-09-11, backlog
 * item cb04ddea): "For items in the backlog I can't edit the images or add
 * new screenshots." The per-card inline editor used to be text-only — save
 * passed `item.images` unchanged and there was no way to add/remove
 * attachments from the editor. The editor must now manage an `editImages`
 * set: removable thumbnails, paste/drop attach, and save persisting it.
 */
describe("BacklogView inline editor image editing", () => {
  it("saves the edited image set (not the original item.images)", () => {
    // The old bug: `backlogEdit(item.id, editText, item.images)` — every
    // save silently restored the original attachments.
    expect(source).toContain("backlogEdit(item.id, editText, editImages)");
    expect(source).not.toContain("backlogEdit(item.id, editText, item.images)");
  });

  it("renders removable edit thumbnails in the editor strip", () => {
    expect(source).toContain("removeEditImage(i)");
    expect(source).toContain("editImages.map((dataUrl, i)");
  });

  it("attaches paste and drop handlers in edit mode", () => {
    expect(source).toContain("onPaste={handleEditPaste}");
    expect(source).toContain("onDrop={handleEditDrop}");
  });

  it("resets the editor images from the item on cancel/reopen", () => {
    // Both the sync effect and handleCancelEdit must restore item.images so
    // a cancelled edit never persists a discarded attachment.
    expect(source).toContain("setEditImages(item.images)");
  });

  it("keeps image-only items savable (no text required)", () => {
    expect(source).toContain("disabled={!editText.trim() && editImages.length === 0}");
  });

  it("hides the static display thumbnails while editing (single strip)", () => {
    // Review finding 2026-09-11: the display strip rendered unconditionally,
    // duplicating the editor strip (removed images lingered until save).
    expect(source).toContain("{!editing && item.images.length > 0 && (");
  });
});

/**
 * Deferred (skip Run-All) toggle wiring — backlog f2d2809b: the node
 * harness cannot fire onChange, so the checkbox → IPC wiring is pinned on
 * source (the rendered markup is covered by BacklogView.test.tsx).
 */
describe("BacklogView deferred (skip Run-All) — backlog f2d2809b", () => {
  it("the checkbox persists immediately via the backlog_set_deferred IPC", () => {
    expect(source).toContain("backlogSetDeferred(item.id, e.target.checked)");
  });

  it("the checkbox is labeled with the skip-Run-All semantics", () => {
    expect(source).toContain("Deferred (skip Run-All)");
  });

  it("the Run-All warning names the deferred skip", () => {
    expect(source).toContain("deferred items are skipped");
  });
});

/**
 * Parallel run-all (plan ffd7a86f): the checkbox next to auto-feed gates
 * run-all CONCURRENCY only (auto-feed stays sequential — the persistent
 * single-item conveyor). Source-contract tests — the rendered markup is
 * covered by BacklogView.test.tsx.
 */
describe("BacklogView parallel run-all — plan ffd7a86f", () => {
  it("the parallel checkbox renders next to auto-feed and persists via the IPC", () => {
    expect(source).toContain("handleToggleParallelRunAll(e.target.checked)");
    expect(source).toContain("backlogSetParallelRunAll(enabled)");
    const autoFeedTitle = source.indexOf('title="When the main agent is idle');
    const parallelTitle = source.indexOf(
      'title="Run-All dispatches items concurrently',
    );
    expect(autoFeedTitle).toBeGreaterThan(-1);
    expect(parallelTitle).toBeGreaterThan(autoFeedTitle);
  });

  it("the run-all dispatch carries the checkbox-gated concurrency", () => {
    expect(source).toContain("backlogRunAll(parallelRunAll ? 3 : undefined)");
  });

  it("the run-all warning names the parallel semantics", () => {
    expect(source).toContain("Parallel mode: items beyond the first run concurrently");
  });

  it("the progress line shows the parallel in-flight set", () => {
    expect(source).toContain("runAll.spawned.length > 0");
    expect(source).toContain("runAll.note");
  });
});
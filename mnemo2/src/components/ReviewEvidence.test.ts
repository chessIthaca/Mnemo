import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import {
  createAuditWorker,
  createHighRiskWorker,
  createWorkers,
  fixtureState,
} from "../test/fixtures";
import ReviewEvidence from "./ReviewEvidence.svelte";
import WorkerDetail from "./WorkerDetail.svelte";
import WorkerRow from "./WorkerRow.svelte";

const worker = (id: string): Worker => {
  const found = createWorkers().find((item) => item.id === id);
  if (!found) throw new Error(`missing fixture worker ${id}`);
  return found;
};

/** Strip the structured evidence so the record reads as a legacy review. */
const legacy = (source: Worker): Worker => ({
  ...source,
  review: {
    summary: "legacy review of the current tip",
    tip: source.review?.tip ?? "tip-ana",
    reviewedAtMs: 1,
  },
});

const openReviewTab = async () => {
  await fireEvent.click(screen.getByRole("button", { name: /^Review/ }));
};

describe("review evidence", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("renders every recorded assessment for a standard review", () => {
    const ana = worker("ana");
    render(ReviewEvidence, { worker: ana });

    expect(screen.getByRole("region", { name: "Review evidence" })).toBeTruthy();
    expect(screen.getByText("Standard risk")).toBeTruthy();
    expect(screen.getByText(/Unanchored pass recorded/)).toBeTruthy();

    for (const label of [
      /^Intent$/,
      /^Execution path$/,
      /^Integration$/,
      /^Test quality$/,
      /^Residual risk$/,
    ]) {
      expect(screen.getByText(label)).toBeTruthy();
    }
    expect(screen.getByText(ana.review!.evidence!.intent)).toBeTruthy();
    expect(screen.getByText(ana.review!.evidence!.residualRisk)).toBeTruthy();
    // A standard review never claims an independent audit.
    expect(screen.queryByRole("region", { name: "Independent audit" })).toBeNull();
  });

  it("renders the linked audit and adjudication for a high-risk review", () => {
    const mara = createHighRiskWorker();
    const ada = createAuditWorker();
    app.set({ ...fixtureState(), workers: [mara, ada], selectedWorkerId: "mara" });
    render(ReviewEvidence, { worker: mara });

    expect(screen.getByText("High risk")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Independent audit" })).toBeTruthy();
    expect(screen.getByText(/Ada · ada/)).toBeTruthy();
    expect(screen.getByText("tip-mara")).toBeTruthy();
    expect(screen.getByText(mara.review!.evidence!.auditAdjudication!)).toBeTruthy();
    expect(screen.queryByText(/has not adjudicated/)).toBeNull();
  });

  it("asks for a fresh review instead of inventing evidence for a legacy record", () => {
    render(ReviewEvidence, { worker: legacy(worker("ana")) });

    expect(screen.getByText(/predates structured evidence/i)).toBeTruthy();
    expect(screen.queryByText(/^Intent$/)).toBeNull();
    expect(screen.queryByText(/Unanchored pass recorded/)).toBeNull();
  });

  it("flags a high-risk review that has no linked audit yet", () => {
    const mara = createHighRiskWorker();
    const unaudited: Worker = {
      ...mara,
      review: {
        ...mara.review!,
        evidence: {
          ...mara.review!.evidence!,
          independentReviewerId: undefined,
          auditAdjudication: undefined,
        },
      },
    };
    app.set({ ...fixtureState(), workers: [unaudited], selectedWorkerId: "mara" });
    render(ReviewEvidence, { worker: unaudited });

    // With no audit linked the panel asks for one instead of pretending the
    // lead adjudicated something.
    expect(screen.getByText(/No completed independent audit is linked/)).toBeTruthy();
    expect(screen.queryByText(/has not adjudicated this audit yet/)).toBeNull();
  });

  it("shows a missing adjudication for a linked but unjudged audit", () => {
    const mara = createHighRiskWorker();
    const linked: Worker = {
      ...mara,
      review: {
        ...mara.review!,
        evidence: {
          ...mara.review!.evidence!,
          independentReviewerId: "ada",
          auditAdjudication: "  ",
        },
      },
    };
    app.set({
      ...fixtureState(),
      workers: [linked, createAuditWorker()],
      selectedWorkerId: "mara",
    });
    render(ReviewEvidence, { worker: linked });

    expect(screen.getByText(/has not adjudicated this audit yet/)).toBeTruthy();
  });
});

describe("worker lists", () => {
  it("badges audit workers and high-risk reviews so lists stay distinguishable", () => {
    const audit = render(WorkerRow, { worker: createAuditWorker(), onSelect: () => {} });
    expect(within(audit.container).getByText("Audit")).toBeTruthy();
    expect(
      within(audit.container).queryByRole("button", { name: /Merge/ }),
    ).toBeNull();
    audit.unmount();

    const highRisk = render(WorkerRow, { worker: createHighRiskWorker(), onSelect: () => {} });
    expect(within(highRisk.container).getByText("High risk")).toBeTruthy();
    expect(within(highRisk.container).queryByText("Audit")).toBeNull();
  });
});

describe("worker detail review surface", () => {
  beforeEach(() => {
    app.set(fixtureState());
  });

  it("shows the evidence panel and merge action on the review tab", async () => {
    render(WorkerDetail, { worker: worker("ana") });

    await openReviewTab();
    expect(screen.getByRole("region", { name: "Review evidence" })).toBeTruthy();
    expect(screen.getByText("Standard risk")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Merge reviewed branch" })).toBeTruthy();
  });

  it("marks a high-risk implementation worker in the hero", () => {
    const mara = createHighRiskWorker();
    app.set({ ...fixtureState(), workers: [mara, createAuditWorker()], selectedWorkerId: "mara" });
    render(WorkerDetail, { worker: mara });

    expect(screen.getByText("High risk")).toBeTruthy();
    expect(screen.queryByText("Audit")).toBeNull();
  });

  it("distinguishes an audit worker and offers it no merge action", async () => {
    const ada = createAuditWorker();
    app.set({
      ...fixtureState(),
      workers: [createHighRiskWorker(), ada],
      selectedWorkerId: "ada",
    });
    render(WorkerDetail, { worker: ada });

    expect(screen.getByText("Audit")).toBeTruthy();
    expect(screen.getByText(/Read-only audit of/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Merge reviewed branch" })).toBeNull();

    await openReviewTab();
    expect(screen.getByText(/Audits never merge/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Merge reviewed branch" })).toBeNull();
  });
});

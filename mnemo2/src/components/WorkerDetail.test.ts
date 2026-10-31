import { render } from "@testing-library/svelte";
import { beforeEach, describe, expect, it } from "vitest";
import { app } from "../lib/store";
import type { Worker } from "../lib/types";
import { createWorkers, fixtureState } from "../test/fixtures";
import WorkerDetail from "./WorkerDetail.svelte";

const longBrief = [
  "# Plan: Short worker summaries",
  "",
  "Goal: Mission control shows each worker's task as **at most three lines**.",
  "",
  "- add a briefSummary helper in mnemo2/src/lib/brief.ts",
  "- clamp the row summary with CSS",
  "- keep the full brief one click away in the detail",
  "",
  "Verify with npm test and npm run check.",
].join("\n");

const worker = (): Worker => ({ ...createWorkers()[0], task: longBrief });

describe("WorkerDetail brief", () => {
  beforeEach(() => app.set(fixtureState()));

  it("renders the full brief as Markdown inside the collapsible, not as a heading", () => {
    const { container } = render(WorkerDetail, { worker: worker() });
    // Structure only: jsdom cannot measure the collapsible's overflow.
    const brief = container.querySelector(".worker-brief .collapsible .markdown");
    expect(brief).toBeTruthy();
    // The whole Markdown brief is rendered, markers become real elements.
    expect(brief?.querySelector("h1")?.textContent).toBe("Plan: Short worker summaries");
    expect(brief?.querySelector("strong")?.textContent).toBe("at most three lines");
    expect([...(brief?.querySelectorAll("li") ?? [])].map((li) => li.textContent)).toContain(
      "add a briefSummary helper in mnemo2/src/lib/brief.ts",
    );
    // No heading element in the detail header carries the raw brief.
    expect(container.querySelector(".worker-heading h3")).toBeNull();
    expect(container.querySelector("h3")?.textContent ?? "").not.toBe(longBrief);
  });
});
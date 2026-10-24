import { describe, expect, it } from "vitest";
import { createWorkers } from "./mock";
import { deriveTaskWorkflow } from "./workflow";
import type { Worker } from "./types";

const worker = (id: string): Worker => {
  const found = createWorkers().find((item) => item.id === id);
  if (!found) throw new Error(`missing mock worker ${id}`);
  return found;
};

const step = (workflow: ReturnType<typeof deriveTaskWorkflow>, id: string) =>
  workflow.steps.find((item) => item.id === id);

describe("deriveTaskWorkflow", () => {
  it("derives independent workflows for tasks running in parallel", () => {
    const ana = deriveTaskWorkflow(worker("ana"));
    const kai = deriveTaskWorkflow(worker("kai"));
    const milo = deriveTaskWorkflow(worker("milo"));
    const theo = deriveTaskWorkflow(worker("theo"));
    const june = deriveTaskWorkflow(worker("june"));

    expect(ana.current).toBe("review");
    expect(ana.state).toBe("active");
    expect(step(ana, "review")?.status).toBe("active");
    expect(step(ana, "merge")?.status).toBe("pending");

    expect(kai.current).toBe("implement");
    expect(step(kai, "implement")?.status).toBe("active");

    expect(milo.current).toBe("verify");
    expect(step(milo, "verify")?.status).toBe("active");

    expect(theo.current).toBe("merge");
    expect(theo.state).toBe("blocked");
    expect(step(theo, "merge")?.status).toBe("blocked");

    expect(june.current).toBe("brief");
    expect(step(june, "brief")?.status).toBe("active");
  });

  it("always exposes the five task-local steps in order", () => {
    for (const item of createWorkers()) {
      const workflow = deriveTaskWorkflow(item);
      expect(workflow.steps.map((entry) => entry.id)).toEqual([
        "brief",
        "implement",
        "verify",
        "review",
        "merge",
      ]);
    }
  });

  it("advances to merge only when this task's findings and checks are closed", () => {
    const ana = worker("ana");
    const closed = {
      ...ana,
      findings: ana.findings.map((finding) => ({ ...finding, status: "fixed" as const })),
    };
    const workflow = deriveTaskWorkflow(closed);

    expect(workflow.current).toBe("merge");
    expect(workflow.state).toBe("active");
    expect(step(workflow, "review")?.status).toBe("done");
    expect(step(workflow, "merge")?.status).toBe("active");
  });

  it("marks failed, cancelled, and merged tasks explicitly", () => {
    expect(deriveTaskWorkflow(worker("noa")).state).toBe("blocked");
    expect(deriveTaskWorkflow(worker("noa")).current).toBe("implement");
    expect(deriveTaskWorkflow(worker("liv")).state).toBe("cancelled");
    expect(deriveTaskWorkflow(worker("ren")).state).toBe("done");
    expect(deriveTaskWorkflow(worker("ren")).steps.every((item) => item.status === "done")).toBe(true);
  });
});

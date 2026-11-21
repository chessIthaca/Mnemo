// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Unit tests for the job-call presentation helpers (`./jobCards.ts`): the
 * collapsed title (`job · <purpose> — <state>`) and the argument/result data
 * the expanded body reads. The state comes from the plan-5b498359 interface
 * (`state` + `exit_code`); a pre-plan transcript's human state text and its
 * argument-only fields are the fallback.
 */

import { describe, expect, it } from "vitest";

import { isJobTool, jobCallFrom, jobCallFromArgs, jobHeadline, jobOutcome } from "./jobCards";

describe("isJobTool", () => {
  it("recognises the job tools and both start names", () => {
    for (const name of ["job", "job_start", "job_wait", "job_tail", "job_kill"]) {
      expect(isJobTool(name)).toBe(true);
    }
    for (const name of ["shell", "run_checks", "job_startx", ""]) {
      expect(isJobTool(name)).toBe(false);
    }
  });
});

describe("jobCallFromArgs", () => {
  it("reads a start call's command, purpose and cwd", () => {
    expect(
      jobCallFromArgs(
        '{"command":"cargo build","purpose":"build game","cwd":"src-tauri","wait_s":0}',
      ),
    ).toEqual({ jobId: null, purpose: "build game", command: "cargo build", cwd: "src-tauri" });
  });

  it("reads the job id of a follow-up call", () => {
    expect(jobCallFromArgs('{"job_id":"j3","timeout_s":30}')).toEqual({
      jobId: "j3",
      purpose: null,
      command: null,
      cwd: null,
    });
  });

  it("yields empty fields for a still-streaming fragment", () => {
    expect(jobCallFromArgs('{"command":"cargo bu')).toEqual({
      jobId: null,
      purpose: null,
      command: null,
      cwd: null,
    });
  });
});

describe("jobOutcome", () => {
  it("reports a call with no result yet as running", () => {
    expect(jobOutcome(null)).toEqual({ label: "running", exitCode: null });
  });

  it("renders the interface's exited state with its exit code", () => {
    expect(jobOutcome({ data: { state: "exited", exit_code: 0 } })).toEqual({
      label: "exited 0",
      exitCode: 0,
    });
    expect(jobOutcome({ data: { state: "exited", exit_code: 1 } })).toEqual({
      label: "exited 1",
      exitCode: 1,
    });
  });

  it("renders an exited state that carries no exit code", () => {
    expect(jobOutcome({ data: { state: "exited" } })).toEqual({ label: "exited", exitCode: null });
  });

  it("renders a waited-on job that is still running", () => {
    expect(jobOutcome({ data: { state: "running" } })).toEqual({
      label: "still running",
      exitCode: null,
    });
  });

  it("renders killed and failed states", () => {
    expect(jobOutcome({ data: { state: "killed" } })).toEqual({ label: "killed", exitCode: null });
    expect(jobOutcome({ data: { state: "failed" } })).toEqual({ label: "failed", exitCode: null });
  });

  it("understands a pre-plan transcript's human state text", () => {
    // JobStatus::text in src/tool/agent/jobs.rs before the interface changed.
    expect(jobOutcome({ data: { state: "exited 2" } })).toEqual({
      label: "exited 2",
      exitCode: 2,
    });
    expect(jobOutcome({ data: { state: "still running" } })).toEqual({
      label: "still running",
      exitCode: null,
    });
    expect(jobOutcome({ data: { state: "killed (job_kill)" } })).toEqual({
      label: "killed",
      exitCode: null,
    });
    expect(jobOutcome({ data: { state: "failed to start (no shell)" } })).toEqual({
      label: "failed",
      exitCode: null,
    });
  });

  it("is null when the result carries no state at all", () => {
    expect(jobOutcome({ data: undefined })).toBeNull();
    expect(jobOutcome({ data: { job_id: "j1" } })).toBeNull();
    expect(jobOutcome({ data: { state: "confused" } })).toBeNull();
  });
});

describe("jobCallFrom", () => {
  it("is null for every tool that is not a job tool", () => {
    expect(jobCallFrom("shell", '{"command":"cargo build","purpose":"build"}', null)).toBeNull();
  });

  it("marks job and job_start as starts, the id tools as follow-ups", () => {
    expect(jobCallFrom("job", '{"command":"cargo build","purpose":"build"}', null)?.start).toBe(true);
    expect(jobCallFrom("job_start", '{"command":"cargo build","purpose":"build"}', null)?.start).toBe(
      true,
    );
    expect(jobCallFrom("job_wait", '{"job_id":"j1"}', null)?.start).toBe(false);
    expect(jobCallFrom("job_tail", '{"job_id":"j1"}', null)?.start).toBe(false);
    expect(jobCallFrom("job_kill", '{"job_id":"j1"}', null)?.start).toBe(false);
  });

  it("completes a follow-up call from the result's data", () => {
    // job_wait's arguments name only the id; the result carries the job's label.
    expect(
      jobCallFrom("job_wait", '{"job_id":"j1"}', {
        data: { job_id: "j1", purpose: "build game", command: "cargo build", state: "exited", exit_code: 0 },
      }),
    ).toEqual({
      start: false,
      jobId: "j1",
      purpose: "build game",
      command: "cargo build",
      cwd: null,
      outcome: { label: "exited 0", exitCode: 0 },
    });
  });
});

describe("jobHeadline", () => {
  it("titles a start call with its purpose and exit state", () => {
    expect(
      jobHeadline("job", '{"command":"cargo build","purpose":"build game"}', {
        data: { job_id: "j1", purpose: "build game", command: "cargo build", state: "exited", exit_code: 0 },
      }),
    ).toEqual({ text: "job · build game — exited 0", title: "job · build game — exited 0" });
  });

  it("titles a call that has not returned yet as running", () => {
    expect(
      jobHeadline("job", '{"command":"cargo build","purpose":"build game"}', null),
    ).toEqual({ text: "job · build game — running", title: "job · build game — running" });
  });

  it("titles a waited-on still-running job", () => {
    expect(
      jobHeadline("job_wait", '{"job_id":"j1"}', {
        data: { job_id: "j1", purpose: "build game", state: "running" },
      })?.text,
    ).toBe("job · build game — still running");
  });

  it("titles killed and failed jobs", () => {
    expect(
      jobHeadline("job_kill", '{"job_id":"j7"}', {
        data: { job_id: "j7", purpose: "dev server", state: "killed" },
      })?.text,
    ).toBe("job · dev server — killed");
    expect(
      jobHeadline("job", '{"command":"cargo build","purpose":"build game"}', {
        data: { state: "failed" },
      })?.text,
    ).toBe("job · build game — failed");
  });

  it("titles an exited job with a non-zero code", () => {
    expect(
      jobHeadline("job", '{"command":"cargo test","purpose":"run tests"}', {
        data: { state: "exited", exit_code: 1 },
      })?.text,
    ).toBe("job · run tests — exited 1");
  });

  it("falls back to the shortened command when the call has no purpose", () => {
    const long =
      "cargo build --workspace --all-targets --release --features embeddings --timings --verbose --jobs 8 --no-default-features";
    const { text, title } = jobHeadline("job", JSON.stringify({ command: long }), null)!;
    expect(text.startsWith("job · cargo build --workspace")).toBe(true);
    expect(text.endsWith("… — running")).toBe(true);
    // The tooltip keeps the full command.
    expect(title).toBe(`job · ${long} — running`);
  });

  it("titles a job_tail call with the purpose its result carries", () => {
    expect(
      jobHeadline("job_tail", '{"job_id":"j2","lines":20}', {
        data: { job_id: "j2", purpose: "run tests", state: "running" },
      })?.text,
    ).toBe("job · run tests — still running");
  });

  it("uses the purpose carried by a follow-up result", () => {
    expect(
      jobHeadline("job_wait", '{"job_id":"j3","timeout_s":10}', {
        data: { job_id: "j3", purpose: "build game", command: "cargo build", state: "exited", exit_code: 0 },
      })?.text,
    ).toBe("job · build game — exited 0");
  });

  it("falls back to the job id for an old follow-up without a purpose", () => {
    // A pre-plan transcript: the arguments name the id, the result has only the
    // legacy human state text.
    expect(
      jobHeadline("job_wait", '{"job_id":"j3","timeout_s":10}', {
        data: { job_id: "j3", state: "exited 2" },
      })?.text,
    ).toBe("job · j3 — exited 2");
  });

  it("still titles an old job_start transcript from its arguments", () => {
    // No result data at all (the pre-plan tool), so no state suffix is known.
    const oldResult = { success: true, output: "started job j1" };
    expect(
      jobHeadline("job_start", '{"command":"cargo build","purpose":"build game"}', oldResult),
    ).toEqual({ text: "job · build game", title: "job · build game" });
  });

  it("keeps a purpose on one line", () => {
    expect(
      jobHeadline("job", '{"command":"cargo build","purpose":"build game\\nsecond line"}', null)?.text,
    ).toBe("job · build game — running");
  });

  it("is null for a job call whose arguments and result name nothing", () => {
    expect(jobHeadline("job", "{}", { data: {} })).toBeNull();
    expect(jobHeadline("job", '{"command":"cargo bu', null)).toBeNull();
  });

  it("is null for every other tool, leaving its headline untouched", () => {
    expect(jobHeadline("shell", '{"command":"cargo test","purpose":"running tests"}', null)).toBeNull();
    expect(jobHeadline("git_read", '{"op":"log"}', null)).toBeNull();
  });
});

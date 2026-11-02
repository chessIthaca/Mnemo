/** The plan board as the native host reports it (`board_snapshot`). */
export type BoardPlanStatus = "open" | "closed" | "abandoned";

export interface BoardStepWorker {
  id: string;
  name: string;
  status: string;
}

export interface BoardStep {
  /** 1-based. */
  number: number;
  header: string | null;
  text: string;
  done: boolean;
  workers: BoardStepWorker[];
}

export interface BoardPlan {
  id: string;
  title: string;
  goal: string;
  kind: string;
  status: BoardPlanStatus;
  closed_at_ms: number | null;
  report_path: string | null;
  steps: BoardStep[];
}

export interface BoardSnapshot {
  plans: BoardPlan[];
}

export const emptyBoard: BoardPlan[] = [];

/** Normalize a snapshot into the stored board; open plans first, newest closes first. */
export function boardFromSnapshot(snapshot: BoardSnapshot | null | undefined): BoardPlan[] {
  const plans = (snapshot?.plans ?? []).map((plan) => ({
    ...plan,
    steps: plan.steps ?? [],
  }));
  const open = plans.filter((plan) => plan.status === "open");
  const closed = plans
    .filter((plan) => plan.status !== "open")
    .sort((a, b) => (b.closed_at_ms ?? 0) - (a.closed_at_ms ?? 0));
  return [...open, ...closed];
}

export function openPlans(board: BoardPlan[]): BoardPlan[] {
  return board.filter((plan) => plan.status === "open");
}

export function closedPlans(board: BoardPlan[]): BoardPlan[] {
  return board.filter((plan) => plan.status !== "open");
}

export function planProgress(plan: BoardPlan): { done: number; total: number } {
  return { done: plan.steps.filter((step) => step.done).length, total: plan.steps.length };
}

/** A plan's title, or the first 8 characters of its id when the board lacks it. */
export function planTitle(board: BoardPlan[], planId: string): string {
  return board.find((plan) => plan.id === planId)?.title || planId.slice(0, 8);
}

/** "Plan title · step N" for a worker stamped with a plan, else null. */
export function workerPlanLabel(
  board: BoardPlan[],
  worker: { planId?: string | null; planStep?: number | null },
): string | null {
  if (!worker.planId) return null;
  const title = planTitle(board, worker.planId);
  return worker.planStep == null ? title : `${title} · step ${worker.planStep + 1}`;
}

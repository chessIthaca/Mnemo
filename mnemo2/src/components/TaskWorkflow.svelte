<script lang="ts">
  import type { TaskWorkflow, TaskWorkflowStepId } from "../lib/types";

  let {
    workflow,
    compact = false,
    onselect,
  }: {
    workflow: TaskWorkflow;
    compact?: boolean;
    onselect?: (stepId: TaskWorkflowStepId) => void;
  } = $props();

  const current = $derived(workflow.steps.find((step) => step.id === workflow.current) ?? workflow.steps[0]);
  const currentIndex = $derived(Math.max(0, workflow.steps.findIndex((step) => step.id === workflow.current)));
  const summary = $derived(
    workflow.steps.map((step) => `${step.label}${step.id === workflow.current ? " (current)" : ""}`).join(" → "),
  );
</script>

{#if compact}
  <span
    class="workflow-compact"
    data-state={workflow.state}
    aria-label={`Task workflow: ${summary}`}
    title={`${current.label}: ${current.detail}`}
  ><span class="dot" data-status={current.status} aria-hidden="true"></span>{current.label}<small>{currentIndex + 1}/{workflow.steps.length}</small></span>
{:else}
  <ol class="task-workflow" data-state={workflow.state} aria-label={`Task workflow: ${summary}`}>
    {#each workflow.steps as step (step.id)}
      <li data-status={step.status} aria-current={step.id === workflow.current ? "step" : undefined} title={step.detail}>
        {#if onselect}
          <button type="button" aria-label={`Workflow: ${step.label}`} onclick={() => onselect(step.id)}>
            <span class="dot" data-status={step.status} aria-hidden="true"></span><span>{step.label}</span>
          </button>
        {:else}
          <span class="dot" data-status={step.status} aria-hidden="true"></span><span>{step.label}</span>
        {/if}
      </li>
    {/each}
  </ol>
{/if}

<style>
  .workflow-compact { display: inline-flex; align-items: center; gap: 4px; color: var(--ink-soft); font-size: var(--text-meta); font-weight: 600; letter-spacing: 0.02em; white-space: nowrap; }
  .workflow-compact small { color: var(--muted); font-size: var(--text-meta); font-weight: 400; }
  .workflow-compact[data-state="blocked"] { color: var(--danger, #a34a3a); }
  .workflow-compact[data-state="done"] { color: var(--muted); }
  .task-workflow { display: flex; flex-wrap: wrap; align-items: center; gap: 3px 10px; margin: 0; padding: 0; list-style: none; }
  .task-workflow li { display: inline-flex; align-items: center; gap: 4px; color: var(--muted); font-size: var(--text-meta); font-weight: 500; letter-spacing: 0.02em; white-space: nowrap; }
  .task-workflow li[data-status="active"] { color: var(--ink); font-weight: 650; }
  .task-workflow li[data-status="done"] { color: var(--ink-soft); }
  .task-workflow li[data-status="blocked"] { color: var(--danger, #a34a3a); }
  .task-workflow li button { display: inline-flex; align-items: center; gap: 4px; padding: 2px 4px; border: 0; border-radius: 4px; background: transparent; color: inherit; cursor: pointer; font: inherit; letter-spacing: inherit; }
  .task-workflow li button:hover { background: var(--surface-soft); }
  .dot { width: 4px; height: 4px; flex: 0 0 auto; border-radius: 50%; background: currentColor; opacity: 0.35; }
  .dot[data-status="active"] { background: var(--accent); opacity: 1; }
  .dot[data-status="done"] { opacity: 0.7; }
  .dot[data-status="blocked"] { background: var(--danger, #a34a3a); opacity: 1; }
</style>

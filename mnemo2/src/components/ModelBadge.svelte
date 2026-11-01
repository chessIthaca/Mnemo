<script lang="ts">
  let {
    model,
    role = "worker",
  }: {
    model: string;
    role?: "lead" | "worker";
  } = $props();
</script>

<span
  class="model-badge"
  data-role={role}
  aria-label={`${role === "lead" ? "Lead" : "Worker"} model: ${model}`}
  title={`${role === "lead" ? "Lead" : "Worker"} model: ${model}`}
><span class="dot" aria-hidden="true"></span><span class="model-name">{model}</span></span>

<style>
  .model-badge { display: inline-flex; align-items: center; gap: 4px; min-width: 0; max-width: 100%; color: var(--muted); font-size: var(--text-meta); font-weight: 500; line-height: 1.2; white-space: nowrap; }
  .model-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .dot { width: 4px; height: 4px; flex: 0 0 auto; border-radius: 50%; background: var(--muted); opacity: 0.7; }
  .model-badge[data-role="lead"] { color: var(--accent-strong); font-weight: 600; }
  .model-badge[data-role="lead"] .dot { background: var(--accent); opacity: 1; }
</style>

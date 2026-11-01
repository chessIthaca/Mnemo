<script lang="ts">
  import { actions, app } from "../lib/store";
  import { deletionDiscardsWork, type WorkerDeletionPreview } from "../lib/teamState";
  import Sheet from "./Sheet.svelte";

  const request = $derived($app.workerDeletion);
  const previews = $derived(request?.previews ?? []);
  const deletable = $derived(previews.filter((preview) => preview.blocked === null));
  const kept = $derived(previews.filter((preview) => preview.blocked !== null));
  /** Deletable previews whose discarded work no other worker already landed. */
  const losing = $derived(deletable.filter((preview) => deletionDiscardsWork(preview) && preview.superseded_by === null));
  const busy = $derived(request?.busy === true);
  const title = $derived(
    deletable.length === 1 ? `Delete ${deletable[0].name}?` : `Delete ${deletable.length} workers?`,
  );

  /** One line naming what deleting this worker removes. */
  function removes(preview: WorkerDeletionPreview): string {
    const parts: string[] = [];
    if (preview.live) parts.push("stops its running session");
    if (preview.shared_branch) {
      parts.push(`keeps ${preview.branch}, which another worker still uses`);
    } else {
      if (preview.checkout) parts.push("removes its checkout");
      parts.push(`deletes ${preview.branch}`);
    }
    return parts.join(" · ");
  }

  /** The work the lead branch lacks that this delete throws away. */
  function loss(preview: WorkerDeletionPreview): string {
    const parts: string[] = [];
    if (preview.unmerged_commits > 0) {
      parts.push(`${preview.unmerged_commits} unmerged commit${preview.unmerged_commits === 1 ? "" : "s"}`);
    }
    if (preview.uncommitted) parts.push("uncommitted changes");
    return parts.join(" and ");
  }
</script>

<Sheet open={request !== null} {title} eyebrow="WORKER HISTORY" onclose={actions.cancelDeleteWorkers}>
  {#if deletable.length > 0}
    <p class="dialog-copy">
      Deleting removes each worker's record, conversation, telemetry, review, and messages. This cannot be undone.
    </p>
  {:else}
    <p class="dialog-copy">None of these workers can be deleted right now.</p>
  {/if}
  <ul class="deletion-list">
    {#each deletable as preview (preview.worker_id)}
      <li data-loss={deletionDiscardsWork(preview) && preview.superseded_by === null}>
        <strong>{preview.name}</strong>
        <span>{removes(preview)}</span>
        {#if deletionDiscardsWork(preview)}
          {#if preview.superseded_by}
            <em>Its step landed through {preview.superseded_by}; its own {loss(preview)} not on the lead branch will be discarded.</em>
          {:else}<em>Discards {loss(preview)}.</em>{/if}
        {/if}
      </li>
    {/each}
    {#each kept as preview (preview.worker_id)}
      <li class="kept">
        <strong>{preview.name}</strong>
        <span>Kept: {preview.blocked}</span>
      </li>
    {/each}
  </ul>
  {#if losing.length > 0}
    <p class="loss-warning" role="alert">
      {losing.length === 1 ? "1 worker has" : `${losing.length} workers have`} work that is not on the lead branch. Merge it first if you need it.
    </p>
  {/if}
  <div class="dialog-actions">
    <button class="button button-quiet" disabled={busy} onclick={actions.cancelDeleteWorkers}>Cancel</button>
    <button class="button button-danger" disabled={busy || deletable.length === 0} onclick={actions.confirmDeleteWorkers}>
      {busy ? "Deleting…" : losing.length > 0 ? "Delete and discard work" : "Delete"}
    </button>
  </div>
</Sheet>

<style>
  .dialog-copy { margin: 0 0 12px; color: var(--muted); font-size: var(--text-body); line-height: 1.65; }
  .deletion-list { display: grid; gap: 6px; max-height: 280px; margin: 0 0 12px; padding: 0; overflow-y: auto; list-style: none; }
  .deletion-list li { display: grid; gap: 2px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-soft); }
  .deletion-list li[data-loss="true"] { border-color: #e3c7c2; background: #fbf3f1; }
  .deletion-list li.kept { opacity: 0.75; }
  .deletion-list strong { color: var(--ink); font-size: var(--text-body); font-weight: 650; }
  .deletion-list span { color: var(--muted); font-size: var(--text-body); overflow-wrap: anywhere; }
  .deletion-list em { color: var(--danger); font-size: var(--text-body); font-style: normal; font-weight: 600; }
  .loss-warning { margin: 0 0 12px; color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; padding-top: 14px; border-top: 1px solid var(--line); }
  .button-danger { background: var(--danger); color: white; }
  .button-danger:hover:not(:disabled) { background: #8a4039; }
</style>

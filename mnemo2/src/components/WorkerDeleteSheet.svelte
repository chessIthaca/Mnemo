<script lang="ts">
  import { LoaderCircle } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import { deletionDiscardsWork, type WorkerDeletionPreview } from "../lib/teamState";
  import type { WorkerDeletionRow } from "../lib/types";
  import Sheet from "./Sheet.svelte";

  const request = $derived($app.workerDeletion);
  const rows = $derived(request?.rows ?? []);
  const checking = $derived(request?.phase === "checking");
  const deleting = $derived(request?.phase === "deleting");
  const deletable = $derived(
    rows.filter((row) => row.preview && row.preview.blocked === null && !row.deleted),
  );
  const kept = $derived(rows.filter((row) => row.preview && row.preview.blocked !== null));
  /** Deletable previews whose discarded work no other worker already landed. */
  const losing = $derived(
    deletable.filter(
      (row) =>
        row.preview !== null &&
        deletionDiscardsWork(row.preview) &&
        row.preview.superseded_by === null,
    ),
  );
  /** Workers whose deletion failed and can be retried. */
  const failed = $derived(rows.filter((row) => row.error !== null && !row.deleted));
  const title = $derived(
    checking
      ? "Checking workers…"
      : deleting
        ? "Deleting workers…"
        : failed.length > 0
          ? "Some workers failed to delete"
          : deletable.length === 1
            ? `Delete ${deletable[0].name}?`
            : `Delete ${deletable.length} workers?`,
  );

  /** What one row of the sheet currently shows. */
  type RowState = "deleted" | "failed" | "checking" | "deleting" | "kept" | "ready";

  function rowState(row: WorkerDeletionRow): RowState {
    if (row.deleted) return "deleted";
    if (row.error) return "failed";
    if (row.preview === null) return "checking";
    if (row.preview.blocked !== null) return "kept";
    if (deleting) return "deleting";
    return "ready";
  }

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
      parts.push(
        `${preview.unmerged_commits} unmerged commit${preview.unmerged_commits === 1 ? "" : "s"}`,
      );
    }
    if (preview.uncommitted) parts.push("uncommitted changes");
    return parts.join(" and ");
  }
</script>

<Sheet open={request !== null} {title} eyebrow="WORKER HISTORY" onclose={actions.cancelDeleteWorkers}>
  {#if checking}
    <p class="dialog-copy">Checking what deleting each worker removes…</p>
  {:else if deletable.length > 0 || failed.length > 0}
    <p class="dialog-copy">
      Deleting removes each worker's record, conversation, telemetry, review, and messages. This cannot be undone.
    </p>
  {:else}
    <p class="dialog-copy">None of these workers can be deleted right now.</p>
  {/if}
  <ul class="deletion-list">
    {#each rows as row (row.workerId)}
      {@const state = rowState(row)}
      {@const preview = row.preview}
      <li
        class:kept={state === "kept"}
        class:done={state === "deleted"}
        data-loss={state === "ready" &&
          preview !== null &&
          deletionDiscardsWork(preview) &&
          preview.superseded_by === null}
      >
        <strong>{row.name}</strong>
        {#if state === "deleted"}
          <span class="row-done">Deleted.</span>
        {:else if state === "failed"}
          <span class="row-error">Failed: {row.error}</span>
        {:else if state === "checking"}
          <span class="row-pending">
            <span class="row-spinner"><LoaderCircle size={11} aria-hidden="true" /></span>
            Checking…
          </span>
        {:else if state === "deleting"}
          <span class="row-pending">
            <span class="row-spinner"><LoaderCircle size={11} aria-hidden="true" /></span>
            Deleting…
          </span>
        {:else if state === "kept" && preview !== null}
          <span>Kept: {preview.blocked}</span>
        {:else if preview !== null}
          <span>{removes(preview)}</span>
          {#if deletionDiscardsWork(preview)}
            {#if preview.superseded_by}
              <em>Its step landed through {preview.superseded_by}; its own {loss(preview)} not on the lead branch will be discarded.</em>
            {:else}<em>Discards {loss(preview)}.</em>{/if}
          {/if}
        {/if}
      </li>
    {/each}
  </ul>
  {#if !checking && !deleting && losing.length > 0}
    <p class="loss-warning" role="alert">
      {losing.length === 1 ? "1 worker has" : `${losing.length} workers have`} work that is not on the lead branch. Merge it first if you need it.
    </p>
  {/if}
  <div class="dialog-actions">
    <button class="button button-quiet" disabled={deleting} onclick={actions.cancelDeleteWorkers}>Cancel</button>
    {#if failed.length > 0}
      <button class="button button-danger" disabled={checking || deleting} onclick={actions.confirmDeleteWorkers}>
        {#if deleting}
          Retrying…
        {:else}
          Retry {failed.length === 1 ? "the failed worker" : `${failed.length} failed workers`}
        {/if}
      </button>
    {:else}
      <button
        class="button button-danger"
        disabled={checking || deleting || deletable.length === 0}
        onclick={actions.confirmDeleteWorkers}
      >
        {#if checking}
          Checking…
        {:else if deleting}
          Deleting…
        {:else if losing.length > 0}
          Delete and discard work
        {:else}
          Delete
        {/if}
      </button>
    {/if}
  </div>
</Sheet>

<style>
  .dialog-copy { margin: 0 0 12px; color: var(--muted); font-size: var(--text-body); line-height: 1.65; }
  .deletion-list { display: grid; gap: 6px; max-height: 280px; margin: 0 0 12px; padding: 0; overflow-y: auto; list-style: none; }
  .deletion-list li { display: grid; gap: 2px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-soft); }
  .deletion-list li[data-loss="true"] { border-color: var(--danger-tint-line); background: var(--danger-tint); }
  .deletion-list li.kept { opacity: 0.75; }
  .deletion-list li.done { opacity: 0.75; }
  .deletion-list strong { color: var(--ink); font-size: var(--text-body); font-weight: 650; }
  .deletion-list span { color: var(--muted); font-size: var(--text-body); overflow-wrap: anywhere; }
  .deletion-list em { color: var(--danger); font-size: var(--text-body); font-style: normal; font-weight: 600; }
  .row-pending { display: flex; align-items: center; gap: 6px; }
  .row-spinner { display: grid; place-items: center; animation: spin 1s linear infinite; }
  .row-done { color: var(--muted); }
  .row-error { color: var(--danger); overflow-wrap: anywhere; }
  .loss-warning { margin: 0 0 12px; color: var(--danger); font-size: var(--text-body); line-height: 1.5; }
  .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; padding-top: 14px; border-top: 1px solid var(--line); }
  .button-danger { background: var(--danger); color: var(--on-danger); }
  .button-danger:hover:not(:disabled) { background: var(--danger-hover); }
</style>
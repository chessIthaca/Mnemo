<script lang="ts">
  import { FileDiff, RotateCw } from "@lucide/svelte";
  import { onMount } from "svelte";
  import { actions, app } from "../lib/store";

  const workerId = $derived($app.diffWorkerId ?? $app.selectedWorkerId ?? $app.workers[0]?.id ?? null);
  const worker = $derived($app.workers.find((item) => item.id === workerId));
  const totals = $derived($app.diff ? {
    files: $app.diff.total_files,
    additions: $app.diff.total_additions,
    deletions: $app.diff.total_deletions,
  } : $app.diffFiles.reduce(
    (sum, file) => ({
      files: sum.files + 1,
      additions: sum.additions + file.additions,
      deletions: sum.deletions + file.deletions,
    }),
    { files: 0, additions: 0, deletions: 0 },
  ));

  onMount(() => {
    if ($app.leadRuntime.mode === "native") void actions.refreshDiff();
  });
</script>

<section class="diff-tab" aria-label="Diff">
  <header class="diff-header">
    <div>
      <p>WORKER BRANCH · GIT DELTA</p>
      <h2>Diff</h2>
      <span>{$app.diff?.branch ?? worker?.branch ?? "No worker selected"} → {$app.diff?.target ?? "lead branch"}</span>
    </div>
    <div class="diff-totals"><strong>{totals.files} files</strong><span>+{totals.additions}</span><span class="deletions">−{totals.deletions}</span></div>
  </header>

  <div class="diff-controls">
    <select aria-label="Diff worker" value={workerId ?? ""} onchange={(event) => actions.selectDiffWorker(event.currentTarget.value)}>
      {#each $app.workers as item (item.id)}<option value={item.id}>{item.name} · {item.branch}</option>{/each}
    </select>
    <button class="button button-quiet" aria-label="Refresh diff" disabled={!workerId || $app.diffLoading} onclick={() => actions.refreshDiff()}><RotateCw size={12} /> Refresh</button>
  </div>

  {#if $app.diffError}<div class="diff-error" role="alert">{$app.diffError}</div>{/if}
  {#if $app.diffLoading}<p class="diff-note" role="status">Reading the worker’s Git delta…</p>{/if}
  {#if $app.diff}<div class="diff-revisions"><code>base {$app.diff.base}</code><code>tip {$app.diff.tip}</code></div>{/if}
  {#each $app.diffFiles as file (`${file.old_path ?? ""}:${file.path}`)}
    <article class="diff-file">
      <header><FileDiff size={12} /><code>{#if file.old_path && file.old_path !== file.path}{file.old_path} → {/if}{file.path}</code><span class="additions">+{file.additions}</span><span class="deletions">−{file.deletions}</span></header>
      {#if file.sensitive}<p class="file-note">Sensitive file content is withheld.</p>{/if}
      {#if file.binary}<p class="file-note">Binary file · no text preview</p>{/if}
      {#if file.truncated && file.lines.length === 0 && !file.binary && !file.sensitive}<p class="file-note">Preview omitted because the file is large or protected.</p>{/if}
      <div class="diff-lines">
        {#each file.lines as line, index (index)}
          <div class="diff-line" data-type={line.type}><span class="line-number">{line.old_line ?? ""}</span><span class="line-number">{line.new_line ?? ""}</span><span class="line-sign">{line.type === "add" ? "+" : line.type === "remove" ? "−" : line.type === "hunk" ? "@" : " "}</span><code>{line.text}</code></div>
        {/each}
      </div>
      {#if file.truncated && file.lines.length > 0}<p class="file-note">Some lines are omitted; the file totals are unchanged.</p>{/if}
    </article>
  {:else}
    {#if !$app.diffLoading && !$app.diffError}
      <p class="diff-note">{workerId ? "No changes in this worker’s delta." : "No worker branches yet."}</p>
    {/if}
  {/each}
  {#if $app.diff?.truncated}<p class="diff-note">Preview truncated. Totals include omitted diff lines and files.</p>{/if}
</section>

<style>
  .diff-tab { padding: 18px; }
  .diff-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 16px; margin-bottom: 16px; }
  .diff-header p { margin: 0 0 4px; color: var(--muted); font-size: var(--text-meta); letter-spacing: 0.11em; }
  .diff-header h2 { margin: 0 0 3px; font-family: var(--font-prose); font-size: var(--text-page); font-weight: 550; }
  .diff-header div > span { color: var(--muted); font-family: var(--font-mono); font-size: var(--text-body); }
  .diff-totals { display: flex; gap: 8px; color: var(--muted); font-size: var(--text-body); }
  .diff-totals strong { color: var(--ink-soft); font-weight: 600; }
  .additions { color: var(--stat-add); }
  .deletions { color: var(--stat-remove); }
  .diff-file { margin-bottom: 12px; overflow: hidden; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
  .diff-file > header { display: flex; align-items: center; gap: 7px; padding: 9px 12px; border-bottom: 1px solid var(--line); background: var(--surface-soft); color: var(--muted); }
  .diff-file header code { flex: 1; overflow: hidden; color: var(--ink-soft); font-family: var(--font-mono); font-size: var(--text-meta); text-overflow: ellipsis; white-space: nowrap; }
  .diff-file header span { font-size: var(--text-meta); }
  .diff-lines { overflow-x: auto; padding: 6px 0; }
  .diff-line { display: flex; min-width: max-content; padding: 1.5px 12px 1.5px 0; font-family: var(--font-mono); font-size: var(--text-meta); line-height: 1.6; }
  .diff-line code { white-space: pre; }
  .line-sign { flex: 0 0 24px; color: var(--diff-sign); text-align: center; }
  .diff-line[data-type="add"] { background: var(--diff-add-bg); color: var(--diff-add-ink); }
  .diff-line[data-type="add"] .line-sign { color: var(--diff-add-sign); }
  .diff-line[data-type="remove"] { background: var(--diff-remove-bg); color: var(--danger-ink); }
  .diff-line[data-type="remove"] .line-sign { color: var(--diff-remove-sign); }
  .diff-line[data-type="hunk"] { margin: 4px 0; background: var(--diff-hunk-bg); color: var(--diff-hunk-ink); }
  .diff-note { margin: 14px 2px 4px; color: var(--muted); font-size: var(--text-body); line-height: 1.6; }
  .diff-controls { display: flex; gap: 8px; margin-bottom: 12px; }
  .diff-controls select { flex: 1; min-width: 0; padding: 7px 9px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface); color: var(--ink-soft); font-size: var(--text-body); }
  .diff-error { margin-bottom: 12px; padding: 10px 12px; border: 1px solid var(--danger-line); border-radius: 8px; background: var(--danger-soft); color: var(--danger-ink); font-size: var(--text-body); line-height: 1.5; }
  .diff-revisions { display: grid; gap: 4px; margin: 0 0 12px; color: var(--muted); font-size: var(--text-body); }
  .diff-revisions code { overflow-wrap: anywhere; }
  .line-number { flex: 0 0 36px; padding-right: 7px; color: var(--muted); text-align: right; user-select: none; }
  .file-note { margin: 0; padding: 12px; color: var(--muted); font-size: var(--text-body); }
  .diff-line { font-size: var(--text-body); }
  .diff-file header code { font-size: var(--text-body); }
  @media (max-width: 480px) { .diff-tab { padding: 14px; } .diff-header { align-items: flex-start; flex-direction: column; gap: 8px; } .line-number { flex-basis: 28px; } }
</style>

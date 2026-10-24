<script lang="ts">
  import { FileDiff } from "@lucide/svelte";
  import { app } from "../lib/store";

  const ana = $derived($app.workers.find((worker) => worker.id === "ana"));
  const totals = $derived(
    $app.diffFiles.reduce(
      (sum, file) => ({
        files: sum.files + 1,
        additions: sum.additions + file.additions,
        deletions: sum.deletions + file.deletions,
      }),
      { files: 0, additions: 0, deletions: 0 },
    ),
  );
</script>

<section class="diff-tab" aria-label="Diff">
  <header class="diff-header">
    <div><p>LEAD REVIEW · REVIEWED BRANCH</p><h2>Diff</h2><span>{ana?.branch ?? "wt/agent-ana"} → {$app.projects.find((project) => project.id === $app.projectId)?.branch ?? "wt/mnemo"}</span></div>
    <div class="diff-totals"><strong>{totals.files} files</strong><span>+{totals.additions}</span><span class="deletions">−{totals.deletions}</span></div>
  </header>

  {#each $app.diffFiles as file (file.path)}
    <article class="diff-file">
      <header><FileDiff size={12} /><code>{file.path}</code><span class="additions">+{file.additions}</span><span class="deletions">−{file.deletions}</span></header>
      <div class="diff-lines">
        {#each file.lines as line, index (index)}
          <div class="diff-line" data-type={line.type}><span class="line-sign">{line.type === "add" ? "+" : line.type === "remove" ? "−" : line.type === "hunk" ? "@" : " "}</span><code>{line.text}</code></div>
        {/each}
      </div>
    </article>
  {/each}
  <p class="diff-note">A stand-in for the reviewed branch diff. Phase 1 renders the real worktree diff from the engine.</p>
</section>

<style>
  .diff-tab { padding: 18px; }
  .diff-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 16px; margin-bottom: 16px; }
  .diff-header p { margin: 0 0 4px; color: var(--muted); font-size: 7px; letter-spacing: 0.11em; }
  .diff-header h2 { margin: 0 0 3px; font-family: var(--font-prose); font-size: 26px; font-weight: 550; }
  .diff-header div > span { color: var(--muted); font-family: var(--font-mono); font-size: 8px; }
  .diff-totals { display: flex; gap: 8px; color: var(--muted); font-size: 8px; }
  .diff-totals strong { color: var(--ink-soft); font-weight: 600; }
  .additions { color: #4f7d48; }
  .deletions { color: #a15b53; }
  .diff-file { margin-bottom: 12px; overflow: hidden; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); }
  .diff-file > header { display: flex; align-items: center; gap: 7px; padding: 9px 12px; border-bottom: 1px solid var(--line); background: var(--surface-soft); color: var(--muted); }
  .diff-file header code { flex: 1; overflow: hidden; color: var(--ink-soft); font-family: var(--font-mono); font-size: 8px; text-overflow: ellipsis; white-space: nowrap; }
  .diff-file header span { font-size: 8px; }
  .diff-lines { overflow-x: auto; padding: 6px 0; }
  .diff-line { display: flex; min-width: max-content; padding: 1.5px 12px 1.5px 0; font-family: var(--font-mono); font-size: 8px; line-height: 1.6; }
  .diff-line code { white-space: pre; }
  .line-sign { flex: 0 0 24px; color: #b0b4a9; text-align: center; }
  .diff-line[data-type="add"] { background: #f1f7ee; color: #3f6f39; }
  .diff-line[data-type="add"] .line-sign { color: #5a9452; }
  .diff-line[data-type="remove"] { background: #fbf0ee; color: #8d4d46; }
  .diff-line[data-type="remove"] .line-sign { color: #b26a61; }
  .diff-line[data-type="hunk"] { margin: 4px 0; background: #f4f5f1; color: #7d8276; }
  .diff-note { margin: 14px 2px 4px; color: var(--muted); font-size: 8px; line-height: 1.5; }
</style>

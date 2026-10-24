<script lang="ts">
  import { ArrowUpRight, ClipboardCheck, MessageSquareText, ShieldCheck } from "@lucide/svelte";
  import { actions, app } from "../lib/store";
  import Composer from "./Composer.svelte";
  import Markdown from "./Markdown.svelte";
  import MergeCard from "./MergeCard.svelte";
  import RunStrip from "./RunStrip.svelte";
  import SpawnCard from "./SpawnCard.svelte";

  let scroller: HTMLDivElement;

  $effect(() => {
    const entryCount = $app.leadEntries.length;
    if (scroller && entryCount > 0) scroller.scrollTop = scroller.scrollHeight;
  });
</script>

<section class="lead-workspace" aria-label="Lead conversation">
  <div class="lead-scroll" bind:this={scroller}>
    <div class="lead-content">
      <header class="lead-heading">
        <div><p>PROJECT / CONVERSATION</p><h1>Lead</h1></div>
        <span><ShieldCheck size={13} /> Worker logs stay in worker threads</span>
      </header>

      <RunStrip />

      <div class="lead-entries">
        {#each $app.leadEntries as entry (entry.id)}
          {#if entry.kind === "message"}
            <article class:user-entry={entry.author === "user"} class="chat-entry">
              {#if entry.author === "lead"}
                <div class="entry-label"><span class="lead-mark">M</span> LEAD · ATLAS CORE <time>{entry.time}</time></div>
                <Markdown source={entry.body} onOpenLink={actions.openBrowser} />
              {:else}
                <p>{entry.body}</p>
                <div class="user-meta">YOU · {entry.time}</div>
              {/if}
            </article>
          {:else if entry.kind === "spawn"}
            <SpawnCard {entry} />
          {:else if entry.kind === "team-message"}
            {@const worker = $app.workers.find((item) => item.id === entry.workerId)}
            <article class="team-card">
              <div class="team-card-heading">
                <span class="team-card-icon"><MessageSquareText size={15} /></span>
                <div><strong>{worker?.name ?? "Worker"} sent an update</strong><span>TEAM MESSAGE · {entry.time}</span></div>
              </div>
              <p>{entry.body}</p>
              <button onclick={() => actions.selectWorker(entry.workerId)}>Open worker thread <ArrowUpRight size={13} /></button>
            </article>
          {:else if entry.kind === "review"}
            {@const worker = $app.workers.find((item) => item.id === entry.workerId)}
            <article class="team-card review-card">
              <div class="team-card-heading">
                <span class="team-card-icon"><ClipboardCheck size={15} /></span>
                <div><strong>{entry.title}</strong><span>LEAD REVIEW · {entry.time}</span></div>
              </div>
              <p>{entry.body}</p>
              <button onclick={() => actions.selectWorker(entry.workerId)}>Review {worker?.name ?? "worker"}’s delta <ArrowUpRight size={13} /></button>
            </article>
          {:else if entry.kind === "merge"}
            <MergeCard {entry} />
          {/if}
        {/each}
      </div>
    </div>
  </div>
  <footer class="composer-dock"><Composer /></footer>
</section>

<style>
  .lead-workspace { display: flex; min-height: 0; flex: 1; flex-direction: column; background: var(--canvas); }
  .lead-scroll { flex: 1; min-height: 0; overflow-y: auto; scrollbar-gutter: stable; }
  .lead-content { width: min(100%, 900px); margin: 0 auto; padding: 34px 40px 28px; }
  .lead-heading { display: flex; align-items: flex-end; justify-content: space-between; gap: 20px; margin-bottom: 24px; }
  .lead-heading p { margin: 0 0 4px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.12em; }
  .lead-heading h1 { margin: 0; font-family: var(--font-prose); font-size: 34px; font-weight: 500; letter-spacing: -0.03em; }
  .lead-heading > span { display: inline-flex; align-items: center; gap: 5px; padding-bottom: 5px; color: var(--muted); font-size: 9px; }
  .lead-entries { margin-top: 28px; }
  .chat-entry { margin: 0 0 26px; }
  .entry-label { display: flex; align-items: center; gap: 6px; margin-bottom: 9px; color: var(--muted); font-size: 8px; font-weight: 650; letter-spacing: 0.08em; }
  .entry-label time { margin-left: 2px; font-weight: 400; letter-spacing: 0; }
  .lead-mark { display: grid; width: 20px; height: 20px; place-items: center; border: 1px solid var(--line); border-radius: 6px; background: var(--surface); color: var(--accent-strong); font-family: var(--font-prose); font-size: 12px; }
  .user-entry { max-width: 620px; margin-left: auto; padding: 13px 16px 9px; border: 1px solid var(--line); border-radius: 16px 16px 4px 16px; background: var(--surface); }
  .user-entry p { margin: 0; color: var(--ink-soft); font-size: 12px; line-height: 1.6; white-space: pre-wrap; }
  .user-meta { margin-top: 6px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; text-align: right; }
  .team-card { margin: 18px 0 18px 34px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface-soft); }
  .team-card-heading { display: flex; align-items: center; gap: 10px; }
  .team-card-icon { display: grid; width: 29px; height: 29px; place-items: center; border-radius: 9px; background: var(--surface); color: var(--accent-strong); }
  .team-card-heading strong, .team-card-heading span { display: block; }
  .team-card-heading strong { font-size: 11px; font-weight: 650; }
  .team-card-heading div > span { margin-top: 3px; color: var(--muted); font-size: 7px; letter-spacing: 0.08em; }
  .team-card p { margin: 12px 0; color: var(--ink-soft); font-family: var(--font-prose); font-size: 14px; line-height: 1.55; }
  .team-card button { display: inline-flex; align-items: center; gap: 5px; padding: 0; border: 0; background: transparent; color: var(--accent-strong); cursor: pointer; font-size: 9px; font-weight: 650; }
  .review-card { background: #f4f2ed; }
  .composer-dock { flex: 0 0 auto; padding: 12px 40px 16px; background: linear-gradient(transparent, var(--canvas) 18%); }
  @media (max-width: 760px) {
    .lead-content { padding: 24px 20px; }
    .lead-heading > span { display: none; }
    .composer-dock { padding: 10px 16px 72px; }
  }
  @media (max-width: 560px) {
    .lead-heading h1 { font-size: 29px; }
    .team-card { margin-left: 0; }
  }
</style>

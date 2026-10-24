<script lang="ts">
  import { app, actions } from "./lib/store";
  import LeadChat from "./components/LeadChat.svelte";
  import StartScreen from "./components/StartScreen.svelte";
  import TopBar from "./components/TopBar.svelte";
</script>

<svelte:head>
  <title>Mnemo2 · Lead + Team</title>
</svelte:head>

{#if $app.route === "start"}
  <StartScreen />
{:else}
  <div class="workspace-shell">
    <TopBar />
    <div class="workspace-main">
      {#if $app.route === "lead"}
        <LeadChat />
      {:else}
        <section class="team-placeholder">
          <p>MISSION CONTROL</p>
          <h1>Ten workers, without the noise.</h1>
          <span>The full control room and worker review land in the next build step.</span>
          <button class="button button-quiet" onclick={actions.openLead}>Back to lead</button>
        </section>
      {/if}
    </div>
  </div>
{/if}

<style>
  .workspace-shell { display: flex; height: 100vh; min-height: 0; flex-direction: column; }
  .workspace-main { display: flex; flex: 1; min-height: 0; }
  .team-placeholder { display: grid; flex: 1; place-content: center; max-width: 640px; margin: 0 auto; padding: 40px; text-align: center; }
  .team-placeholder p { color: var(--muted); font-size: 9px; letter-spacing: 0.12em; }
  .team-placeholder h1 { margin: 8px 0 14px; font-family: var(--font-prose); font-size: 42px; font-weight: 500; }
  .team-placeholder span { margin-bottom: 22px; color: var(--muted); font-size: 12px; }
  .team-placeholder button { justify-self: center; }
</style>


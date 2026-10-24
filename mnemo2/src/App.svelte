<script lang="ts">
  import { app } from "./lib/store";
  import ControlRoom from "./components/ControlRoom.svelte";
  import LeadChat from "./components/LeadChat.svelte";
  import StartScreen from "./components/StartScreen.svelte";
  import TeamPeek from "./components/TeamPeek.svelte";
  import Toast from "./components/Toast.svelte";
  import TopBar from "./components/TopBar.svelte";
  import Workbench from "./components/Workbench.svelte";
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
        <div class="lead-column">
          <LeadChat />
          {#if $app.workbenchOpen}<Workbench />{/if}
        </div>
      {:else}
        <ControlRoom />
      {/if}
    </div>
    {#if $app.teamPeekOpen && $app.route === "lead"}<TeamPeek />{/if}
  </div>
{/if}
<Toast />

<style>
  .workspace-shell { display: flex; height: 100vh; height: 100dvh; min-height: 0; flex-direction: column; }
  .workspace-main { display: flex; flex: 1; min-height: 0; }
  .lead-column { position: relative; display: flex; flex: 1; min-width: 0; min-height: 0; }
</style>


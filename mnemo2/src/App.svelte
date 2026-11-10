<script lang="ts">
  import { onMount } from "svelte";
  import { actions, app } from "./lib/store";
  import ControlRoom from "./components/ControlRoom.svelte";
  import CostView from "./components/CostView.svelte";
  import FirstIndexOverlay from "./components/FirstIndexOverlay.svelte";
  import LayaLearningsSheet from "./components/LayaLearningsSheet.svelte";
  import LeadChat from "./components/LeadChat.svelte";
  import PlansView from "./components/PlansView.svelte";
  import StartScreen from "./components/StartScreen.svelte";
  import TeamPeek from "./components/TeamPeek.svelte";
  import Toast from "./components/Toast.svelte";
  import TopBar from "./components/TopBar.svelte";
  import WorkerPanel from "./components/WorkerPanel.svelte";
  import Workbench from "./components/Workbench.svelte";
  import WorkerDeleteSheet from "./components/WorkerDeleteSheet.svelte";

  // Claude Code drives Mnemo2 from outside: it can start the app on a project
  // (`--project`), ask it to open one, or ask for one of the project's workers
  // to be shown.
  onMount(() => {
    void actions.initExternalLead();
    void actions.initOpenWorker();
  });
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
      <div class="main-column">
        <div class="main-route">
          {#if $app.route === "lead"}
            <LeadChat />
          {:else if $app.route === "team"}
            <ControlRoom />
          {:else if $app.route === "cost"}
            <CostView />
          {:else}
            <PlansView />
          {/if}
        </div>
        {#if $app.workbenchOpen}<Workbench />{:else if $app.route === "lead" && $app.workerPanelId}<WorkerPanel />{/if}
      </div>
    </div>
    {#if $app.teamPeekOpen && $app.route === "lead"}<TeamPeek />{/if}
    <WorkerDeleteSheet />
    <LayaLearningsSheet />
    <FirstIndexOverlay />
  </div>
{/if}
<Toast />

<style>
  .workspace-shell { display: flex; height: 100vh; height: 100dvh; min-height: 0; flex-direction: column; }
  .workspace-main { display: flex; flex: 1; min-height: 0; }
  .main-column { position: relative; display: flex; flex: 1; min-width: 0; min-height: 0; }
  .main-route { display: flex; flex: 1; min-width: 0; min-height: 0; }
  @media (max-width: 760px) { .workspace-main { padding-bottom: 64px; } }
</style>


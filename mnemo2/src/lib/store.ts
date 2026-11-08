import { applyStoredTheme } from "./theme";
import type { ToastLevel } from "./types";
// The `./store/*` imports come last on purpose: `core` creates `app` and reads
// the sound toggles at import time, exactly where store.ts used to do it
// inline, and `browser` installs the native-pane visibility subscription.
import { app, createInitialState, withToast } from "./store/core";
import { discardPendingPreferences } from "./store/preferences";
import {
  browserBack,
  browserForward,
  browserOverlay,
  browserReload,
  connectBrowser,
  ensureBrowser,
  navigateBrowser,
  openBrowser,
  openExternalBrowser,
  setBrowserRect,
  setBrowserVisible,
} from "./store/browser";
import {
  closeScreenshot,
  closeWorkbench,
  disconnectWorkbench,
  openScreenshot,
  openWorkerDiff,
  openWorkbench,
  reclampBoardWidth,
  reclampWorkbenchWidth,
  refreshDiff,
  selectDiffWorker,
  setBoardWidth,
  setWorkbenchTab,
  setWorkbenchWidth,
} from "./store/workbench";
import {
  clearPlanDocument,
  clearTraceEntry,
  clearWorkbenchFile,
  closeWorkerPanel,
  openCost,
  openFileInWorkbench,
  openGraph,
  openLead,
  openMemory,
  openPlanDocument,
  openPlans,
  openRepoPath,
  openTeam,
  openTrace,
  openWorkerPanel,
  selectWorker,
  setTeamFilter,
  toggleTeamPeek,
} from "./store/navigation";
import { disconnectBoard, syncBoard } from "./store/board";
import { disconnectIndexing } from "./store/indexing";
import { disconnectTeam, syncTeam } from "./store/team";
import {
  addLeadDraftImage,
  answerLead,
  approveLead,
  bumpLeadSession,
  clearLeadDraft,
  compactLead,
  interruptLead,
  removeLeadDraftImage,
  retryLead,
  sendLeadMessage,
  setLeadDraftText,
  setLeadModel,
  stopNativeLead,
} from "./store/lead";
import {
  cancelDeleteWorkers,
  compactWorker,
  confirmDeleteWorkers,
  fixFinding,
  mergeWorker,
  pauseWorker,
  recordWorkerCheck,
  replyWorker,
  requestDeleteOldWorkers,
  requestDeleteWorkers,
  resolveFinding,
  resumeWorker,
  revertMerge,
  reviewWorker,
  setWorkerModel,
  setWorkerSlots,
  spawnFixer,
  stopAll,
  stopWorker,
  wontfixFinding,
} from "./store/teamActions";
import { disconnectExternalLead, disconnectOpenWorker, initOpenWorker } from "./store/externalLead";
import {
  cancelCleanup,
  cancelCloseProject,
  cancelNewProject,
  cancelProjectConflict,
  closeProject,
  confirmCleanup,
  confirmCloseProject,
  confirmProjectConflict,
  createProject,
  initExternalLead,
  loadProjects,
  openFolder,
  openNewProject,
  openProject,
  requestCleanup,
  reviewProject,
} from "./store/project";

export { SESSION_START_MS, app, createInitialState, currentProject, needsYouCount, selectedWorker } from "./store/core";
export { MIN_LEAD_WIDTH, MIN_WORKBENCH_WIDTH, maxWorkbenchWidth, clampWorkbenchWidth, MIN_BOARD_WIDTH, MIN_BOARD_OUTPUT_WIDTH, BOARD_DIVIDER_WIDTH, maxBoardWidth, clampBoardWidth, defaultBoardWidth } from "./store/layout";
export { OLD_WORKER_STATUSES, isOldWorker } from "./store/teamActions";

/** Surface a transient status or failure message to the user. */
function showToast(message: string, level: ToastLevel = "info"): void {
  app.update((state) => withToast(state, message, level));
}

function dismissToast() {
  app.update((state) => ({ ...state, toast: null }));
}

function resetDemo() {
  discardPendingPreferences();
  bumpLeadSession();
  void stopNativeLead();
  void disconnectTeam();
  void disconnectBoard();
  disconnectIndexing();
  disconnectExternalLead();
  disconnectOpenWorker();
  void disconnectWorkbench();
  app.set(createInitialState());
}

export const actions = {
  openProject,
  loadProjects,
  initExternalLead,
  initOpenWorker,
  openFolder,
  closeProject,
  reviewProject,
  cancelCloseProject,
  confirmCloseProject,
  cancelProjectConflict,
  confirmProjectConflict,
  requestCleanup,
  cancelCleanup,
  confirmCleanup,
  openNewProject,
  createProject,
  cancelNewProject,
  openLead,
  openTeam,
  openCost,
  openGraph,
  openPlans,
  openMemory,
  openTrace,
  clearTraceEntry,
  openPlanDocument,
  clearPlanDocument,
  openFileInWorkbench,
  clearWorkbenchFile,
  openRepoPath,
  toggleTeamPeek,
  selectWorker,
  setTeamFilter,
  openWorkerPanel,
  closeWorkerPanel,
  openWorkbench,
  closeWorkbench,
  setWorkbenchTab,
  setWorkbenchWidth,
  reclampWorkbenchWidth,
  refreshDiff,
  selectDiffWorker,
  openWorkerDiff,
  openBrowser,
  navigateBrowser,
  connectBrowser,
  ensureBrowser,
  setBrowserRect,
  setBrowserVisible,
  browserOverlay,
  browserBack,
  browserForward,
  browserReload,
  openExternalBrowser,
  openScreenshot,
  closeScreenshot,
  sendLeadMessage,
  setLeadDraftText,
  addLeadDraftImage,
  removeLeadDraftImage,
  clearLeadDraft,
  showToast,
  interruptLead,
  compactLead,
  approveLead,
  answerLead,
  setLeadModel,
  retryLead,
  replyWorker,
  stopWorker,
  pauseWorker,
  requestDeleteWorkers,
  requestDeleteOldWorkers,
  cancelDeleteWorkers,
  confirmDeleteWorkers,
  resumeWorker,
  setWorkerModel,
  compactWorker,
  syncTeam,
  syncBoard,
  reviewWorker,
  recordWorkerCheck,
  resolveFinding,
  fixFinding,
  spawnFixer,
  wontfixFinding,
  mergeWorker,
  revertMerge,
  stopAll,
  setWorkerSlots,
  dismissToast,
  resetDemo,
  /** Read the stored `[ui] theme` and paint it (see `lib/theme.ts`). */
  initTheme: applyStoredTheme,
  setBoardWidth,
  reclampBoardWidth,
};

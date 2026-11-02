+++
title = "Mnemo2 preview scenes feed transport-driven views through a dev-only seed seam in their lib module"
created = "2026-11-02"
+++

Preview scenes (mnemo2/src/test/previewScenes.ts) only build AppState, so views that create their own Tauri transport inside the component (SettingsDialog pages, CostView, GraphView, LayaPage) cannot get scene data through the store. Pattern: the view's lib module holds a dev-only module-level seam (e.g. lib/cost.ts seedCostPreview/costPreviewInsights; lib/modelSettings.ts and lib/layaSettings.ts setPreview*Transport), and createXxxTransport() returns the preview transport/seed only when installed and the Tauri runtime is absent, so the real app always uses IPC. Only src/test/* imports the setters (test→production direction); production code must never import ../test/*, which `npm run check:no-mock` enforces. Component tests stub the transport with vi.mock("../lib/x", importOriginal spread overriding createXxxTransport); polling views test with vi.useFakeTimers + advanceTimersByTimeAsync.

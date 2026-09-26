+++
title = "Laya classifier foundation — opt-in Classifier trait + LayaClassifier + status + Settings (commit 9a8b39c)"
created = "2027-01-11"
status = "superseded"
+++

Shipped in commit 9a8b39c on branch wt/mnemo (NOT yet merged to main; plan 9295aa31, backlog bb54bdcc). The opt-in Laya "System 1" classifier foundation — item 1 of the 5-item Laya chain.

SURFACE
- src/memory/classifier.rs: `Classifier` trait (async, Send+Sync; `classify(state, question) -> Option<Answer>`, infallible — None = "no answer", mirroring Embedder); typed `Question` choice/score/noul serializing to the laya-serve wire shape (`{"type","instructions","criteria"}`); `Answer` {Choice{label,confidence,probabilities}, Score{value,confidence}, NoUl{probability}}; `ClassifierStatus` (lowercase serde: disabled/ready/failed); `NoClassifier` (explicit no-op — the app NEVER builds it; tests only); `LayaClassifier` (reqwest `POST {endpoint}/v1/systemone`, 10 s timeout, every transport/protocol failure ⇒ `None` + status Failed, no retries/panics). Wire mapping lives in one place: `systemone_request` + `parse_answer` (request `{"state", "questions":{"question": …}}`; response `{"answers":{"question":{"choice"|"score"|"noul", "confidence", "probabilities"}}}`; usage/routing ignored). Shape pinned from laya's serve.py (github.com/NandhaKishorM/laya) + the HF model card.
- src/provider/client_factory.rs `build_classifier(config, status)`: enabled + non-blank endpoint ⇒ `Some(LayaClassifier)` + status Ready; disabled / absent / blank endpoint ⇒ `None` + Disabled and NO HTTP client built (the hard requirement: zero calls, zero cost, zero new failure modes). Unbuildable client ⇒ None + Failed (logged).
- Config: `[general.laya] { enabled = false, endpoint: Option<String> }`; OMITTED from config.toml while both fields are defaults (`skip_serializing_if = "LayaConfig::is_default"`); save patch keys `laya_enabled` / `laya_endpoint` (absent keeps, blank clears, trimmed).
- IPC/runtime: `IpcState.classifier_status` (shared Arc) + `classifier` slot (Arc<RwLock<Option<Arc<dyn Classifier>>>>); command `get_classifier_status`; startup snapshot field `classifier_status`; event `classifier://status` emitted at startup AND on every settings-save rewire (rewire_vision_embedder_and_classifier takes `app: &tauri::AppHandle`; save_settings gained an AppHandle param). Live calls flipping ready⇄failed do NOT emit (consumers re-poll).

FRONTEND: Settings → Classifier (frontend/src/components/settings/sections/ClassifierSection.tsx — enable toggle, endpoint URL, live status line, install hints `pip install "laya[serve]"` + `laya-serve`, dirty/save contract) + bindings `getClassifierStatus`/`onClassifierStatus` (frontend/src/lib/tauri.ts) + source-contract test.

CONTRACT FOR ITEMS 2-5 (091e694d model routing, e2c47d5f tool steering, 1a4049c1 failure triage, a147b63c memory typing): read the handle via `IpcState::classifier()`; treat None as "no classifier" and fall back to the pre-classifier behavior; never gate a real decision on an unvalidated probability (base checkpoints are near-chance until fine-tuned; confidence-gate every consumer).

TESTS: src/memory/classifier.rs stub-server suite (one-shot tokio TcpListener: choice round-trip pins POST + path + full body; malformed/500/timeout ⇒ None + Failed; disabled path opens no connection); build_classifier gate pins (disabled ⇒ None + Disabled); config laya default/round-trip/omission tests; frontend ClassifierSection.test.ts. Review: round 1 = 4 low findings (all fixed), round 2 = PASS (.coding/reviews/2026-09-24-laya-foundation-round2-verification.md). Docs synced: README.md, docs/FEATURES.md, docs/CONFIGURATION.md, PLAN.md.

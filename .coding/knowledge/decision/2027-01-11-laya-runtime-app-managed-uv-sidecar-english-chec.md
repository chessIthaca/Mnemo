+++
title = "Laya runtime = app-managed uv sidecar, English checkpoint only (external-endpoint mode removed)"
supersedes = "2027-01-11-laya-managed-uv-sidecar-not-in-process-for-embed"
created = "2027-01-11"
+++

DECISION (plan 2f74e10a, 2027-02, wt/mnemo): the managed-sidecar architecture of d6fc659a STANDS (uv-downloaded venv + `laya[serve]` + the checkpoint under <global_config_dir>/laya/, app-started loopback laya-serve, no in-process port), but its "External-endpoint mode stays as an advanced option" clause is REVERSED: the external-endpoint mode and the checkpoint choice are DELETED. Laya is managed-only and serves the English checkpoint — one supported path, no `mode`/`endpoint`/`checkpoint` config, no mode radios or endpoint field in Settings → Classifier, and no external branch in the startup hook or the Settings rewire. Rationale: the app owns the runtime end-to-end (download, start, monitor, stop), so the user-run `laya-serve` variant only duplicated the surface and could back-door a classifier the next save would drop. Landed files: src/config/general.rs, src/config/settings_dto.rs, src/provider/client_factory.rs, src-tauri/src/{main.rs, ipc/laya.rs, ipc/rewire.rs, ipc/finetune.rs, ipc/settings.rs, ipc/contract_fixtures.rs}, frontend/src/lib/tauri.ts, frontend/src/components/settings/{types.ts, sections/ClassifierSection.tsx}.

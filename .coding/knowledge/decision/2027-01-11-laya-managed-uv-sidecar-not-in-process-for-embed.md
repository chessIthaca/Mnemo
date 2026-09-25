+++
title = "Laya managed uv sidecar (not in-process) for embedding-parity UX"
created = "2027-01-11"
+++

DECISION (plan d6fc659a): the Laya classifier gets a MANAGED SIDECAR runtime, not an in-process port. Mnemo downloads uv (single-file binary, GitHub releases) on demand into <global_config_dir>/laya/bin, creates <config>/laya/venv via `uv venv --python 3.12` (uv fetches standalone CPython), `uv pip install "laya[serve]"`, and pre-downloads the checkpoint via `laya.load` with HF_HOME=<config>/laya/hf (progress = dir-size sampling, the embeddings trick). When enabled, Mnemo spawns the venv's laya-serve on 127.0.0.1:<free port> (LAYA_HOST/PORT/MODELS/PRELOAD/THREADS env), probes POST /v1/systemone with empty questions (returns 200 answers:{} — documented readiness probe), and kills the child on app exit / disable / reconfigure.

Why not in-process ONNX: Laya's scoring head (option [MASK] markers, head-budget splitting, per-(type,option-count) temperature calibration, router language detection) ships no ONNX export; a Rust port risks silent miscalibration. torch/transformers are hard deps of the laya package, so Python must be hosted either way — uv owns it with zero user prerequisites.

Facts pinned (HF convaiinnovations/laya + github.com/NandhaKishorM/laya, 2027-01-11): english checkpoint at repo root ~808 MB, multilingual subfolder ~647 MB; laya-serve serves POST /v1/systemone; env config LAYA_HOST, LAYA_PORT, LAYA_PRELOAD, LAYA_MODELS, LAYA_THREADS, LAYA_DEVICE, LAYA_API_KEY; LAYA_MODELS=english limits preload to one checkpoint. External-endpoint mode stays as an advanced option. Foundation gate preserved: absent/disabled [general.laya] ⇒ no client, no server, no downloads.

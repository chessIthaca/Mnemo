+++
title = "Mnemo2 attaches Laya once per engine through Setup::attach_laya"
created = "2026-11-01"
+++

Laya (the local classifier for routing lanes, memory auto-typing, failure triage and tool-choice reflexes) is wired into every engine by wire_brain (src/host/brain.rs). Only the classifier slot must be filled. Mnemo2 fills it once per engine in LeadRuntime::start_engine through a headless Setup::attach_laya(). Workers run as sessions on the lead's engine, so they inherit it with no worker-side code. Mnemo2 reuses Mnemo1's managed laya-serve runtime under <config_dir>/laya. LeadRuntime holds the LayaManager (dropping it kills the child) and stops it on shutdown. Attaching never blocks engine start. With Laya disabled, not installed or down, behaviour is byte-identical to no Laya. Mnemo1 and Mnemo2 running together each start their own sidecar against one shared install. The full design is .coding/design/mnemo2-laya.md.

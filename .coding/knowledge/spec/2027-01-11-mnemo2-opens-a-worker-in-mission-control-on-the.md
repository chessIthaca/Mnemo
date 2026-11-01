+++
title = "Mnemo2 opens a worker in mission control on the mnemo2://open-worker event"
created = "2027-01-11"
+++

The Mnemo2 frontend listens for the Tauri event `mnemo2://open-worker` with payload {root, worker_id} (mnemo2/src/lib/workerSelection.ts OPEN_WORKER_CHANNEL; store.ts initOpenWorker, started from App.svelte onMount behind probeMnemo2Host and a token guard like the external-lead listeners). When root is the open project (compared with sameProjectPath) and the worker is known, it calls selectWorker, exactly as a row click does, so mission control shows that worker. A foreign root, unknown worker or no open project keeps the current view and shows a short toast (openWorkerNotice). The backend emits the event from the helper's show-worker path, so external tools such as the Claude Desktop worker band can open a worker without attaching as lead.

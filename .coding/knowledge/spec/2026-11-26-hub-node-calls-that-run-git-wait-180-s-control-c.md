+++
title = "hub→node calls that run git wait 180 s; control calls 30 s"
created = "2026-11-26"
evidence = "git_node_calls_use_the_long_timeout in team/remote.rs; loaded check runs on 2026-11-26 failed with 'no answer to \"repo.push_end\" within 30s' before the change"
+++

team/remote.rs node_call_timeout(method) chooses the timeout of every hub→node call. REPO_PUSH_END (fetch an uploaded bundle into the node's bare repo), CHECKOUT_PREPARE (create or reset a checkout) and REPO_FETCH_BRANCH (write a bundle of the worker branch) get GIT_CALL_TIMEOUT = hub LONG_TIMEOUT (180 s), because git on a large repository or a node busy with builds regularly exceeds 30 s. Everything else (REPO_HAVE, push begin/chunk, REPO_TIP, CHECKOUT_RELEASE, CLAUDE_*, NODE_STATUS) keeps CALL_TIMEOUT (30 s). A new node call that runs git must be added to the long list.

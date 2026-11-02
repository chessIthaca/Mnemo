+++
title = "unknown spawn roles became permissive Skill filters"
created = "2027-01-11"
+++

Root cause: src/host/runtime.rs applied set_role_allowlist only to reviewer; every other role used set_tool_allowlist, which yields ToolFilter::Skill. Skill(vec![]) is not deny-all because it auto-grants memory and several mutating Agent/AutoRun tools. Fix: spawn_agent_shared rejects every role except None or reviewer before id allocation or agent construction. The shared-path regression test is parentless_unknown_role_is_rejected_before_registration in src/host/runtime.rs.

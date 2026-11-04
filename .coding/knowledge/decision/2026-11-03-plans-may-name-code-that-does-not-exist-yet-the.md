+++
title = "plans may name code that does not exist yet; the evidence gate is gone"
created = "2026-11-03"
+++

The plan board and create_plan no longer refuse a context line that names a member call or says something "is callable/reachable" without a "verified:"/"assumption:" label. The user removed that rule (2026-11-02) because it pushed the lead into vague designs, while cheap worker models need exact names and signatures. Plans now describe new code directly and mark it as new. The remaining resumability checks stay: a minimum context length, a file path (or no-code marker) per step, and a regression-test design for bug fixes. context_issue(context, kind, detailed_steps) in src/app/tool/workflow/plan.rs is shared by the board and Mnemo1's create_plan.

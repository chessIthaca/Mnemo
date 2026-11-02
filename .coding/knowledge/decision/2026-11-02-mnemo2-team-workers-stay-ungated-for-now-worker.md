+++
title = "Mnemo2 team workers stay ungated for now; worker gating is future work"
created = "2026-11-02"
+++

The user decided on 2026-11-02 to keep Mnemo2 team workers ungated (spawned with PermissionPolicy::OverridePermissions, so the Safety mode and project safety rules apply only to the lead; worker actions that would ask are logged as bypassed approvals). Worker gating is a later piece of work. The suggested shape: the same mode and rules, with a pending approval pausing the worker as "needs you" in mission control, or a middle ground that gates only shell commands or actions outside the worker's own worktree. Why ungated today: workers run unattended in parallel in their own worktrees, and the lead's review before merge is the gate for what reaches the lead branch.

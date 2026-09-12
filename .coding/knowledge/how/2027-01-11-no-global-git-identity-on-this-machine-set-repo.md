+++
title = "no global git identity on this machine — set repo-local user.name/user.email before committing"
created = "2027-01-11"
+++

Symptom: `git commit` fails with "Author identity unknown / fatal: unable to auto-detect email address (got 'carsten@CoolAndTheGang.(none)')" — this machine has no GLOBAL git identity, so every commit in a repo without a user.name/user.email fails. Observed 2026-09-12 on wt/mnemo (plan 2c19aca0, first commit attempt exit 128).
Fix (repo-local, NOT --global — do not touch machine-global config):
  git config user.name "Carsten Hess"; git config user.email "carsten@hess.net"
This matches the repo's existing commit author (git log -1 --format="%an <%ae>" on 3217714 "Initial commit" = Carsten Hess <carsten@hess.net>). Set it in ~/.git/config for the Mnemo repo once — after that, commits on any wt/* branch in this repo work. Check before committing in a fresh clone/other repo: `git config user.email` empty → set it before the commit, or the whole closing sequence stalls at the last step. Also useful: `git config --local --list` reveals remote.origin.url — for this repo it is https://github.com/chessIthaca/Mnemo.git (search tools skip .git/config, so read it from git config, not from file search).

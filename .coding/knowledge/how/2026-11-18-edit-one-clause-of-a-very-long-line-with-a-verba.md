+++
title = "edit one clause of a very long line with a verbatim old_string, never a whole-line from_read cite"
created = "2026-11-18"
evidence = "2026-11-18: missExcerpt1 replaced docs/FEATURES.md:41 with a fragment (caught by delta audit excerptDelta1, fixed in ac6e7400); argShape1, briefed to use verbatim anchors, then edited the same line cleanly (word-diff verified)."
+++

A from_read cite replaces the WHOLE cited line with new_string. On a very long single-line paragraph (docs/FEATURES.md's bullets run thousands of chars), a worker who means to change one clause and sends only that clause as new_string silently deletes the rest of the line. The edit "succeeds", so the edit-outcome metric cannot see it. To change part of a long line, anchor on the exact phrase with a verbatim old_string and check afterwards with `git diff --word-diff` that only that phrase changed. In briefs that touch such files, say so explicitly.

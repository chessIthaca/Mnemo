+++
title = "Clear directory-path error for file tools (os error 5 misdirection)"
created = "2026-08-23"
+++

symptom: context_pack-adjacent tool card shows: failed to read '.coding/plans': Access is denied. (os error 5) — actually file_read on a directory path; Windows read_to_string(dir) = ERROR_ACCESS_DENIED, no is_dir pre-check, misleading permission error (backlog #89)
regression test: reading_a_directory_path_returns_directory_hint · path .coding/plans/7be09c11-6fd2-4ef8-a93c-c4dcb0ccb4f9.md · branch fix/fileread-dir-os-error5 @ 20a8843 (unmerged — exists only on this branch)

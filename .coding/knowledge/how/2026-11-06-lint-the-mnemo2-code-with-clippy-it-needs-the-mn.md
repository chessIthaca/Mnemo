+++
title = "lint the Mnemo2 code with clippy: it needs the mnemo2 feature"
created = "2026-11-06"
+++

`cargo clippy --workspace --all-targets -- -D warnings` may not lint src-tauri/src/mnemo2/ because `mnemo2` is not a default feature of mnemo-app. To lint that tree run `cargo clippy -p mnemo-app --features mnemo2 --all-targets -- -D warnings`, in addition to the workspace command.

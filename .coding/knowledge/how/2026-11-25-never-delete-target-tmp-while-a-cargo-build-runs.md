+++
title = "never delete target\\.tmp* while a cargo build runs in that checkout"
created = "2026-11-25"
evidence = "2026-11-25 integration check: clippy failed in 2.5 s right after a cleanup deleted D:\\Mnemo\\target\\.tmp*; an immediate rerun passed."
+++

.cargo/config.toml [env] forces TMP/TEMP/TMPDIR into <checkout>/target, so rustc's live temp folders are target\.tmpXXXX. Deleting them during a build makes every crate fail with "could not compile `cfg-if`" and similar errors within seconds. When freeing disk space, delete target\.tmp* only when no cargo or rustc process runs in that checkout; build caches (target\debug, pool targets) of idle checkouts are the real space and are safe to delete.

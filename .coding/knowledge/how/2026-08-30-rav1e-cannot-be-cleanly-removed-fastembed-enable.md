+++
title = "rav1e cannot be cleanly removed — fastembed enables image's default (avif) feature"
created = "2026-08-30"
+++

HOW (2026-09-19, plan 90dfade8 step 1): rav1e (AV1 encoder, ~5s cold-compile) CANNOT be cleanly removed. Root cause: fastembed v4.9.1 declares `[dependencies.image] version = "0.25.2"` WITHOUT `default-features = false`, so it enables image's `default` feature → `default-formats` → `avif` → `ravif` → `rav1e`. Cargo feature-unification only ADDS features (never subtracts), so our `image = { default-features = false, features = ["png","jpeg","gif","webp","bmp"] }` in Cargo.toml is overridden by fastembed's enablement. image is a non-optional dep of fastembed (not behind a feature gate), so `default-features = false` on fastembed wouldn't help either. The only fixes would be: (a) `[patch.crates-io]` image → a fork with avif removed from default-formats (maintenance burden), or (b) wait for fastembed to add `default-features = false` on its image dep. Neither is clean — skipped per the plan's escape clause. The 5s cold-compile cost is accepted as inherent to using fastembed.

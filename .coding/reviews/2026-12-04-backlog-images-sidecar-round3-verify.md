## Verdict: PASS

Round-3 (final) verification of the single round-2 finding **LOW 2** (`resolve_images` path-traversal guard was bypassable) on `wt/agenticcoding` @ `eba994c`. **LOW 2 is fully resolved.** The `ParentDir`-component check fires before `starts_with` and before `std::fs::read`, closing the exfiltration surface; the 3 new regression tests genuinely exercise all three LOW 2 guards and fail without them. No new issues introduced.

---

### LOW 2 — `resolve_images` path-traversal guard — RESOLVED ✓

`src/backlog.rs:497-516` now reads:

```rust
let file = parent.join(p);
// …doc comment explaining starts_with doesn't resolve `..`…
if file
    .components()
    .any(|c| c == std::path::Component::ParentDir)
{
    return None;
}
if !file.starts_with(&image_root) {
    return None;
}
let bytes = std::fs::read(&file).ok()?;
```

**Correctness of the fix.** For a tampered path `backlog-images/../../decoy.txt`, `parent.join(p)` yields components `[Normal(<parent>), Normal("backlog-images"), ParentDir, ParentDir, Normal("decoy.txt")]`. The `.any(|c| c == Component::ParentDir)` check matches → `None` (skipped, **never read**). The check is placed **before** both `starts_with` and `std::fs::read`, so the bypass round-2 identified (where `starts_with` returns `true` because the leading components match `image_root`) can no longer reach the file read. The `starts_with` check is retained as a second layer for absolute-path escapes (e.g. `/etc/passwd` or `C:\...`), which carry no `ParentDir` but won't prefix-match `image_root`. `Component` implements `PartialEq`, so the comparison is sound on both Unix and Windows; backslash `..` and `./../` variants also resolve to a `ParentDir` component and are caught.

**No false rejections.** A legitimate sidecar path is always `backlog-images/<id>/<i>.<ext>` — `parent.join` of it has only `Normal` components, no `ParentDir`, so it passes. The pre-existing `add_with_data_url_stores_path_and_resolves_back` and `legacy_inline_data_url_migrated_on_open` tests still resolve round-trip, confirming legitimate reads are unaffected.

### Regression tests pin the guards ✓

All three are in `src/backlog.rs` (test module, lines 1329-1388) and each **fails without its guard**:

- **`write_image_files_rejects_unsafe_item_id`** — `item_id = "../escape"`: `is_safe_item_id` is false → returns images inline, no file/dir. Without the guard, `create_dir_all(image_dir().join("../escape"))` resolves `..` and creates `backlog-images/`, failing the `!dir.0.join("backlog-images").exists()` assertion. ✓
- **`delete_item_images_is_noop_for_unsafe_id`** — `item_id = "../decoy"`: `is_safe_item_id` false → early return; the decoy dir + `secret.txt` survive. Without the guard, `remove_dir_all(image_dir().join("../decoy"))` = `remove_dir_all(<parent>/decoy)` deletes the decoy → assertion fails. ✓
- **`resolve_images_rejects_traversal_path`** — `images = ["backlog-images/../../decoy.txt"]` with a real `decoy.txt` placed where the traversal lands: the `ParentDir` check returns `None` → `resolved.is_empty()`. Without the `ParentDir` check, `starts_with(image_root)` is `true` and `fs::read` exfiltrates `decoy.txt` as a data URL → `resolved` non-empty → assertion fails. ✓

### No new issues

- The change is strictly **more restrictive** on the read path only; legitimate paths still resolve (existing round-trip tests pass). No caller of `resolve_images` is affected.
- `write_image_files` / `delete_item_images` were already correct via `is_safe_item_id` (round-2 confirmed) and are unchanged here.
- Test count is consistent: round-2's 1693 + 3 new = 1696 passed, 0 warnings under `#![deny(warnings)]` (per the main agent's `cargo test --lib`).
- Multi-platform neutral: pure `std::path::Component` / `std::fs`; the `ParentDir` comparison is platform-agnostic. No Windows-only assumptions.
- Docs: the in-code comment at `src/backlog.rs:502-507` now accurately explains *why* the explicit `ParentDir` check is required (round-2 flagged the prior comment as misleading) — the comment and the behavior now agree.

### Out of scope (noted, not a finding)

The commit also flips backlog item `a39830cf` to `failed` ("plan loop did not close") and the commit message summarizes round-2 as "LOW 2 resolved" (round-2 actually said *partially* resolved). Both are bookkeeping/wording, not code defects.

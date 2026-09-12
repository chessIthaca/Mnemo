// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Back-compat re-export — Settings lives under `components/settings/`.
 * Prefer importing `SettingsDialog` from `../settings` in new code.
 */
export { SettingsDialog as ConfigDialog } from "../settings";
export type { SettingsDialogProps as ConfigDialogProps } from "../settings";

// SPDX-License-Identifier: MIT
/**
 * Shared types for the Settings feature.
 *
 * Lives in `lib/` (not `components/`) so consumers — including the
 * `openAppSettings` helper — can reference it without importing from
 * the settings panel component tree.
 */
export type SectionKey =
  | "general"
  | "appearance"
  | "dashboards"
  | "identity"
  | "shortcuts"
  | "account"
  | "workspace"
  | "repo"
  | "phases"
  | "projection-excludes";

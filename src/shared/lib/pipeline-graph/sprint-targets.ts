// SPDX-License-Identifier: MIT
import type { SprintRegistry } from "@/tauri/commands";

/** One concrete sprint a `sprint:` write can target (SW-SB-15). */
export interface SprintTargetOption {
  /** The concrete sprint id written to `sprint:`. */
  id: string;
  /** Display label — the active sprint is annotated so the quick-pick is obvious. */
  label: string;
  /** True for the registry's active sprint (the default one-click add target). */
  active: boolean;
}

/**
 * Build the concrete sprints a `sprint:` write can point at (SW-SB-15), ordered
 * active → planning → closed (newest-closed first) so the everyday pick sits up
 * top. Returns `[]` for a still-loading / empty registry.
 */
export function buildSprintTargetOptions(registry: SprintRegistry | null): SprintTargetOption[] {
  const active = registry?.activeSprint ?? null;
  const sprints = registry?.sprints ?? [];
  const opt = (id: string): SprintTargetOption => ({
    id,
    label: id === active ? `${id} (active)` : id,
    active: id === active,
  });
  const activeOpt = sprints.filter((s) => s.id === active).map((s) => opt(s.id));
  const planning = sprints
    .filter((s) => s.state === "planning" && s.id !== active)
    .map((s) => opt(s.id));
  const closed = sprints
    .filter((s) => s.state === "closed" && s.id !== active)
    .map((s) => opt(s.id))
    .reverse();
  return [...activeOpt, ...planning, ...closed];
}

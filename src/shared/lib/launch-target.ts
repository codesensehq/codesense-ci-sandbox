// SPDX-License-Identifier: MIT
/**
 * Where a launched agent works, and the visible progress of getting it there. Shared by
 * every "launch this agent somewhere" dialog: a launch must pick a target (an existing
 * story, a repo branch, a new worktree or an existing worktree); no target, no launch.
 *
 * Pure and DI-only: the launch mechanism is injected ({@link LaunchOpen}), so the progress
 * state machine unit-tests without Tauri, a store or the launch controller.
 */

import type { OpenWorkspaceArg } from "@/features/dashboard/components/dashboard-launch";
import type { AgentLaunchWarning } from "@/tauri/commands";

/** The visible launch progress: `hydrating` (the worktree is opening) → `starting` (the
 *  session is spawning) → `running`. `error` surfaces a failure, never a silent hang. */
export type LaunchPhase = "idle" | "hydrating" | "starting" | "running" | "error";

/** The four launch targets. `repoBranch` / `newWorktree` both check a branch out into a
 *  managed worktree; `existingWorktree` opens the agent in a worktree that already exists;
 *  `story` launches on a managed story (the worktree comes from the story's frontmatter). */
export type LaunchTarget =
  | { kind: "story"; storyId: string }
  | { kind: "repoBranch"; repo: string; branch: string }
  | { kind: "newWorktree"; repo: string; branch: string }
  | { kind: "existingWorktree"; path: string };

/** The kind discriminant alone (for the target picker). */
export type LaunchTargetKind = LaunchTarget["kind"];

/** Is a target fully specified? A Launch control stays disabled until it is. */
export function isLaunchable(target: LaunchTarget | null): target is LaunchTarget {
  if (!target) return false;
  const has = (s: string) => s.trim().length > 0;
  switch (target.kind) {
    case "story":
      return has(target.storyId);
    case "repoBranch":
    case "newWorktree":
      return has(target.repo) && has(target.branch);
    case "existingWorktree":
      return has(target.path);
  }
}

/** What a launch returned. */
export interface LaunchOpenResult {
  sessionId?: string;
  promptDelivered?: boolean;
  reconcileStatus?: "reconciled" | "skipped";
  /** A named agent launch's warnings; they never block it. */
  warnings?: AgentLaunchWarning[];
}

/** Hooks the launch mechanism calls back, so progress can move from `hydrating` to
 *  `starting` the moment the worktree is ready. */
export interface LaunchOpenHooks {
  onHydrated: () => void;
}

/** The injected launch mechanism: production drives `dispatchOpenWorkspace`; tests fake it. */
export type LaunchOpen = (
  arg: OpenWorkspaceArg,
  hooks: LaunchOpenHooks,
) => Promise<LaunchOpenResult>;

/** Drive the progress over `open`: `hydrating` → (`onHydrated`) `starting` → `running`, or
 *  `error` on any failure (rethrown so the caller shows the message). */
export async function runLaunchWithProgress(
  arg: OpenWorkspaceArg,
  onPhase: (phase: LaunchPhase) => void,
  open: LaunchOpen,
): Promise<LaunchOpenResult> {
  onPhase("hydrating");
  try {
    const result = await open(arg, { onHydrated: () => onPhase("starting") });
    onPhase("running");
    return result;
  } catch (err) {
    onPhase("error");
    throw err;
  }
}

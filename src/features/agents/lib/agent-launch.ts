// SPDX-License-Identifier: MIT
import type { OpenWorkspaceArg } from "@/features/dashboard/components/dashboard-launch";
import type { LaunchTarget } from "@/shared/lib/launch-target";

/** The launcher stamped on every session the Agents tab starts, shown in the agents list. */
export const AGENTS_LAUNCHER = "virtual://agents";

/**
 * A chosen target, the agent and the (edited) prompt as the shipped open-workspace arg. The
 * backend prepares the agent for the session it spawns: its runtime, revision, permissions
 * and this user's account. A named agent launch runs attended, in a terminal.
 *
 * - `story` → the story open, seeded with the prompt.
 * - `repoBranch` → the branch open of an existing branch in a managed worktree;
 *   `newWorktree` → the same open, creating the branch from the repo's local default
 *   branch. Neither fetches; the prompt is submitted once the agent is live.
 * - `existingWorktree` → the dir open, which always starts the agent there.
 *
 * A blank prompt is no prompt: the backend then starts the agent with its own default.
 */
export function toAgentOpenWorkspaceArg(
  target: LaunchTarget,
  agent: string,
  prompt: string,
): OpenWorkspaceArg {
  const seed = prompt.trim() ? prompt : undefined;
  switch (target.kind) {
    case "story":
      return {
        target: { kind: "story", storyId: target.storyId.trim(), agent },
        headed: true,
        initialPrompt: seed,
      };
    case "repoBranch":
    case "newWorktree":
      return {
        target: {
          kind: "branch",
          repo: target.repo.trim(),
          branch: target.branch.trim(),
          agent,
          checkout: target.kind === "newWorktree" ? "new" : "existing",
        },
        headed: true,
        reviewPrompt: seed,
      };
    case "existingWorktree":
      return {
        target: { kind: "dir", path: target.path.trim(), agent },
        headed: true,
        initialPrompt: seed,
      };
  }
}

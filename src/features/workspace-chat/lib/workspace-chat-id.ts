// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";

/** The standalone pane uses the same workspace context captured by session binding. */
export function chatIdForWorkspace(workspace: Workspace): string | null {
  if (workspace.kind === "root") return null;
  if (workspace.storyId) return `story:${workspace.storyId}`;
  if (workspace.location?.kind === "machine") return `remote:${workspace.id}`;
  return workspace.cwd ? `generic:${workspace.cwd}` : null;
}

// SPDX-License-Identifier: MIT
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import type { NavSource } from "./nav-lens";
import type { ContentSource } from "./team-content-context";
import { privateFile } from "./private-file";

/** An opened file keeps its source when the nav switches to another one. */
export function resolveFileSource(
  filePath: string,
  clone?: ContentSource,
  cloneRoot?: string | null,
): { source: NavSource; contentRoot: string; orgRoot: string | null } {
  const worktree = useWorktreeRosterStore
    .getState()
    .rows?.find((row) => row.kind === "context" && filePath.startsWith(`${row.path}/`));
  if (worktree?.kind === "context")
    return {
      source: { kind: "worktree", id: worktree.id },
      contentRoot: worktree.launchCwd,
      orgRoot: worktree.path,
    };
  const privatePath = privateFile(filePath);
  return {
    source: { kind: privatePath ? "private" : "clone" },
    contentRoot: privatePath?.contentRoot ?? clone?.teamContentPath ?? "",
    orgRoot: privatePath?.repoRoot ?? cloneRoot ?? clone?.contextRepoPath ?? null,
  };
}

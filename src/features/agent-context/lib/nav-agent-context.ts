// SPDX-License-Identifier: MIT
import type { FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import { itemFilePath } from "@/features/dashboard/lib/flatten-nav-tree";
import type { AgentContextView } from "@/tauri/commands";

import type { ContextEditAction } from "../hooks/use-agent-context";
import type { ContextEditPlan, ContextMembership } from "./agent-context-model";

/**
 * SW-XS-15 — what the left nav needs from the selected agent's context: whether each row is
 * in the want set (and from which tier), and the right-click edits. Absent when the pivot is
 * on Team (everything): the nav then draws no overlay and offers no edits.
 */
export interface NavAgentContext {
  agentName: string;
  /** The repo picked with "· in ‹Repo›", whose "… for ‹Repo› only" edits are offered. */
  repoName: string | null;
  /** The content-relative path of a nav row's absolute path: a team content path, or a
   *  want-set file of the repo tier. `null` for anything else (pinned rows, other roots). */
  relOf: (absPath: string) => string | null;
  /** The same, for the team's own content only: the rows right-click edits. Repo-tier rows
   *  (under "‹repo› content") are browse-only. */
  editRelOf: (absPath: string) => string | null;
  /** While the runtime default applies, a team-wide edit writes the context out. */
  defaulted: boolean;
  /** Membership of a content-relative path, optimistic edits applied. */
  membership: (rel: string) => ContextMembership;
  plan: (rel: string, action: ContextEditAction, repoOnly: boolean) => ContextEditPlan | null;
  edit: (rel: string, action: ContextEditAction, repoOnly: boolean) => void;
}

/** The absolute path a nav row stands for: a file's path, or a folder's directory. */
export function navItemPath(item: FlatNavItem): string | null {
  switch (item.type) {
    case "folder-header":
      return item.node.absoluteDirPath;
    case "generic-dir":
      return item.entry.path;
    default:
      return itemFilePath(item);
  }
}

const INSTALLED_SCHEME = "installed://";

/** The browse-only folder the nav draws a repo's own want-set files in: "codesense content". */
export function repoGroupName(repoName: string): string {
  return `${repoName} content`;
}

/**
 * Build the nav's path → content-relative mapping for one want set: team content under
 * `teamRoot`, repo-tier files by their own path, and the synthetic
 * `installed://owner/repo/<group>/…` folders the repo tier's files are drawn in (the group
 * folder itself maps to nothing: it is a heading, not content).
 */
export function navRelResolver(
  teamRoot: string,
  view: Pick<AgentContextView, "files" | "roots"> | null,
): (absPath: string) => string | null {
  const root = teamRoot.replace(/\/+$/, "");
  const byPath = new Map<string, string>();
  for (const f of view?.files ?? []) {
    const path = view ? fileAbsPath(view, f) : null;
    if (path) byPath.set(path, f.logical);
  }
  return (abs: string) => {
    const known = byPath.get(abs);
    if (known !== undefined) return known;
    if (abs.startsWith(INSTALLED_SCHEME)) {
      // installed://owner/repo/<group>/dir/sub → dir/sub
      const rest = abs.slice(INSTALLED_SCHEME.length).replace(/\/+$/, "");
      const parts = rest.split("/").filter(Boolean);
      return parts.length > 3 ? parts.slice(3).join("/") : null;
    }
    if (!root) return null;
    if (abs === root) return "";
    if (abs.startsWith(`${root}/`)) return abs.slice(root.length + 1).replace(/\/+$/, "");
    return null;
  };
}

/** Where a want-set file lives on this Mac (its tier root + its logical path), or `null`. */
export function fileAbsPath(
  view: Pick<AgentContextView, "roots">,
  file: AgentContextView["files"][number],
): string | null {
  if (file.source) return file.source;
  const root = file.root === null ? undefined : view.roots[file.root];
  return root ? `${root.path.replace(/\/+$/, "")}/${file.logical}` : null;
}

/** The team-content-only resolver: right-click edits never reach repo-tier rows. */
export function teamRelResolver(teamRoot: string): (absPath: string) => string | null {
  const root = teamRoot.replace(/\/+$/, "");
  return (abs: string) => {
    if (!root || abs.startsWith(INSTALLED_SCHEME)) return null;
    if (abs === root) return "";
    return abs.startsWith(`${root}/`) ? abs.slice(root.length + 1).replace(/\/+$/, "") : null;
  };
}

/** The want set's repo-tier files, each with where it lives, for the nav's
 *  "‹repo› content" folder. */
export function repoTierFiles(
  view: Pick<AgentContextView, "roots" | "files">,
): Array<{ logical: string; path: string | null }> {
  return view.files
    .filter((f) => f.root !== null && view.roots[f.root]?.tier.kind === "repo")
    .map((f) => ({ logical: f.logical, path: fileAbsPath(view, f) }));
}

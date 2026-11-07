// SPDX-License-Identifier: MIT
import type { SettingRowSpec } from "@/features/settings/lib/row-matches";

/**
 * Row specs drive the Settings search filter. Each row in the
 * Workspace section renders only if `rowMatches(spec, query)` returns
 * true. Keywords broaden the match surface — add terms a user might
 * type when looking for this row.
 *
 * Labels MUST match the row header text exactly — search hits surface
 * as labelled results.
 */
export const WORKSPACE_ROW_SPECS: SettingRowSpec[] = [
  {
    // SW-XS-11 (SPEC §8.4): the read-only line that replaced the sync cadence
    // and hydration mode rows. Searching for those settings lands here.
    key: "context-delivery",
    label: "How agents get context",
    keywords: [
      "context",
      "sync",
      "cadence",
      "hydrate",
      "hydration",
      "hydration mode",
      "reverse sync",
      "publish",
      "background",
      "auto sync",
    ],
  },
  {
    key: "workspace-name",
    label: "Name",
    keywords: ["name", "rename", "workspace", "display name"],
  },
  {
    key: "workspace-remote",
    label: "Remote",
    keywords: ["remote", "github", "url", "sync"],
  },
  {
    // SW-MC-6 (AC-4): the workspace-root card slot. MC-6 ships the row + the
    // effective-default display; SW-MC-2 fills in the editable per-org value.
    key: "workspace-root",
    label: "Clone location",
    keywords: [
      "clone location",
      "workspace root",
      "clones",
      "code folder",
      "managed clones",
      "sources",
      "where clones live",
    ],
  },
  {
    key: "members",
    label: "Members",
    keywords: ["members", "add", "invite", "remove", "people"],
  },
  {
    key: "teams",
    label: "Teams",
    keywords: ["teams", "create team", "join", "leave", "lead"],
  },
  {
    key: "source-repos",
    label: "Source Repos",
    keywords: [
      "source repos",
      "repos",
      "repositories",
      "tracked repos",
      "attach",
      "detach",
      "clone",
    ],
  },
  {
    key: "branch-protection",
    label: "Branch protection",
    keywords: ["branch protection", "protection", "main", "security"],
  },
];

export type WorkspaceRowKey = (typeof WORKSPACE_ROW_SPECS)[number]["key"];

export function findRowSpec(key: WorkspaceRowKey): SettingRowSpec {
  const spec = WORKSPACE_ROW_SPECS.find((s) => s.key === key);
  if (!spec) throw new Error(`WORKSPACE_ROW_SPECS is missing "${key}"`);
  return spec;
}

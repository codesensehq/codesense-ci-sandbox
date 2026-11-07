// SPDX-License-Identifier: MIT
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { isActionableDocumentType } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { workspaceDisplayLabel } from "@/shared/lib/worktree";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { QuickPickerItem } from "../components/quick-open-dialog";

/**
 * SW-EW-2 AC-5: the quick-switch picker searches open story workspaces PLUS
 * every actionable story from the pipeline tree — so it doubles as the "open a
 * story workspace" power-path. Epic is a grouping label, never an openable
 * target, so the picker carries NO epic items. Reuses the generic
 * {@link QuickOpenDialog}; this builder adapts the domain to its
 * `{ id, label, sublabel }` shape and encodes the dispatch action in `id`.
 */
export type WorkspacePickerAction =
  | { kind: "workspace"; workspaceId: string }
  | { kind: "story"; filePath: string }
  // SW-GW-1: the "Open workspace here" entry — prompts for a dir (native folder
  // picker) then opens a generic terminal there. Not tied to an existing record.
  | { kind: "generic" };

// NUL — never appears in ids/paths, a safe field separator. Written as an
// explicit escape (not a literal NUL byte) so it stays visible in source/diffs.
const SEP = "\u0000";

export function encodePickerId(action: WorkspacePickerAction): string {
  switch (action.kind) {
    case "workspace":
      return `workspace${SEP}${action.workspaceId}`;
    case "story":
      return `story${SEP}${action.filePath}`;
    case "generic":
      return "generic";
  }
}

export function decodePickerId(id: string): WorkspacePickerAction | null {
  if (id === "generic") return { kind: "generic" };
  const parts = id.split(SEP);
  if (parts[0] === "workspace" && parts[1] != null) {
    return { kind: "workspace", workspaceId: parts[1] };
  }
  if (parts[0] === "story" && parts[1] != null) {
    return { kind: "story", filePath: parts[1] };
  }
  return null;
}

function workspaceLabel(ws: Workspace): string {
  if (ws.kind === "story") return ws.storyId ?? ws.storyFilePath ?? ws.id;
  if (ws.kind === "epic") return `epic · ${ws.sourceRepoSlug ?? ""}`.trim();
  // SW-GW-1: an open generic workspace shows its dir basename (matches its tile);
  // a segment-less cwd (root "/") shows the cwd, not the raw `generic:/…` id.
  if (ws.kind === "generic") return workspaceDisplayLabel(ws);
  return ws.id;
}

function storyFileLabel(fileName: string): string {
  return fileName.replace(/\.(md|html)$/i, "");
}

/**
 * Build the picker items: open workspaces first (jump to running work), then
 * every actionable story from the tree (launch new work). A tree story already
 * represented by an open workspace is skipped (the open item covers it) so the
 * list has no duplicates. Epics are grouping labels, not openable targets — the
 * picker carries no epic items.
 */
export function buildWorkspacePickerItems(
  openWorkspaces: readonly Workspace[],
  tree: DerivedPipelineTree | null,
): QuickPickerItem[] {
  const items: QuickPickerItem[] = [];
  const openStoryPaths = new Set<string>();

  // SW-GW-1: the "Open workspace here" door — prompts for a directory and opens a
  // plain terminal there. Always available (not tied to an open record); worded to
  // read as "open a terminal," NOT "add a project" (distinct from Add repo).
  items.push({
    id: encodePickerId({ kind: "generic" }),
    label: "Generic Workspace",
    sublabel: "open any directory as a terminal",
  });

  for (const ws of openWorkspaces) {
    // Only the kinds that own a real terminal tile are activatable targets. Skip
    // `root` (terminal-less; swept at load) so the picker can't activate a
    // workspace with no terminal — that would momentarily break the active-
    // terminal invariant the panel upholds by filtering the same kinds. `story`,
    // `epic`, and `generic` (SW-GW-1) all pin a real terminal, so they qualify.
    if (ws.kind !== "story" && ws.kind !== "epic" && ws.kind !== "generic") continue;
    if (ws.kind === "story" && ws.storyFilePath) openStoryPaths.add(ws.storyFilePath);
    const label = workspaceLabel(ws);
    items.push({
      id: encodePickerId({ kind: "workspace", workspaceId: ws.id }),
      label,
      sublabel: `open · ${label}`,
    });
  }

  if (tree) {
    const walk = (node: DerivedPipelineTree) => {
      for (const story of node.stories) {
        if (!isActionableDocumentType(story.documentType)) continue;
        if (openStoryPaths.has(story.filePath)) continue;
        const label = story.storyId ?? storyFileLabel(story.fileName);
        items.push({
          id: encodePickerId({ kind: "story", filePath: story.filePath }),
          label,
          sublabel: `${label} · ${node.dirName || "story"}`,
        });
      }
      for (const sub of node.subEpics) walk(sub);
    };
    walk(tree);
  }

  return items;
}

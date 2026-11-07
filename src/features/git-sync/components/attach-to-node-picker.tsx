// SPDX-License-Identifier: MIT
import type { Node } from "@xyflow/react";

import { basename } from "@/shared/lib/basename";
import type { GroupNodeData, StoryNodeData } from "@/shared/lib/pipeline-graph";
import { QuickOpenDialog } from "@/features/viewer/components/quick-open-dialog";

/**
 * SW-GS-11 — a candidate node for the "Attach to a story/epic" picker, adapted
 * from a pipeline React-Flow node. `mdPath` is the node's own `.md` (the write
 * target AND the picker id/React key). `label` is the display name; `sublabel`
 * is BOTH the secondary line and the fuzzy-search target (see
 * {@link QuickOpenDialog}), so it must carry a searchable identifier.
 */
export interface AttachTarget {
  mdPath: string;
  label: string;
  sublabel: string;
}

function isStoryNode(n: Node): n is Node<StoryNodeData> {
  return n.type === "story";
}
function isGroupNode(n: Node): n is Node<GroupNodeData> {
  return n.type === "group";
}

/**
 * Map the pipeline graph's story + epic nodes to attach targets. A node with no
 * own `.md` (an epic whose README hasn't been authored) is skipped — there's
 * nothing to write into. Stories search by filename (a slug that carries the
 * story id + title words, matching the SW-SC-12 picker); epics can't (their file
 * is `README.md`), so they search by their enclosing directory name.
 */
export function buildAttachTargets(nodes: Node[]): AttachTarget[] {
  const targets: AttachTarget[] = [];
  const seen = new Set<string>();
  const push = (t: AttachTarget) => {
    // Dedup by write target — two nodes resolving to one `.md` would collide on
    // the picker's React key and make one unselectable (first wins).
    if (seen.has(t.mdPath)) return;
    seen.add(t.mdPath);
    targets.push(t);
  };
  for (const n of nodes) {
    if (isStoryNode(n)) {
      const md = n.data.storyFile;
      if (!md) continue;
      // `||` (not `??`): a blank story_id falls back to the title, not an empty label.
      push({ mdPath: md, label: n.data.storyId || n.data.title, sublabel: basename(md) });
    } else if (isGroupNode(n)) {
      const md = n.data.mainDocFilePath;
      if (!md) continue;
      const slash = md.lastIndexOf("/");
      const dir = slash >= 0 ? md.slice(0, slash) : "";
      push({ mdPath: md, label: n.data.title, sublabel: basename(dir) || "epic" });
    }
  }
  return targets;
}

/**
 * SW-GS-11 — the fuzzy node picker over the active pipeline graph (stories +
 * epics). Thin adapter over the shared {@link QuickOpenDialog}: maps targets to
 * picker items and resolves the selected id back to its `AttachTarget`.
 */
export function AttachToNodePicker({
  targets,
  onSelect,
  onClose,
}: {
  targets: AttachTarget[];
  onSelect: (target: AttachTarget) => void;
  onClose: () => void;
}) {
  const byId = new Map(targets.map((t) => [t.mdPath, t]));
  return (
    <QuickOpenDialog
      open
      onClose={onClose}
      onSelect={(id) => {
        const target = byId.get(id);
        if (target) onSelect(target);
      }}
      placeholder="Attach to a story or epic..."
      emptyLabel="No stories or epics to attach to"
      // SW-FX-45: labels are story/epic display titles (`storyId || title`), not
      // filenames — an epic titled `Config.md` must render verbatim, not stripped.
      stripExtension={false}
      items={targets.map((t) => ({ id: t.mdPath, label: t.label, sublabel: t.sublabel }))}
    />
  );
}

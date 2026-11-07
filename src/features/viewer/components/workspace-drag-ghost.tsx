// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";

import { useWorkspaceLabel } from "../hooks/use-workspace-label";
import { useTrayReorderInBand } from "../lib/tray-reorder-drag";

/** The split ghost: the dragged workspace of `teamKey`, open or minimized, named in a small
 *  lifted chip. While the pointer is in the tray's band the drag is a reorder, and the tray's
 *  insertion line shows it instead. */
export function WorkspaceDragGhost({
  teamKey,
  workspaceId,
}: {
  teamKey: string;
  workspaceId: string;
}) {
  const inBand = useTrayReorderInBand();
  const ws = useViewerStore(
    (s) =>
      s.workspaces[teamKey]?.find((w) => w.id === workspaceId) ??
      s.softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace,
  );
  const label = useWorkspaceLabel(ws ?? { id: workspaceId, kind: "generic", cwd: workspaceId });
  if (inBand) return null;
  return (
    <div
      data-testid="workspace-drag-ghost"
      className="inline-flex max-w-[240px] items-center rounded-md border border-border bg-popover px-2 py-1 text-xs font-medium text-popover-foreground shadow-lg"
    >
      {/* Its own block inside the flex box: a bare text node there would not truncate. */}
      <span className="block min-w-0 truncate">{label}</span>
    </div>
  );
}

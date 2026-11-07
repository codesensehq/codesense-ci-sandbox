// SPDX-License-Identifier: MIT
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import type { AgentFace } from "@/features/viewer/lib/agent-list-view";

/** A tab face, and the box it sits in: room for the active face's ring, so selecting a tab
 *  never shifts its name. */
const TAB_FACE = 20;
const TAB_FACE_BOX = 22;

/**
 * An agent's face as a terminal tab wears it: no role piece (the agents pane and the pills
 * carry that), the live status dot on its lower-right corner, cut out by the tab's own
 * background. The active tab's face is ringed in its workspace's team colour; the rest sit
 * muted until hovered or focused.
 */
export function TabFace({
  face,
  active,
  status,
  title,
  tag,
}: {
  face: AgentFace;
  active: boolean;
  /** Its number among running agents wearing the same face. */
  tag?: number;
  /** The status dot, placed on the face's corner. */
  status?: React.ReactNode;
  title?: string;
}): React.JSX.Element {
  return (
    <span
      className="cs-agent-identity ag-tab-face shrink-0"
      data-active={active ? "" : undefined}
      style={{ width: TAB_FACE_BOX, height: TAB_FACE_BOX }}
      title={title}
    >
      <AgentFaceView face={face} size={TAB_FACE} tag={tag} />
      {status && <span className="ag-tab-face-status">{status}</span>}
    </span>
  );
}

/** Keep text beside the face even when the tab strip overflows. */
const MIN_NAME_PX = 26;

export function AgentTabLabel({
  face,
  tag,
  name,
  nameId,
  fullName,
  progressTitle = false,
  active,
  status,
  onDoubleClick,
}: {
  face: AgentFace;
  tag?: number;
  name: string;
  nameId: string;
  fullName?: string;
  progressTitle?: boolean;
  /** The tab is the selected one: its face is ringed. */
  active: boolean;
  /** The status dot, drawn over the face's corner. */
  status: React.ReactNode;
  onDoubleClick: (e: React.MouseEvent) => void;
}): React.JSX.Element {
  return (
    <>
      <TabFace face={face} tag={tag} active={active} status={status} title={fullName ?? name} />
      <span
        id={nameId}
        className="cursor-pointer truncate"
        style={{
          minWidth: MIN_NAME_PX,
          maxWidth: "24ch",
          width: progressTitle ? "24ch" : undefined,
        }}
        title={fullName ?? name}
        aria-label={fullName ?? name}
        onDoubleClick={onDoubleClick}
      >
        {name}
      </span>
    </>
  );
}

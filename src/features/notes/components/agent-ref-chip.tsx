// SPDX-License-Identifier: MIT
import { NodeViewWrapper, ReactNodeViewRenderer, type NodeViewProps } from "@tiptap/react";
import { toast } from "sonner";

import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { showAgent } from "@/features/viewer/lib/show-agent";
import { StatusMark } from "@/shared/components/status-mark";

import type { AgentRefAttrs } from "../lib/agent-ref";
import { agentStatusLook, useNoteAgents } from "../lib/note-agents";

/** The chip a handed-off line carries: the agent's live mark, face and name, and
 *  its status in words. Clicking it shows the agent. */
export function AgentRefChip({ handle, name }: AgentRefAttrs): React.JSX.Element {
  const { byHandle, statusOf, subagentsOf, teamKey } = useNoteAgents();
  const identity = byHandle.get(handle);
  const status = identity ? statusOf(identity.row) : null;
  const look = agentStatusLook(status, identity ? subagentsOf(identity.row) : 0);
  const label = identity?.label || name;
  return (
    <button
      type="button"
      className="nt-agent-chip"
      data-agent-handle={handle}
      data-mark={look.mark}
      contentEditable={false}
      title={identity ? `Show ${label}` : `${label} is not running`}
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => {
        if (!identity) {
          toast(`${label} is not running.`);
          return;
        }
        void showAgent(identity.row, { teamKey });
      }}
    >
      <StatusMark mark={look.mark} />
      {identity && <AgentFaceView face={identity.face} size={14} className="nt-agent-chip-face" />}
      <span className="nt-agent-chip-name">{label}</span>
      <span className="nt-agent-chip-status">{look.word}</span>
    </button>
  );
}

function AgentRefNodeView({ node }: NodeViewProps): React.JSX.Element {
  const attrs = node.attrs as AgentRefAttrs;
  return (
    <NodeViewWrapper as="span" className="nt-agent-ref">
      <AgentRefChip handle={attrs.handle} name={attrs.name} />
    </NodeViewWrapper>
  );
}

export const agentRefNodeView = ReactNodeViewRenderer(AgentRefNodeView, { as: "span" });

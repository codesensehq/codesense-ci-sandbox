// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import { AgentFaceView } from "./agent-identity";
import { SubagentMark } from "./subagent-mark";
import { initialsOf, type AgentFace } from "../lib/agent-list-view";
import { StatusMark } from "@/shared/components/status-mark";
import type { StatusMarkKind } from "@/shared/lib/status-mark";
import { X } from "lucide-react";

export interface PopupHeadProps {
  /** The agent's name. */
  name: string;
  face?: AgentFace;
  mark?: StatusMarkKind;
  subagents?: number;
  /** The row's status words, followed by the optional compact subagent mark. */
  statusWord: string;
  /** The workspace the agent works in, when it has one. */
  workspace: string | null;
  /** The question the agent is waiting on, shown above the header. */
  ask?: string | null;
  /** Close the popup, focus back to its row. */
  onClose: () => void;
  /** Actions drawn before the close button. */
  children?: ReactNode;
}

/**
 * The head the Agents pane's row popups share (the quick view and the inbox chat): the question
 * the agent waits on, then its name, its status (its subagents part of it), its workspace and a close
 * button, so both popups name the agent the same way.
 */
export function PopupHead({
  name,
  face,
  mark,
  subagents = 0,
  statusWord,
  workspace,
  ask,
  onClose,
  children,
}: PopupHeadProps): React.JSX.Element {
  return (
    <>
      {ask && (
        // Named for what it is: read on its own, the question alone does not say who asks it.
        <div className="ag-quick-send-ask" role="note" aria-label={`${name} asks`}>
          Waiting on you · {ask}
        </div>
      )}
      <div className="ag-quick-send-head">
        <span className="ag-quick-send-face">
          <AgentFaceView
            face={face ?? { kind: "mark", text: initialsOf(name), color: null }}
            size={20}
          />
          {mark && <StatusMark mark={mark} size={7} />}
        </span>
        <span className="ag-quick-send-name">{name}</span>
        <span className="ag-quick-send-meta">
          <span className="ag-quick-send-state">
            {statusWord}
            <SubagentMark count={subagents} />
          </span>
          {workspace && <span className="ag-quick-send-workspace"> · {workspace}</span>}
        </span>
        {children}
        <button
          type="button"
          className="ag-quick-send-btn ag-quick-send-close"
          aria-label="Close"
          onClick={onClose}
        >
          <X className="size-3.5" />
        </button>
      </div>
    </>
  );
}

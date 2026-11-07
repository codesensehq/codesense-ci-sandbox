// SPDX-License-Identifier: MIT
import { selectedIndexById } from "@/shared/lib/picker-selection";
import { useEffect, useMemo, useRef, useState } from "react";
import { Plus, Search, Terminal } from "lucide-react";

import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import { StatusMark } from "@/shared/components/status-mark";

import { agentStatusLook, useNoteAgents } from "../lib/note-agents";
import { NtPopover, NtPopoverAnchor, NtPopoverContent } from "./nt-popover";

export interface AnchorRect {
  top: number;
  left: number;
  width: number;
  height: number;
}

/** An agent's face with its live status mark tucked on the corner. */
export function AgentFaceWithMark({
  identity,
  size = 20,
}: {
  identity: TeamAgentIdentity;
  size?: number;
}): React.JSX.Element {
  const { statusOf } = useNoteAgents();
  const look = agentStatusLook(statusOf(identity.row));
  return (
    <span className="nt-av" style={{ width: size, height: size }}>
      <AgentFaceView face={identity.face} size={size} />
      <span className="nt-av-mark">
        <StatusMark mark={look.mark} size={6} />
      </span>
    </span>
  );
}

type PickRow = { kind: "agent"; identity: TeamAgentIdentity } | { kind: "new" } | { kind: "paste" };

function pickRowId(row: PickRow): string {
  return row.kind === "agent" ? `agent:${row.identity.row.handle}` : row.kind;
}

export interface SendPickerProps {
  open: boolean;
  anchor: AnchorRect | null;
  /** Where the agents run, for the list's label ("Agents in Acme Widgets"). */
  placeLabel: string;
  includeContext: boolean;
  onIncludeContextChange: (include: boolean) => void;
  onSendToAgent: (identity: TeamAgentIdentity) => void;
  onStartNew: () => void;
  onPasteTerminal: () => void;
  onClose: () => void;
}

/**
 * "Send to…": the team's running agents (face, live mark and status in the shared
 * words), then a new agent or the focused terminal. Opened by ⌘↵, a line's send
 * button, a selection's Send to…, or a prompt block's Send.
 */
export function SendPicker({
  open,
  anchor,
  placeLabel,
  includeContext,
  onIncludeContextChange,
  onSendToAgent,
  onStartNew,
  onPasteTerminal,
  onClose,
}: SendPickerProps): React.JSX.Element {
  const { agents, statusOf, subagentsOf } = useNoteAgents();
  const [query, setQuery] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const sendable = useMemo(() => agents.filter((a) => a.row.alive && a.row.sessionId), [agents]);
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? sendable.filter((a) => a.label.toLowerCase().includes(q)) : sendable;
  }, [sendable, query]);
  const rows: PickRow[] = useMemo(
    () => [
      ...shown.map((identity) => ({ kind: "agent" as const, identity })),
      { kind: "new" as const },
      { kind: "paste" as const },
    ],
    [shown],
  );
  const activeIndex = selectedIndexById(rows, selectedId, pickRowId);
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[aria-selected="true"]')
      ?.scrollIntoView?.({ block: "nearest" });
  }, [activeIndex]);

  const run = (row: PickRow) => {
    if (row.kind === "agent") onSendToAgent(row.identity);
    else if (row.kind === "new") onStartNew();
    else onPasteTerminal();
  };

  return (
    <NtPopover
      open={open && anchor !== null}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <NtPopoverAnchor asChild>
        <div
          aria-hidden
          style={{
            position: "fixed",
            top: anchor?.top ?? 0,
            left: anchor?.left ?? 0,
            width: anchor?.width ?? 0,
            height: anchor?.height ?? 0,
            pointerEvents: "none",
          }}
        />
      </NtPopoverAnchor>
      <NtPopoverContent
        side="bottom"
        align="start"
        className="nt-send-picker"
        style={{ width: 290 }}
        aria-label="Send to"
        onCloseAutoFocus={(e) => e.preventDefault()}
      >
        <div className="nt-pop-search">
          <Search size={14} aria-hidden />
          <input
            autoFocus
            value={query}
            placeholder="Send to"
            aria-label="Send to"
            aria-activedescendant={`nt-send-row-${activeIndex}`}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelectedId(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                e.stopPropagation();
                setSelectedId(pickRowId(rows[(activeIndex + 1) % rows.length]));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                e.stopPropagation();
                setSelectedId(pickRowId(rows[(activeIndex - 1 + rows.length) % rows.length]));
              } else if (e.key === "Enter") {
                e.preventDefault();
                if (e.metaKey && e.altKey) onPasteTerminal();
                else run(rows[activeIndex]);
              }
            }}
          />
        </div>
        <div className="nt-pop-label">Agents in {placeLabel}</div>
        {shown.length === 0 && (
          <div className="nt-pop-empty">
            {sendable.length === 0
              ? "No agents are running. Start one with this line."
              : "No agent by that name."}
          </div>
        )}
        <div ref={listRef} role="listbox" aria-label="Agents" className="nt-send-list">
          {rows.map((row, i) => {
            const isActive = i === activeIndex;
            if (row.kind !== "agent") return null;
            const look = agentStatusLook(statusOf(row.identity.row), subagentsOf(row.identity.row));
            return (
              <button
                key={row.identity.row.handle}
                id={`nt-send-row-${i}`}
                type="button"
                role="option"
                aria-selected={isActive}
                data-active={isActive}
                className="nt-row"
                onMouseMove={() => setSelectedId(pickRowId(row))}
                onClick={() => run(row)}
              >
                <AgentFaceWithMark identity={row.identity} />
                <span className="nt-row-label">{row.identity.label}</span>
                <span className="nt-row-sub">{look.word}</span>
              </button>
            );
          })}
        </div>
        <div className="nt-pop-sep" />
        {rows.map((row, i) => {
          if (row.kind === "agent") return null;
          const isActive = i === activeIndex;
          return (
            <button
              key={row.kind}
              id={`nt-send-row-${i}`}
              type="button"
              data-active={isActive}
              className="nt-row"
              onMouseMove={() => setSelectedId(pickRowId(row))}
              onClick={() => run(row)}
            >
              <span className="nt-row-icon">
                {row.kind === "new" ? <Plus size={14} /> : <Terminal size={14} />}
              </span>
              <span className="nt-row-label">
                {row.kind === "new" ? "Start a new agent with this" : "Paste into focused terminal"}
              </span>
              {row.kind === "paste" && (
                <span className="nt-row-sub">
                  <kbd className="nt-kbd">⌥⌘↵</kbd>
                </span>
              )}
            </button>
          );
        })}
        <label className="nt-pop-check">
          <input
            type="checkbox"
            checked={includeContext}
            onChange={(e) => onIncludeContextChange(e.target.checked)}
          />
          Include the note as context
        </label>
      </NtPopoverContent>
    </NtPopover>
  );
}

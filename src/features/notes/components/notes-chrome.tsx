// SPDX-License-Identifier: MIT
import { useState, type ReactNode } from "react";
import {
  ChevronDown,
  Clock,
  History,
  MoreHorizontal,
  PanelLeft,
  Pencil,
  Pin,
  PinOff,
  Plus,
  Search,
  Send,
  Sparkles,
  Trash2,
  Wand,
} from "lucide-react";

import { NtPopover, NtPopoverContent, NtPopoverTrigger } from "./nt-popover";

export interface AgentMenuActions {
  onOrganize: () => void;
  onSendNote: () => void;
  onRoutine: () => void;
  /** Present only where note history exists. */
  onChanges?: () => void;
  /** How many of the note's saved changes agents made. */
  agentChangeCount: number;
}

/** "Agent ▾": what an agent can do for this note — all through the normal agents
 *  (a launch, a running session, a routine), with results in the Inbox. */
export function AgentMenu({
  disabled,
  actions,
}: {
  disabled?: boolean;
  actions: AgentMenuActions;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const pick = (fn: () => void) => () => {
    setOpen(false);
    fn();
  };
  return (
    <NtPopover open={open} onOpenChange={setOpen}>
      <NtPopoverTrigger asChild>
        <button type="button" className="nt-btn" disabled={disabled} title="Agent actions">
          <Sparkles size={14} aria-hidden />
          Agent
          <ChevronDown size={12} className="nt-btn-chev" aria-hidden />
        </button>
      </NtPopoverTrigger>
      <NtPopoverContent style={{ width: 264 }} aria-label="Agent actions">
        <div className="nt-pop-label">This note</div>
        <MenuRow icon={<Wand size={14} />} onClick={pick(actions.onOrganize)} autoFocus>
          Organize with an agent…
        </MenuRow>
        <MenuRow icon={<Send size={14} />} onClick={pick(actions.onSendNote)}>
          Send to a running agent…
        </MenuRow>
        <div className="nt-pop-sep" />
        <div className="nt-pop-label">Keep notes organized</div>
        <MenuRow icon={<Clock size={14} />} onClick={pick(actions.onRoutine)}>
          Set up a routine…
        </MenuRow>
        {actions.onChanges && (
          <MenuRow
            icon={<History size={14} />}
            onClick={pick(actions.onChanges)}
            sub={actions.agentChangeCount > 0 ? String(actions.agentChangeCount) : undefined}
          >
            Changes by agents
          </MenuRow>
        )}
        <div className="nt-pop-note">
          Uses your agents and the normal launch. Results land in your Inbox.
        </div>
      </NtPopoverContent>
    </NtPopover>
  );
}

function MenuRow({
  icon,
  sub,
  autoFocus,
  onClick,
  children,
}: {
  icon: ReactNode;
  sub?: string;
  autoFocus?: boolean;
  onClick: () => void;
  children: ReactNode;
}): React.JSX.Element {
  return (
    <button type="button" className="nt-row" data-nt-row autoFocus={autoFocus} onClick={onClick}>
      <span className="nt-row-icon">{icon}</span>
      <span className="nt-row-label">{children}</span>
      {sub && <span className="nt-row-sub">{sub}</span>}
    </button>
  );
}

/** "…": the note itself — new, pin, rename, delete. */
export function NoteActionsMenu({
  pinned,
  hasNote,
  onNew,
  onTogglePin,
  onRename,
  onDelete,
}: {
  pinned: boolean;
  hasNote: boolean;
  onNew: () => void;
  onTogglePin: () => void;
  onRename: () => void;
  onDelete: () => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const pick = (fn: () => void) => () => {
    setOpen(false);
    fn();
  };
  return (
    <NtPopover open={open} onOpenChange={setOpen}>
      <NtPopoverTrigger asChild>
        <button type="button" className="nt-ib" aria-label="Note actions" title="Note actions">
          <MoreHorizontal size={16} />
        </button>
      </NtPopoverTrigger>
      <NtPopoverContent style={{ minWidth: 176 }} aria-label="Note actions">
        <MenuRow icon={<Plus size={14} />} onClick={pick(onNew)} sub="⌘N" autoFocus>
          New note
        </MenuRow>
        {hasNote && (
          <>
            <MenuRow
              icon={pinned ? <PinOff size={14} /> : <Pin size={14} />}
              onClick={pick(onTogglePin)}
            >
              {pinned ? "Unpin" : "Pin"}
            </MenuRow>
            <MenuRow icon={<Pencil size={14} />} onClick={pick(onRename)}>
              Rename
            </MenuRow>
            <div className="nt-pop-sep" />
            <MenuRow icon={<Trash2 size={14} />} onClick={pick(onDelete)}>
              Delete
            </MenuRow>
          </>
        )}
      </NtPopoverContent>
    </NtPopover>
  );
}

/** The one chrome row. Narrow: list toggle, "Find a note", outline, then the agent
 *  and note menus. Wide (list and outline are columns): just the menus. */
export function NotesChrome({
  wide,
  listOpen,
  outline,
  onToggleList,
  onFind,
  agentMenu,
  noteMenu,
}: {
  wide: boolean;
  listOpen: boolean;
  /** The outline popover's trigger + content (narrow only). */
  outline: ReactNode;
  onToggleList: () => void;
  onFind: () => void;
  agentMenu: ReactNode;
  noteMenu: ReactNode;
}): React.JSX.Element {
  return (
    <div className="nt-chrome">
      {wide ? (
        <span className="nt-chrome-gap" />
      ) : (
        <>
          <button
            type="button"
            className="nt-ib"
            aria-label="Notes list"
            title="Notes list (⌘B)"
            aria-pressed={listOpen}
            onClick={onToggleList}
          >
            <PanelLeft size={16} />
          </button>
          <button type="button" className="nt-find" onClick={onFind} aria-label="Find a note">
            <Search size={14} aria-hidden />
            <span className="nt-find-text">Find a note</span>
            <kbd className="nt-kbd">⌘O</kbd>
          </button>
          {outline}
          <span className="nt-hair-v" aria-hidden />
        </>
      )}
      {agentMenu}
      {noteMenu}
    </div>
  );
}

// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { memo, useCallback, useMemo, useRef, useState } from "react";
import { MoreHorizontal, Pencil, Pin, PinOff, Plus, Search, Trash2 } from "lucide-react";

import { partitionNotes } from "@/features/viewer/lib/note-ordering";
import { StatusMark } from "@/shared/components/status-mark";
import type { NoteMeta } from "@/tauri/commands";
import { AgentFaceView } from "@/features/viewer/components/agent-identity";

import { agentStatusLook, useNoteAgents } from "../lib/note-agents";
import type { NoteSummary } from "../lib/note-summary";
import { ActorFace } from "./note-changes";
import { NtPopover, NtPopoverContent, NtPopoverTrigger } from "./nt-popover";

/** How long ago a note was opened, as the list's right edge shows it: "now", "4m",
 *  "1h", a weekday within the week, else a date. */
export function listTime(at: number, now: number): string {
  const mins = Math.floor((now - at) / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const d = new Date(at);
  if (hours < 24 * 6) return d.toLocaleDateString([], { weekday: "short" });
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

export interface NotesListProps {
  notes: readonly NoteMeta[];
  activeNoteId: string | null;
  summaries: ReadonlyMap<string, NoteSummary>;
  now: number;
  /** `column` beside the note when the pane is wide; `drawer` over it when narrow. */
  variant: "column" | "drawer";
  onSelect: (id: string) => void;
  onCreate: (title?: string) => void;
  onRename: (id: string, title: string) => void;
  onRequestDelete: (id: string) => void;
  onTogglePin: (id: string, pinned: boolean) => void;
  /** Escape in the drawer. */
  onDismiss?: () => void;
}

/**
 * The note list: search over titles and text, then Pinned and Recent. Each note shows
 * its first lines and the faces of the agents its lines were handed to, with their
 * live marks. Arrow keys move between notes; Enter opens one.
 */
export const NotesList = memo(function NotesList({
  notes,
  activeNoteId,
  summaries,
  now,
  variant,
  onSelect,
  onCreate,
  onRename,
  onRequestDelete,
  onTogglePin,
  onDismiss,
}: NotesListProps) {
  const [query, setQuery] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return [...notes];
    return notes.filter(
      (n) =>
        n.title.toLowerCase().includes(q) ||
        (summaries.get(n.id)?.snippet.toLowerCase().includes(q) ?? false),
    );
  }, [notes, query, summaries]);
  const { pinned, recent } = useMemo(() => partitionNotes(filtered), [filtered]);
  const order = useMemo(() => [...pinned, ...recent], [pinned, recent]);

  const focusRow = useCallback((id: string) => {
    bodyRef.current?.querySelector<HTMLElement>(`[data-note-id="${id}"]`)?.focus();
  }, []);

  const onListKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const current = (document.activeElement as HTMLElement | null)?.dataset.noteId;
    const at = order.findIndex((n) => n.id === current);
    const next =
      order[Math.max(0, Math.min(order.length - 1, at + (e.key === "ArrowDown" ? 1 : -1)))];
    if (next) {
      e.preventDefault();
      focusRow(next.id);
    }
  };

  const noMatch = query.trim().length > 0 && filtered.length === 0;

  const row = (n: NoteMeta) => (
    <NoteItem
      key={n.id}
      note={n}
      active={n.id === activeNoteId}
      summary={summaries.get(n.id)}
      now={now}
      renaming={renamingId === n.id}
      onSelect={onSelect}
      onStartRename={setRenamingId}
      onSubmitRename={(title) => {
        onRename(n.id, title);
        setRenamingId(null);
      }}
      onCancelRename={() => setRenamingId(null)}
      onTogglePin={onTogglePin}
      onRequestDelete={onRequestDelete}
    />
  );

  return (
    <nav
      aria-label="Notes"
      className={variant === "column" ? "nt-list nt-list-column" : "nt-list"}
      onKeyDown={(e) => {
        if (e.key === "Escape" && onDismiss && !e.defaultPrevented) {
          e.stopPropagation();
          onDismiss();
        }
      }}
    >
      <div className="nt-list-head">
        <label className="nt-list-search">
          <Search size={14} aria-hidden />
          <input
            value={query}
            autoFocus={variant === "drawer"}
            placeholder="Search titles and text"
            aria-label="Search notes"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" && order[0]) {
                e.preventDefault();
                focusRow(order[0].id);
              } else if (e.key === "Enter" && noMatch) {
                e.preventDefault();
                onCreate(query.trim());
                setQuery("");
              } else if (e.key === "Escape" && query) {
                e.preventDefault();
                e.stopPropagation();
                setQuery("");
              }
            }}
          />
          {variant === "column" && <kbd className="nt-kbd">⌘O</kbd>}
        </label>
        <button
          type="button"
          className="nt-ib"
          aria-label="New note"
          title="New note (⌘N)"
          onClick={() => onCreate()}
        >
          <Plus size={16} />
        </button>
      </div>
      <div
        ref={bodyRef}
        className="nt-list-body"
        role={noMatch ? undefined : "listbox"}
        aria-label={noMatch ? undefined : "Notes"}
        onKeyDown={onListKeyDown}
      >
        {noMatch ? (
          <div className="nt-list-empty">
            No note matches “{query.trim()}”.{" "}
            <button
              type="button"
              className="nt-link"
              onClick={() => {
                onCreate(query.trim());
                setQuery("");
              }}
            >
              Create it
            </button>
          </div>
        ) : (
          <>
            {pinned.length > 0 && (
              <div role="group" aria-label="Pinned">
                <div className="nt-list-group" aria-hidden>
                  <Pin size={12} />
                  Pinned
                </div>
                {pinned.map(row)}
              </div>
            )}
            {recent.length > 0 && (
              <div role="group" aria-label="Recent">
                <div className="nt-list-group" aria-hidden>
                  Recent
                </div>
                {recent.map(row)}
              </div>
            )}
          </>
        )}
      </div>
    </nav>
  );
});

function NoteItem({
  note,
  active,
  summary,
  now,
  renaming,
  onSelect,
  onStartRename,
  onSubmitRename,
  onCancelRename,
  onTogglePin,
  onRequestDelete,
}: {
  note: NoteMeta;
  active: boolean;
  summary: NoteSummary | undefined;
  now: number;
  renaming: boolean;
  onSelect: (id: string) => void;
  onStartRename: (id: string) => void;
  onSubmitRename: (title: string) => void;
  onCancelRename: () => void;
  onTogglePin: (id: string, pinned: boolean) => void;
  onRequestDelete: (id: string) => void;
}): React.JSX.Element {
  const [menuOpen, setMenuOpen] = useState(false);
  const { byHandle, statusOf } = useNoteAgents();
  const faces = (summary?.refs ?? [])
    .map((r) => byHandle.get(r.handle))
    .filter((x) => x !== undefined)
    .slice(0, 5);

  if (renaming) {
    return (
      <div className="nt-item">
        <input
          className="nt-rename"
          autoFocus
          defaultValue={note.title}
          aria-label={`Rename ${note.title}`}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") onSubmitRename(e.currentTarget.value.trim() || note.title);
            else if (e.key === "Escape") onCancelRename();
          }}
          onBlur={(event) => {
            const field = event.currentTarget;
            commitTextEditOnBlur(field, () => onSubmitRename(field.value.trim() || note.title));
          }}
        />
      </div>
    );
  }

  return (
    <div
      className="nt-item"
      role="option"
      aria-selected={active}
      tabIndex={active ? 0 : -1}
      data-note-id={note.id}
      style={active ? ({ "--nt-item-bg": "var(--nt-wash-2)" } as React.CSSProperties) : undefined}
      onClick={() => onSelect(note.id)}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect(note.id);
        }
      }}
    >
      <div className="nt-item-title">
        <span className="nt-item-title-text">{note.title || "Untitled"}</span>
        <span className="nt-item-time">{listTime(note.lastOpenedAt, now)}</span>
      </div>
      {note.updatedBy?.kind === "agent" && note.updatedAt ? (
        <div className="nt-item-snippet nt-item-edited">
          <ActorFace actor={note.updatedBy} size={14} />
          <span>
            {note.updatedBy.name} edited · {listTime(note.updatedAt, now)}
          </span>
        </div>
      ) : summary?.snippet ? (
        <div className="nt-item-snippet">{summary.snippet}</div>
      ) : null}
      {faces.length > 0 && (
        <div className="nt-item-marks" aria-label="Handed to">
          {faces.map((identity) => (
            <span key={identity.row.handle} className="nt-av" title={identity.label}>
              <AgentFaceView face={identity.face} size={20} />
              <span className="nt-av-mark">
                <StatusMark mark={agentStatusLook(statusOf(identity.row)).mark} size={6} />
              </span>
            </span>
          ))}
        </div>
      )}
      <div className="nt-item-actions" data-open={menuOpen}>
        <button
          type="button"
          className="nt-ib"
          aria-label={note.pinned ? `Unpin ${note.title}` : `Pin ${note.title}`}
          title={note.pinned ? "Unpin" : "Pin"}
          onClick={(e) => {
            e.stopPropagation();
            onTogglePin(note.id, !note.pinned);
          }}
        >
          {note.pinned ? <PinOff size={12} /> : <Pin size={12} />}
        </button>
        <NtPopover open={menuOpen} onOpenChange={setMenuOpen}>
          <NtPopoverTrigger asChild>
            <button
              type="button"
              className="nt-ib"
              aria-label={`More actions for ${note.title}`}
              onClick={(e) => e.stopPropagation()}
            >
              <MoreHorizontal size={14} />
            </button>
          </NtPopoverTrigger>
          <NtPopoverContent style={{ minWidth: 150 }} onClick={(e) => e.stopPropagation()}>
            <button
              type="button"
              className="nt-row"
              data-nt-row
              onClick={() => {
                setMenuOpen(false);
                onStartRename(note.id);
              }}
            >
              <span className="nt-row-icon">
                <Pencil size={13} />
              </span>
              Rename
            </button>
            <button
              type="button"
              className="nt-row"
              data-nt-row
              onClick={() => {
                setMenuOpen(false);
                onRequestDelete(note.id);
              }}
            >
              <span className="nt-row-icon">
                <Trash2 size={13} />
              </span>
              Delete
            </button>
          </NtPopoverContent>
        </NtPopover>
      </div>
    </div>
  );
}

// SPDX-License-Identifier: MIT
import { useState } from "react";
import { History, Undo2 } from "lucide-react";

import { formatAgo } from "@/shared/lib/format-updated-ago";
import type { NoteActor, NoteRevision } from "@/tauri/commands";

import { useNoteAgents } from "../lib/note-agents";
import { AgentRowFace, useAgentRowNamed } from "./agent-row-face";
import { PaneSheet } from "./pane-sheet";
import { AgentFaceWithMark } from "./send-picker";

/** Who made a revision, as a face: the agent's own (with its live mark) while it
 *  runs, else the face of the agent it was launched from, else its initials; the
 *  person reads "You". */
export function ActorFace({
  actor,
  size = 20,
}: {
  actor: NoteActor;
  size?: number;
}): React.JSX.Element {
  const { byHandle } = useNoteAgents();
  const identity = actor.handle ? byHandle.get(actor.handle) : undefined;
  const definition = useAgentRowNamed(actor.kind === "agent" ? actor.name : undefined);
  if (identity) return <AgentFaceWithMark identity={identity} size={size} />;
  if (definition) return <AgentRowFace agent={definition} size={size} />;
  const initials =
    actor.kind === "human"
      ? "You"
      : actor.name
          .split(/\s+/)
          .map((w) => w[0] ?? "")
          .join("")
          .slice(0, 2)
          .toUpperCase();
  return (
    <span
      className="nt-av-initials"
      aria-hidden
      style={size === 20 ? undefined : { width: size, height: size, fontSize: size * 0.45 }}
    >
      {initials}
    </span>
  );
}

function clock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** When a revision was made: its time today, else how long ago. */
export function whenLabel(at: number, now: number): string {
  const sameDay = new Date(at).toDateString() === new Date(now).toDateString();
  return sameDay && now - at > 60 * 60 * 1000 ? clock(at) : formatAgo(at, now);
}

/** What a revision did, without the actor's name the saved summary leads with
 *  ("Notes tidy edited “Prompts”" reads "edited “Prompts”" beside the name). */
export function revisionWhat(rev: NoteRevision): string {
  if (rev.change === "restore") return "restored an earlier version";
  // The person's own saves carry a stock summary; "You edited" says it.
  if (rev.actor.kind === "human") return "edited";
  const summary = rev.summary.trim();
  const name = rev.actor.name;
  const rest =
    name && summary.toLowerCase().startsWith(`${name.toLowerCase()} `)
      ? summary.slice(name.length + 1).trim()
      : summary;
  return rest || "edited";
}

/** "18:00 · +4 −3" */
function RevisionSub({ rev, now }: { rev: NoteRevision; now: number }): React.JSX.Element {
  return (
    <div className="nt-hrow-sub">
      {whenLabel(rev.at, now)} · <span className="nt-add">+{rev.added}</span>{" "}
      <span className="nt-del">−{rev.removed}</span>
    </div>
  );
}

function changeCount(rev: NoteRevision): number {
  return rev.added + rev.removed;
}

/** The strip over the sheet after an agent changed the note: who, how, and the two
 *  things to do about it. In review it holds the review's own actions. */
export function ChangeBanner({
  rev,
  changeCount: shownChanges,
  reviewing,
  now,
  onReview,
  onUndo,
  onDone,
}: {
  rev: NoteRevision;
  /** The changed lines on screen, when the earlier body has been compared. */
  changeCount?: number;
  reviewing: boolean;
  now: number;
  onReview: () => void;
  onUndo: () => void;
  onDone: () => void;
}): React.JSX.Element {
  const n = shownChanges ?? changeCount(rev);
  const changes = `${n} change${n === 1 ? "" : "s"}`;
  return (
    <div className="nt-banner" role="status">
      <ActorFace actor={rev.actor} />
      {reviewing ? (
        <span className="nt-banner-text">
          <b>{changes}</b> by {rev.actor.name}
          <span className="nt-banner-sub">Removed lines are struck through</span>
        </span>
      ) : (
        <span className="nt-banner-text">
          <span title={revisionWhat(rev)}>
            <b>{rev.actor.name}</b>{" "}
            {rev.change === "restore" ? "restored this" : "reorganized this"}
          </span>
          <span className="nt-banner-sub">
            {whenLabel(rev.at, now)} · {changes}
          </span>
        </span>
      )}
      {reviewing ? (
        <>
          <button type="button" className="nt-btn nt-btn-sm" onClick={onUndo}>
            Undo all
          </button>
          <button type="button" className="nt-btn nt-btn-sm nt-btn-prim" onClick={onDone}>
            Done
          </button>
        </>
      ) : (
        <>
          <button type="button" className="nt-btn nt-btn-sm" onClick={onReview}>
            Review
          </button>
          <button
            type="button"
            className="nt-ib"
            aria-label="Undo these changes"
            title="Undo these changes"
            onClick={onUndo}
          >
            <Undo2 size={14} />
          </button>
        </>
      )}
    </div>
  );
}

/** "Changes to <note>": who changed it, newest first, each with Review (an agent's
 *  change) or Restore (the person's own earlier version). */
export function HistorySheet({
  top,
  title,
  entries,
  now,
  onReview,
  onRestore,
  onClose,
}: {
  top: number;
  title: string;
  entries: readonly NoteRevision[];
  now: number;
  onReview: (rev: NoteRevision) => void;
  onRestore: (rev: NoteRevision) => void;
  onClose: () => void;
}): React.JSX.Element {
  const [all, setAll] = useState(false);
  const shown = all ? entries : entries.slice(0, 3);
  return (
    <PaneSheet label={`Changes to ${title}`} top={top} onClose={onClose}>
      <div className="nt-pop-label">Changes to {title}</div>
      {entries.length === 0 && <div className="nt-pop-empty">No saved changes yet.</div>}
      <div style={{ maxHeight: 360, overflowY: "auto" }}>
        {shown.map((rev, i) => (
          <div key={rev.commit} className="nt-hrow">
            <ActorFace actor={rev.actor} />
            <div className="nt-hrow-text">
              <div>
                <b>{rev.actor.kind === "human" ? "You" : rev.actor.name}</b> {revisionWhat(rev)}
              </div>
              <RevisionSub rev={rev} now={now} />
            </div>
            {rev.actor.kind === "agent" ? (
              <button
                type="button"
                className="nt-btn nt-btn-sm"
                data-autofocus={i === 0 ? true : undefined}
                onClick={() => onReview(rev)}
              >
                Review
              </button>
            ) : (
              i > 0 && (
                <button
                  type="button"
                  className="nt-btn nt-btn-sm"
                  onClick={() => onRestore(rev)}
                  title="Make this version the note again"
                >
                  Restore
                </button>
              )
            )}
          </div>
        ))}
      </div>
      {entries.length > 0 && (
        <>
          <div className="nt-pop-sep" />
          <button
            type="button"
            className="nt-row"
            data-nt-row
            aria-expanded={all}
            onClick={() => setAll((v) => !v)}
          >
            <span className="nt-row-icon">
              <History size={14} />
            </span>
            {all ? "Recent changes only" : "All note history"}
            <span className="nt-row-sub">{entries.length}</span>
          </button>
        </>
      )}
    </PaneSheet>
  );
}

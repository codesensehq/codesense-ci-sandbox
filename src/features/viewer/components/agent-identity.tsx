// SPDX-License-Identifier: MIT
import { useState, type CSSProperties } from "react";

import { cn } from "@/lib/utils";
import type { AgentFace, AgentPiece, AgentRole } from "@/features/viewer/lib/agent-list-view";

// The shared visual identity of an agent: its face (which agent), its chess piece (its
// role in the launch tree), drawn the same way in
// the agents list, its collapsed rail and the workspace tray. How it is doing is the
// shared status mark (`shared/components/status-mark.tsx`).

// The pieces, drawn on a 16-unit grid with one stroke weight so they read at 9px.
const PIECE_PATHS: Record<AgentPiece, React.ReactNode> = {
  manager: (
    <>
      <circle cx="3" cy="3.2" r="1.1" />
      <circle cx="8" cy="2.2" r="1.1" />
      <circle cx="13" cy="3.2" r="1.1" />
      <path d="m3 5.5 2 4.5h6l2-4.5-3 2-2-3-2 3zM5 12h6M4 14.5h8" />
    </>
  ),
  worker: (
    <>
      <circle cx="8" cy="4.2" r="2.4" />
      <path d="M6.2 7.2h3.6M7 7.2v2.6l-2.6 2.6v2.1h7.2v-2.1L9 9.8V7.2" />
    </>
  ),
  custom: (
    <path d="M8 1.5c-4.6 3.6-4.6 6.6 0 8.6 4.6-2 4.6-5 0-8.6ZM8 3.6l1.8 1.8M6 10.4l-1 3h6l-1-3M4 14.5h8" />
  ),
};

/** A chess piece: the queen for a manager, a pawn for a worker. */
export function PieceGlyph({
  piece,
  className,
  style,
  "data-testid": testId,
}: {
  piece: AgentPiece;
  className?: string;
  style?: CSSProperties;
  "data-testid"?: string;
}): React.JSX.Element {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      className={className}
      style={style}
      data-testid={testId}
    >
      {PIECE_PATHS[piece]}
    </svg>
  );
}

/** The colour a piece wears: its workspace's team colour. */
export function pieceColor(teamHue: number | null): string {
  if (teamHue === null) return "var(--ag-muted)";
  return `oklch(var(--ag-piece-l) var(--ag-piece-c) ${teamHue})`;
}

/** Smaller than this, a face's number tag cannot be read, so it is left off: every surface
 *  that tags faces (list rows 26px, tile tabs 20px, pill crews 15–17px) is at least this. */
export const MIN_TAGGED_FACE_PX = 15;

/** The number on a face's top-left corner that tells apart agents wearing the same face. */
function FaceTag({ tag, size }: { tag: number; size: number }): React.JSX.Element {
  const font = Math.max(8, Math.round(size * 0.36));
  return (
    <span
      className="ag-face-tag"
      data-testid="agent-face-tag"
      style={{ fontSize: font, height: font + 3, minWidth: font + 3 }}
    >
      {tag}
    </span>
  );
}

/** An agent's face: an avatar-pack image, or a mark; with `tag`, the
 *  number that tells it apart from other running agents wearing the same face. */
export function AgentFaceView({
  face,
  size,
  className,
  tag,
}: {
  face: AgentFace;
  size: number;
  className?: string;
  tag?: number;
}): React.JSX.Element {
  const [failed, setFailed] = useState<string | null>(null);
  const box: CSSProperties = { width: size, height: size };
  const tagged = tag !== undefined && size >= MIN_TAGGED_FACE_PX;
  const tagEl = tagged ? <FaceTag tag={tag} size={size} /> : null;
  if (face.kind === "mark") {
    return (
      <span
        aria-hidden
        className={cn("ag-face ag-face-mark", tagged && "ag-face-tagged", className)}
        style={{
          ...box,
          fontSize: Math.round(size * 0.42),
          ...(face.color ? { backgroundColor: face.color, color: "#fff" } : {}),
        }}
      >
        {face.text}
        {tagEl}
      </span>
    );
  }
  const src = face.src;
  // An image that will not load leaves the plain face backdrop, never a broken image.
  return (
    <span
      aria-hidden
      data-face={face.ref}
      className={cn("ag-face", tagged && "ag-face-tagged", className)}
      style={box}
    >
      {failed !== src && <img src={src} alt="" draggable={false} onError={() => setFailed(src)} />}
      {tagEl}
    </span>
  );
}

/**
 * Face plus role: the portrait with its chess piece badged over its lower-right corner (a list
 * row moves it to the lower-left, leaving that corner to the status dot), as a small tile in the
 * caller's piece tone (the agents pane passes neutral ink) cut out of the face by a ring of the
 * panel colour. The badge scales with the face (about 45%) unless a size is given. The piece is
 * keyed by its kind, so a worker that starts launching visibly turns into a manager.
 */
export function AgentAvatar({
  face,
  role,
  pieceTone,
  size = 22,
  className,
  tag,
  pieceSize,
}: {
  face: AgentFace;
  role: AgentRole;
  /** The colour of the role piece (a CSS colour), or none for the muted ink. */
  pieceTone: string | null;
  size?: number;
  className?: string;
  /** The number telling it apart from others wearing the same face, on the opposite corner. */
  tag?: number;
  /** The role badge's side in px; about 45% of the face when not given. */
  pieceSize?: number;
}): React.JSX.Element {
  const color = pieceTone ?? "var(--ag-muted)";
  const badge = pieceSize ?? Math.max(8, Math.round(size * 0.46));
  return (
    <span className={cn("ag-avatar", className)} style={{ width: size, height: size }}>
      <AgentFaceView face={face} size={size} tag={tag} />
      <span
        key={role.piece}
        className="ag-piece"
        style={{ ["--ag-piece-color" as string]: color, width: badge, height: badge }}
        title={`Hierarchy: ${role.label}`}
      >
        <PieceGlyph
          piece={role.piece}
          style={{ width: Math.round(badge * 0.74), height: Math.round(badge * 0.74) }}
        />
      </span>
    </span>
  );
}

export interface CrewMember {
  key: string;
  face: AgentFace;
  name: string;
  /** Its number among running agents wearing the same face. */
  tag?: number;
}

/** The agent leading a crew: its face, badged with its role piece. */
export interface CrewLead {
  face: AgentFace;
  piece: AgentPiece;
  /** Its role in words. */
  label: string;
  hue: number | null;
  tag?: number;
}

/**
 * A workspace's crew at a glance: its lead's face wearing its role piece, then up to three
 * overlapping faces and "+N" for the rest. New faces slide in; the stack itself never
 * reorders on a tick.
 */
export function CrewStack({
  lead,
  crew,
  more,
  size = 14,
  className,
  tone,
}: {
  lead: CrewLead | null;
  crew: readonly CrewMember[];
  more: number;
  size?: number;
  className?: string;
  /** The lead's role piece colour, a CSS colour (the agents pane passes its neutral ink). Without
   *  it, the piece wears the lead's workspace hue. */
  tone?: string | null;
}): React.JSX.Element | null {
  if (!lead && crew.length === 0) return null;
  return (
    <span className={cn("ag-crew", className)} aria-hidden>
      {lead && (
        <AgentAvatar
          face={lead.face}
          role={{ piece: lead.piece, label: lead.label }}
          pieceTone={tone ?? pieceColor(lead.hue)}
          size={size + 2}
          className="ag-crew-lead"
          tag={lead.tag}
        />
      )}
      {crew.length > 0 && (
        <span className="ag-crew-faces">
          {crew.map((m) => (
            <AgentFaceView
              key={m.key}
              face={m.face}
              size={size}
              className="ag-crew-face"
              tag={m.tag}
            />
          ))}
          {more > 0 && <span className="ag-crew-more">+{more}</span>}
        </span>
      )}
    </span>
  );
}

/** Whether the list had settled when this row first mounted: rows arriving later animate in. */
export function useArrivedLate(settled: boolean): boolean {
  const [late] = useState(settled);
  return late;
}

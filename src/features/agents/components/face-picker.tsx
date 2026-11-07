// SPDX-License-Identifier: MIT
import { useMemo, useState } from "react";
import { Search, X } from "lucide-react";

import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import type { AgentFace } from "@/features/viewer/lib/agent-list-view";
import { cn } from "@/lib/utils";
import type { AvatarPackView } from "@/tauri/commands";

/** The words a face is found by: its label, its id and its pack's name. */
function faceText(pack: AvatarPackView, face: AvatarPackView["faces"][number]): string {
  return `${face.label ?? ""} ${face.id} ${pack.name}`.toLowerCase();
}

/** A face's name in words. */
function faceName(face: AvatarPackView["faces"][number]): string {
  return face.label ?? face.id.replace(/-/g, " ");
}

/**
 * Every face of every pack (the team's, your own and the built-ins), as one grid you can
 * search, with Auto first: the face the agent would wear anyway. One choice, `pack:id`, or
 * null for Auto. Arrow keys move through the grid.
 */
export function FacePicker({
  packs,
  autoLabel = "Auto",
  autoDescription = "Its own face, or the team’s pick",
  value,
  autoFace,
  onChange,
  compact = false,
}: {
  packs: readonly AvatarPackView[];
  autoLabel?: string;
  autoDescription?: string;
  value: string | null;
  /** The face Auto stands for, drawn on its tile. */
  autoFace: AgentFace;
  onChange: (face: string | null) => void;
  /** Faces in cells of at most 40px (a page that is not the launch sheet's side panel). */
  compact?: boolean;
}): React.JSX.Element {
  const [query, setQuery] = useState("");
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const shown = useMemo(
    () =>
      packs
        .map((pack) => ({
          pack,
          faces: pack.faces.filter((f) => words.every((w) => faceText(pack, f).includes(w))),
        }))
        .filter((g) => g.faces.length > 0),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `words` derives from `query`
    [packs, query],
  );
  const count = shown.reduce((n, g) => n + g.faces.length, 0);

  // Arrow keys move between tiles in reading order; the grid is 5 wide.
  const onGridKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: 5, ArrowUp: -5 }[e.key];
    if (!step) return;
    const tiles = [...e.currentTarget.querySelectorAll<HTMLButtonElement>("[data-face-tile]")];
    const at = tiles.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return;
    e.preventDefault();
    tiles[Math.max(0, Math.min(tiles.length - 1, at + step))]?.focus();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <label className="ls-search mx-4 mb-2">
        <Search className="size-3.5 shrink-0" aria-hidden />
        <input
          type="search"
          value={query}
          placeholder="Find a face…"
          aria-label="Find a face"
          data-testid="launch-face-search"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.stopPropagation();
              setQuery("");
            }
          }}
        />
        {query && (
          <button
            type="button"
            aria-label="Clear face search"
            className="ls-search-clear"
            onClick={() => setQuery("")}
          >
            <X className="size-3" />
          </button>
        )}
      </label>
      <div
        role="radiogroup"
        aria-label="Face"
        className="ls-scroll min-h-0 flex-1 overflow-y-auto px-4 pb-4"
        onKeyDown={onGridKey}
      >
        {!query && (
          <button
            type="button"
            role="radio"
            aria-checked={value === null}
            data-face-tile
            data-testid="launch-face-auto"
            className="ls-auto"
            onClick={() => onChange(null)}
          >
            <AgentFaceView face={autoFace} size={30} />
            <span className="min-w-0 text-left">
              <span className="block text-[13px] font-medium leading-4">{autoLabel}</span>
              <span className="block truncate text-[11px] leading-[14px] text-muted-foreground">
                {autoDescription}
              </span>
            </span>
          </button>
        )}
        {shown.map(({ pack, faces }) => (
          <section key={pack.id} aria-label={pack.name} className="mt-3">
            <h3 className="ls-pack">
              <span className="truncate">{pack.name}</span>
              {pack.scope === "personal" && <span className="ls-pack-note">only you</span>}
            </h3>
            <div
              className={cn(
                "grid gap-1",
                compact ? "grid-cols-[repeat(auto-fill,40px)]" : "grid-cols-5",
              )}
            >
              {faces.map((f) => {
                const selected = value === f.ref;
                return (
                  <button
                    key={f.ref}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    aria-label={faceName(f)}
                    title={faceName(f)}
                    data-face-tile
                    data-testid={`launch-face-${f.ref}`}
                    className={cn(
                      "ls-face",
                      compact && "ls-face-compact",
                      selected && "ls-face-on",
                    )}
                    onClick={() => onChange(f.ref)}
                  >
                    <AgentFaceView face={{ kind: "image", ref: f.ref, src: f.src }} size={36} />
                  </button>
                );
              })}
            </div>
          </section>
        ))}
        {count === 0 && (
          <p className="mt-6 text-center text-[12px] text-muted-foreground">
            No face matches &ldquo;{query.trim()}&rdquo;.
          </p>
        )}
      </div>
    </div>
  );
}

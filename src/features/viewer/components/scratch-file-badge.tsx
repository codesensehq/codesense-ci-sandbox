// SPDX-License-Identifier: MIT
import { Globe, Lock } from "lucide-react";

import { cn } from "@/lib/utils";
import type { ScratchOverlayFile } from "@/tauri/commands";

/**
 * SLW-PT-20 — the per-file provenance a scratch overlay file carries wherever a
 * worktree file shows: "From private", "From private · all orgs" or "From
 * private · replaces team's". The words carry the meaning; the glyph only
 * reinforces it (lock = only you, globe = every org).
 */
export function scratchBadgeLabel(file: ScratchOverlayFile): string {
  if (file.origin === "allOrgs") return "From private · all orgs";
  if (file.replacesTeam) return "From private · replaces team's";
  return "From private";
}

/** The compact tree-row form: the full label rides `title`/`aria-label`. */
function shortLabel(file: ScratchOverlayFile): string {
  if (file.origin === "allOrgs") return "Private · all orgs";
  if (file.replacesTeam) return "Private · replaces";
  return "Private";
}

function Glyph({ file }: { file: ScratchOverlayFile }) {
  return file.origin === "allOrgs" ? (
    <Globe size={10} className="shrink-0" aria-hidden="true" />
  ) : (
    <Lock size={10} className="shrink-0" aria-hidden="true" />
  );
}

const PILL =
  "inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-px leading-none bg-muted text-muted-foreground";

/** The badge on a file-tree row. Nothing for a team file the user chose over scratch. */
export function ScratchTreeBadge({ file }: { file: ScratchOverlayFile }) {
  if (file.usingTeam) return null;
  const label = scratchBadgeLabel(file);
  return (
    <span
      data-testid="scratch-tree-badge"
      data-origin={file.origin}
      data-replaces-team={file.replacesTeam || undefined}
      title={label}
      aria-label={label}
      className={cn(PILL, "text-[10px]", file.replacesTeam && "ring-1 ring-inset ring-border")}
    >
      <Glyph file={file} />
      <span aria-hidden="true">{shortLabel(file)}</span>
    </span>
  );
}

/**
 * The file-header form: the full label, plus the one-click way back to the
 * team's file when this scratch file replaces one ("Use team's"), or back to
 * the scratch copy once the team's is in use ("Use mine").
 */
export function ScratchHeaderBadge({
  file,
  busy,
  onUseTeam,
}: {
  file: ScratchOverlayFile;
  busy: boolean;
  /** `true` = use the team's file here; `false` = back to the scratch file. */
  onUseTeam: (useTeam: boolean) => void;
}) {
  if (file.usingTeam) {
    return (
      <span data-testid="scratch-header-badge" className="flex shrink-0 items-center gap-1.5">
        <span className={cn(PILL, "text-[0.65rem]")}>
          Team&apos;s file · your private copy is set aside
        </span>
        <button
          type="button"
          data-testid="scratch-use-mine"
          disabled={busy}
          onClick={() => onUseTeam(false)}
          className="rounded px-1.5 py-0.5 text-[0.65rem] font-medium text-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40"
        >
          Use mine
        </button>
      </span>
    );
  }
  return (
    <span data-testid="scratch-header-badge" className="flex shrink-0 items-center gap-1.5">
      <span
        data-origin={file.origin}
        className={cn(PILL, "text-[0.65rem]", file.replacesTeam && "ring-1 ring-inset ring-border")}
      >
        <Glyph file={file} />
        {scratchBadgeLabel(file)}
      </span>
      {file.replacesTeam && (
        <button
          type="button"
          data-testid="scratch-use-team"
          disabled={busy}
          onClick={() => onUseTeam(true)}
          title="Show the team's file here instead. Your copy stays in Private."
          className="rounded px-1.5 py-0.5 text-[0.65rem] font-medium text-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40"
        >
          Use team&apos;s
        </button>
      )}
    </span>
  );
}

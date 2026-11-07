// SPDX-License-Identifier: MIT
import type { PublishFileBadge, PublishFileStatus } from "../lib/types";

/**
 * Shared status/rename chips + badge labels for a changed-file list, so the
 * Git Sync Workbench and the SLW-WD-2 source-diff overlay render the A/M/D/R status letter+color (a11y: never color
 * alone) and the mode/EOL/rename badges from ONE place — no second color map to
 * drift (RD-3 "one place it evolves", applied to status chips).
 *
 * Pure presentation, no data fetch: this module lives under `git-sync` but must
 * NOT import `@/tauri/commands` (the `no-direct-invoke.test.ts` guard) — it never
 * needs to.
 */

export const STATUS_CHIP: Record<PublishFileStatus, { letter: string; cssVar: string }> = {
  added: { letter: "A", cssVar: "--cs-status-added" },
  modified: { letter: "M", cssVar: "--cs-status-modified" },
  deleted: { letter: "D", cssVar: "--cs-status-deleted" },
};

/** The full human word for a changed-file status (the chip's expanded label).
 *  Shared so the redline toggle, the binary affordance, and the WD-2 source-diff
 *  reader don't each keep their own identical Added/Modified/Deleted map. */
export const STATUS_LABEL: Record<PublishFileStatus, string> = {
  added: "Added",
  modified: "Modified",
  deleted: "Deleted",
};

/** A one-letter status/rename chip (A/M/D/R), colored by a `--cs-status-*`
 *  token. Shared by the A/M/D status markers and the SW-GS-14 rename marker. */
export function Chip({ letter, cssVar, label }: { letter: string; cssVar: string; label: string }) {
  return (
    <span
      className="inline-flex w-4 shrink-0 justify-center rounded text-[0.65rem] font-semibold"
      style={{
        color: `var(${cssVar})`,
        backgroundColor: `color-mix(in srgb, var(${cssVar}) 16%, transparent)`,
      }}
      aria-label={label}
    >
      {letter}
    </span>
  );
}

export function StatusChip({ status }: { status: PublishFileStatus }) {
  const { letter, cssVar } = STATUS_CHIP[status];
  return <Chip letter={letter} cssVar={cssVar} label={status} />;
}

// SW-GS-14: a rename renders an R chip in place of the M chip. Status stays
// "modified" under the hood; the chip + the `← old` subtitle make the move
// legible at a glance.
export function RenameChip() {
  return <Chip letter="R" cssVar="--cs-status-renamed" label="renamed" />;
}

/** SW-GS: the human label for each per-file classification badge (mode/EOL/rename),
 *  shown as small trailing pills. */
export const BADGE_LABEL: Record<PublishFileBadge, string> = {
  "eol-only": "EOL only",
  mode: "mode",
  rename: "rename",
};

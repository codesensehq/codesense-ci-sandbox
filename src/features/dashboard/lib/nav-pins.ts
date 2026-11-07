// SPDX-License-Identifier: MIT
import { compareNavNames } from "./nav-ordering";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";

/**
 * SLW-NC-7: the pure merge/partition for the left-nav "Pinned" section.
 *
 * Mirrors `partitionNotes()` (`src/features/viewer/lib/note-ordering.ts`): a small,
 * pure, fully-tested function that turns the two pin sources into one ordered list
 * the renderer maps over. It owns exactly the load-bearing rules the story calls
 * out — team+personal merge, dedup-by-path (team wins), personal-additive (a
 * personal pin can never hide a team pin), stale-flagging, and natural-alphabetical
 * order — so the component stays a thin renderer.
 *
 * Key space: pins are **repo-relative** paths (so a git-shared team pin resolves
 * for every teammate regardless of their absolute clone location). Team and
 * personal pins and the `resolvable` map MUST all be keyed the same way; paths are
 * normalized here defensively so `"a/b/"`, `"/a/b"`, and `"a\\b"` never split into
 * duplicate pins.
 */

export type PinScope = "team" | "personal";

/** What the nav knows about a currently-present pinnable node. */
export interface NavPinMeta {
  /** Directory (reveal-only) vs file (reveal + open). */
  isDir: boolean;
  /** Display label (already extension-stripped by the caller for files). */
  label: string;
}

/** One row in the rendered Pinned section. */
export interface PinnedNavItem {
  /** Repo-relative path (normalized) — the stable identity + unpin key. */
  path: string;
  label: string;
  isDir: boolean;
  scope: PinScope;
  /** True when the path no longer resolves to a nav node (dimmed + "Remove pin"). */
  stale: boolean;
}

/**
 * Canonicalize a pin path — trim, forward-slash separators, strip leading/trailing
 * slashes — matching the Rust store's `normalize_pin_path` so team pins written by
 * the backend and personal pins written locally compare on identical shapes.
 */
export function normalizePinPath(path: string): string {
  return path.trim().replace(/\\/g, "/").replace(/^\/+/, "").replace(/\/+$/, "");
}

/** Best-effort display label for a STALE pin (its node is gone from the nav, so we
 *  derive from the path's last segment; strip a viewer extension so a stale file
 *  pin reads like a live one). */
function labelFromPath(path: string): string {
  const base = path.split("/").filter(Boolean).pop() ?? path;
  return stripViewerExtension(base);
}

/** Best-effort dir-vs-file guess for a STALE pin (its real node is gone, so `isDir`
 *  isn't known from the nav): a last segment with no extension reads as a dir. Only
 *  drives the stale row's icon — a live pin always uses the resolved `meta.isDir`. */
function looksLikeDirPath(path: string): boolean {
  const base = path.split("/").filter(Boolean).pop() ?? "";
  return !base.includes(".");
}

/**
 * Merge the git-shared team pins and the local personal pins into one ordered,
 * deduped, stale-aware list for the Pinned section.
 *
 * Rules:
 * - **Dedup by normalized path; team wins.** A path pinned both by the team and
 *   personally renders once, tagged `team` — this is the "a personal pin cannot
 *   hide a team pin" guarantee (AC-5): personal is purely additive.
 * - **Stale** = the path is not a key in `resolvable` (no matching nav node);
 *   rendered dimmed with a remove action, never silently dropped (AC-7).
 * - **Order** = natural-alphabetical by label, dirs and files interleaved (AC-3),
 *   path as the stable tiebreak.
 */
export function partitionPinnedNav(
  teamPins: string[],
  personalPins: string[],
  resolvable: Map<string, NavPinMeta>,
): PinnedNavItem[] {
  const byPath = new Map<string, PinnedNavItem>();

  const add = (rawPath: string, scope: PinScope) => {
    const path = normalizePinPath(rawPath);
    if (!path) return;
    const existing = byPath.get(path);
    // Team wins: never let a personal entry downgrade an already-team row, and
    // never duplicate. A team pin added after a personal one upgrades the scope.
    if (existing && (existing.scope === "team" || scope === "personal")) return;

    const meta = resolvable.get(path);
    byPath.set(path, {
      path,
      label: meta?.label ?? labelFromPath(path),
      // Live pin → the resolved type; stale pin → infer from the path so a stale
      // dir keeps a folder icon instead of guessing "file".
      isDir: meta?.isDir ?? looksLikeDirPath(path),
      scope,
      stale: meta === undefined,
    });
  };

  // Team first so its scope wins the dedup; personal is layered additively on top.
  for (const p of teamPins) add(p, "team");
  for (const p of personalPins) add(p, "personal");

  return [...byPath.values()].sort(
    (a, b) => compareNavNames(a.label, b.label) || a.path.localeCompare(b.path),
  );
}

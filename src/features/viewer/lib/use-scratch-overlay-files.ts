// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { listScratchOverlayFiles, type ScratchOverlayFile } from "@/tauri/commands";
import { SCRATCH_USE_CHANGED_EVENT, SYNC_WAVE_COMPLETED_EVENT } from "@/tauri/events";
import { logger } from "@/shared/lib/logger";

/** Worktree-relative path → the scratch overlay file there. */
export type ScratchOverlayIndex = ReadonlyMap<string, ScratchOverlayFile>;

const EMPTY: ScratchOverlayIndex = new Map();

/** The worktree-relative form of an absolute path under `root` (`null` outside it). */
export function relativeToRoot(root: string, absolute: string): string | null {
  const base = root.endsWith("/") ? root : `${root}/`;
  return absolute.startsWith(base) ? absolute.slice(base.length) : null;
}

/**
 * SLW-PT-20 — which files of the worktree at `cwd` the user's scratch overlay
 * materialized (or set aside for the team's). Read from what the worktree
 * actually holds (its lockfile), re-read after every sync wave and every
 * scratch-use change, so the "From scratch" badge never lags the files.
 * A blank `cwd` (worktree not ready) or a remote worktree reads empty.
 */
export function useScratchOverlayFiles(cwd: string): {
  files: ScratchOverlayIndex;
  refresh: () => void;
} {
  // Held with the cwd it was read for, so a switched or blanked worktree never
  // shows another worktree's badges (derived at render, no reset effect).
  const [read, setRead] = useState<{ cwd: string; files: ScratchOverlayIndex } | null>(null);
  const [nonce, setNonce] = useState(0);
  const refresh = useCallback(() => setNonce((n) => n + 1), []);

  useEffect(() => {
    if (cwd.trim() === "") return;
    let live = true;
    listScratchOverlayFiles(cwd)
      .then((list) => {
        if (live) setRead({ cwd, files: new Map(list.map((f) => [f.path, f])) });
      })
      .catch((err) => {
        if (!live) return;
        logger.warn("useScratchOverlayFiles", `scratch overlay read failed for ${cwd} — ${err}`);
        setRead({ cwd, files: EMPTY });
      });
    return () => {
      live = false;
    };
  }, [cwd, nonce]);

  useEffect(() => {
    if (cwd.trim() === "") return;
    // A missing event bridge (tests, a torn-down window) just means no live re-read.
    const unlisteners: Array<Promise<(() => void) | null>> = [
      listen(SYNC_WAVE_COMPLETED_EVENT, refresh).catch(() => null),
      listen(SCRATCH_USE_CHANGED_EVENT, refresh).catch(() => null),
    ];
    return () => {
      for (const u of unlisteners) void u.then((off) => off?.());
    };
  }, [cwd, refresh]);

  const files = read !== null && read.cwd === cwd && cwd.trim() !== "" ? read.files : EMPTY;
  return { files, refresh };
}

// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { readTeamKeyed, useViewerStore } from "@/shared/stores/viewer-store";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";

/**
 * Open a file in a tile's worktree code editor, optionally at a line — THE
 * entry point other tools use (the source diff's "Open in editor", a test
 * failure, an agent's file:line reference). Stable contract:
 *
 * ```ts
 * openInWorktreeEditor({
 *   sessionId,   // the terminal tab whose editor opens (the open flag is per tab)
 *   workspaceId, // the tile's workspace (tabs + drafts are per workspace)
 *   teamKey,     // the owning team (the store's sibling-overlay rules need it)
 *   path: "src/auth/session.ts", // worktree-relative, `/`-separated (an absolute
 *                                // path inside the worktree is accepted too)
 *   line: 42,    // optional, 1-based; clamped to the file
 *   column: 7,   // optional, 1-based; clamped to the line
 * });
 * ```
 *
 * Works whether the editor is closed (it opens on the file) or already open
 * (the file comes forward as a tab). The jump waits for the file to load —
 * however slow a remote machine is — and lands centred with the cursor
 * there. Opening the editor closes the diff on the same tab (the store's
 * overlay rule). A file the editor can't show (binary, too large) opens as
 * its placeholder and the line is ignored.
 */
export interface WorktreeEditorTarget {
  sessionId: string;
  workspaceId: string;
  teamKey: string;
  /** Worktree-relative path, `/`-separated, no leading slash. */
  path: string;
  /** 1-based line to reveal. Omitted: the file's remembered position. */
  line?: number;
  /** 1-based column on that line (default 1). */
  column?: number;
}

export function openInWorktreeEditor(target: WorktreeEditorTarget): void {
  // An absolute path inside the worktree (what a test failure or an agent's
  // `file:line` usually carries) opens as the worktree file it names.
  const root = worktreeRoot(target.teamKey, target.workspaceId);
  const relPath =
    root != null && target.path.startsWith(`${root}/`)
      ? target.path.slice(root.length + 1)
      : target.path;
  // One spelling per file: `./src/a.ts`, `src//a.ts` and `/src/a.ts` all open the
  // same tab as `src/a.ts`.
  const segments = relPath.split("/").filter((seg) => seg !== "" && seg !== ".");
  // Nothing to open, or a path that climbs out of the worktree: ignore it.
  if (segments.length === 0 || segments.includes("..")) return;
  const path = segments.join("/");
  const viewer = useViewerStore.getState();
  viewer.setWorktreeEditorFile(target.workspaceId, path);
  useWorktreeEditorRevealStore.getState().request(target.workspaceId, {
    path,
    line: positive(target.line),
    column: positive(target.column),
  });
  viewer.toggleWorktreeEditorOpen({
    sessionId: target.sessionId,
    workspaceId: target.workspaceId,
    teamKey: target.teamKey,
    open: true,
  });
}

/** The workspace's worktree directory (on its machine, for a remote one),
 *  without a trailing slash; `null` when unknown or the filesystem root. */
function worktreeRoot(teamKey: string, workspaceId: string): string | null {
  const s = useViewerStore.getState();
  const ws = readTeamKeyed(s.workspaces, teamKey, []).find((w) => w.id === workspaceId);
  if (!ws) return null;
  const dir = (isRemoteWorkspace(ws) ? ws.location.cwd : ws.cwd) ?? "";
  const trimmed = dir.replace(/\/+$/, "");
  return trimmed === "" ? null : trimmed;
}

function positive(n: number | undefined): number | null {
  return n != null && Number.isFinite(n) && n >= 1 ? Math.floor(n) : null;
}

/** A pending "bring this file forward (at this line)" for one workspace's
 *  editor. `nonce` makes a repeat request for the same spot distinct. */
export interface WorktreeEditorReveal {
  path: string;
  line: number | null;
  column: number | null;
  nonce: number;
}

interface RevealState {
  requests: Record<string, WorktreeEditorReveal>;
  request: (workspaceId: string, reveal: Omit<WorktreeEditorReveal, "nonce">) => void;
  /** The editor took the request; drop it unless a newer one replaced it. */
  consume: (workspaceId: string, nonce: number) => void;
}

let nextNonce = 1;

/** Runtime-only hand-off between {@link openInWorktreeEditor} and the
 *  mounted editor. Not persisted: a reveal is a one-shot instruction. */
export const useWorktreeEditorRevealStore = create<RevealState>()((set) => ({
  requests: {},
  request: (workspaceId, reveal) =>
    set((s) => ({
      requests: { ...s.requests, [workspaceId]: { ...reveal, nonce: nextNonce++ } },
    })),
  consume: (workspaceId, nonce) =>
    set((s) => {
      if (s.requests[workspaceId]?.nonce !== nonce) return s;
      const requests = { ...s.requests };
      delete requests[workspaceId];
      return { requests };
    }),
}));

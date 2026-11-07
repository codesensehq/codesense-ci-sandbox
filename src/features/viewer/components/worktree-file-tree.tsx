// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, Folder } from "lucide-react";

import { cn } from "@/lib/utils";
import type { FileEntry, ScratchOverlayFile } from "@/tauri/commands";
import { useRovingTreeNav, type RovingTreeRow } from "@/shared/hooks/use-roving-tree-nav";

import { useWorktreeChildrenByPath } from "../lib/use-worktree-children-by-path";
import { relativeToRoot, type ScratchOverlayIndex } from "../lib/use-scratch-overlay-files";
import { ScratchTreeBadge } from "./scratch-file-badge";
import { isNoiseDir } from "../lib/worktree-file-view";
import { FileGlyph } from "./worktree-editor/file-glyph";

export interface WorktreeFileSelection {
  /** Absolute path of the selected file. */
  path: string;
  /** Byte size from `list_directory` — drives the §B pre-read size gate. */
  size: number | null;
}

interface WorktreeFileTreeProps {
  /** Absolute worktree root (`ws.cwd`). */
  rootPath: string;
  /** Whether the root has been reported. Defaults to a non-blank `rootPath`;
   *  pass it where a real root can trim to "" (a remote worktree at "/"). */
  ready?: boolean;
  /** Absolute path of the file currently open in the view (highlighted), or null. */
  selectedPath: string | null;
  /** Fired when a FILE row is activated (click / Enter / Space). */
  onSelect: (file: WorktreeFileSelection) => void;
  /** SW-RM-UI-1: list through a remote machine's terminal server instead of the
   *  local `list_directory`. Must be stable (memoized). */
  listDirectory?: (dirPath: string) => Promise<FileEntry[]>;
  /** Bump to re-fetch every expanded directory (a manual / on-focus refresh). */
  refreshToken?: number;
  /** SLW-PT-20: the files the scratch overlay put here, keyed worktree-relative —
   *  each such row carries its "From scratch" badge. */
  scratchFiles?: ScratchOverlayIndex;
  /** SLW-PT-22: wrap a "From scratch" row (e.g. in its Share / Copy / Remove
   *  right-click menu). Not offered where the team's file is in use instead. */
  wrapScratchRow?: (file: ScratchOverlayFile, row: ReactNode) => ReactNode;
  /** Reveal a file: expand every folder above it and scroll its row into view.
   *  A new `nonce` asks again for the same path (the "reveal active file"
   *  button after the user collapsed a folder). */
  reveal?: { path: string; nonce: number } | null;
}

type RenderItem =
  | { type: "row"; entry: FileEntry; depth: number; treeIndex: number }
  | { type: "error"; path: string; depth: number };

/**
 * SLW-WDE-1 (AC-2) — a lazy, keyboard-navigable file tree rooted at the story
 * worktree (`ws.cwd`). Reuses `useRovingTreeNav` verbatim for arrow/Home/End/
 * expand-collapse and `useWorktreeChildrenByPath` for per-directory lazy loads.
 * Unlike the dashboard nav it lists the WHOLE worktree (no viewer-file filter,
 * §A); heavy dirs (`node_modules`/`target`/`dist`) are de-emphasized, never
 * hidden, and — being lazy — collapsed by default. Selecting a file fires
 * `onSelect(absolutePath)`; this component owns no read/view concern.
 */
export function WorktreeFileTree({
  rootPath,
  ready,
  selectedPath,
  onSelect,
  listDirectory,
  refreshToken = 0,
  scratchFiles,
  wrapScratchRow,
  reveal,
}: WorktreeFileTreeProps) {
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(() => new Set());
  const [refreshKey, setRefreshKey] = useState(0);

  // Reset expansion when the root (workspace) changes — a new tile starts clean.
  const [seenRoot, setSeenRoot] = useState(rootPath);
  if (seenRoot !== rootPath) {
    setSeenRoot(rootPath);
    setExpandedDirs(new Set());
  }

  // AC-4: a blank `ws.cwd` (worktree not materialized yet) is a not-ready
  // state, not an error. Guard BEFORE fetching so we never fire
  // `list_directory("")` — which the path sandbox would reject and surface as
  // the "Couldn't load the worktree." permission error. (A registered-but-empty
  // worktree dir still lists fine and shows the "Empty worktree" state below.)
  const notReady = ready === undefined ? rootPath.trim() === "" : !ready;

  // The root is always "expanded" so its children load; user toggles nest below.
  // When not ready we pass an empty set so no fetch is issued.
  const expandedPaths = useMemo(
    () => (notReady ? new Set<string>() : new Set<string>([rootPath, ...expandedDirs])),
    [notReady, rootPath, expandedDirs],
  );
  const { childrenByPath, erroredPaths } = useWorktreeChildrenByPath({
    expandedPaths,
    refreshKey: refreshKey + refreshToken,
    list: listDirectory,
  });

  // Reveal: open the folders above the file now; scroll once its row renders
  // (each folder lists lazily, so that can take a few round trips).
  const revealKey = reveal ? `${reveal.nonce}:${reveal.path}` : null;
  const [seenReveal, setSeenReveal] = useState<string | null>(null);
  if (revealKey !== seenReveal) {
    setSeenReveal(revealKey);
    const ancestors = reveal ? ancestorDirs(rootPath, reveal.path) : [];
    if (ancestors.some((a) => !expandedDirs.has(a))) {
      setExpandedDirs((prev) => new Set([...prev, ...ancestors]));
    }
  }
  const pendingScrollRef = useRef<string | null>(null);
  const revealPath = reveal?.path ?? null;
  useEffect(() => {
    pendingScrollRef.current = revealPath;
  }, [revealPath, revealKey]);

  const toggleDir = useCallback((absPath: string) => {
    setExpandedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(absPath)) next.delete(absPath);
      else next.add(absPath);
      return next;
    });
  }, []);

  // Flatten the visible tree into render items + the parallel roving-row model.
  // `treeIndex` is stamped only on real tree rows (error lines are non-focusable),
  // so `data-index` stays aligned with `rovingRows`.
  const { items, rovingRows, rowEntries } = useMemo(() => {
    const items: RenderItem[] = [];
    const rovingRows: RovingTreeRow[] = [];
    const rowEntries: FileEntry[] = [];
    const walk = (dirPath: string, depth: number) => {
      const children = childrenByPath.get(dirPath);
      if (children) {
        for (const entry of children) {
          const treeIndex = rovingRows.length;
          rovingRows.push({
            depth,
            expandable: entry.is_dir,
            expanded: expandedPaths.has(entry.path),
          });
          rowEntries.push(entry);
          items.push({ type: "row", entry, depth, treeIndex });
          if (entry.is_dir && expandedPaths.has(entry.path)) walk(entry.path, depth + 1);
        }
      } else if (dirPath !== rootPath && erroredPaths.has(dirPath)) {
        // A nested expanded dir whose listing failed → inline error line (AC-5).
        items.push({ type: "error", path: dirPath, depth });
      }
    };
    walk(rootPath, 0);
    return { items, rovingRows, rowEntries };
  }, [childrenByPath, expandedPaths, erroredPaths, rootPath]);

  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    const target = pendingScrollRef.current;
    if (!target) return;
    const row = listRef.current?.querySelector<HTMLElement>(
      `[data-testid="worktree-row-${CSS.escape(target)}"]`,
    );
    if (!row) return;
    pendingScrollRef.current = null;
    row.scrollIntoView?.({ block: "nearest" });
  });
  const focusRow = useCallback((index: number) => {
    listRef.current?.querySelector<HTMLElement>(`[data-index="${index}"]`)?.focus();
  }, []);
  const toggleAtIndex = useCallback(
    (index: number) => {
      const entry = rowEntries[index];
      if (entry?.is_dir) toggleDir(entry.path);
    },
    [rowEntries, toggleDir],
  );
  const { onKeyDown, onFocus, tabIndexFor } = useRovingTreeNav({
    rows: rovingRows,
    focusRow,
    toggle: toggleAtIndex,
  });

  const activate = (entry: FileEntry) => {
    if (entry.is_dir) toggleDir(entry.path);
    else onSelect({ path: entry.path, size: entry.size });
  };

  const rootLoaded = childrenByPath.has(rootPath);
  const rootErrored = erroredPaths.has(rootPath);

  // Root-level non-happy states (AC-5): skeleton while first-loading, an inline
  // error + Retry if the root listing failed, a quiet empty hint otherwise.
  // AC-4: not-ready (blank cwd) is a calm hint, never the error/Retry row.
  if (notReady) {
    return (
      <p data-testid="worktree-tree-not-ready" className="px-1 py-1 text-xs text-muted-foreground">
        Worktree isn’t ready yet.
      </p>
    );
  }
  if (rootErrored) {
    return (
      <div
        role="alert"
        data-testid="worktree-tree-error"
        className="flex items-center justify-between gap-2 rounded px-1 py-1 text-xs text-destructive"
      >
        <span className="min-w-0 truncate">Couldn’t load the worktree.</span>
        <button
          type="button"
          onClick={() => setRefreshKey((k) => k + 1)}
          className="shrink-0 rounded border border-border px-2 py-0.5 text-[0.7rem] font-medium text-foreground hover:bg-accent"
        >
          Retry
        </button>
      </div>
    );
  }
  if (!rootLoaded) {
    return (
      <div className="flex flex-col gap-0.5" aria-hidden data-testid="worktree-tree-skeleton">
        {/* Shaped like rows (glyph + name) so the tree doesn't jump in. */}
        {[46, 38, 58, 30, 52].map((w, i) => (
          <div key={i} className="flex h-6 items-center gap-1.5 px-1">
            <span className="size-3 shrink-0 rounded-sm bg-foreground/[0.07]" />
            <span
              className="h-2.5 animate-pulse rounded-sm bg-foreground/10"
              style={{ width: `${w}%`, animationDelay: `${i * 60}ms` }}
            />
          </div>
        ))}
      </div>
    );
  }
  if (items.length === 0) {
    return <p className="px-1 py-1 text-xs text-muted-foreground">Empty worktree</p>;
  }

  return (
    <ul
      ref={listRef}
      role="tree"
      aria-label="Worktree files"
      data-testid="worktree-tree"
      onKeyDown={onKeyDown}
      onFocus={onFocus}
      className="flex min-h-0 flex-col gap-0.5"
    >
      {items.map((item) => {
        if (item.type === "error") {
          return (
            <li
              key={`err:${item.path}`}
              role="alert"
              className="truncate px-1 py-0.5 text-[0.7rem] text-destructive"
              style={{ paddingLeft: `${item.depth * 12 + 20}px` }}
            >
              Couldn’t open this folder.
            </li>
          );
        }
        const { entry, depth, treeIndex } = item;
        const selected = entry.path === selectedPath;
        const noise = isNoiseDir(entry.name, entry.is_dir);
        const expanded = entry.is_dir && expandedPaths.has(entry.path);
        const rel = entry.is_dir ? null : relativeToRoot(rootPath, entry.path);
        const scratch = rel === null ? undefined : scratchFiles?.get(rel);
        const row = (
          <button
            type="button"
            role="treeitem"
            data-index={treeIndex}
            data-testid={`worktree-row-${entry.path}`}
            aria-selected={selected}
            aria-expanded={entry.is_dir ? expanded : undefined}
            tabIndex={tabIndexFor(treeIndex)}
            onClick={() => activate(entry)}
            style={{ paddingLeft: `${depth * 12 + 4}px` }}
            className={cn(
              "flex h-6 w-full min-w-0 items-center gap-1 rounded-md px-1 text-left text-xs text-foreground/90 hover:bg-muted",
              "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
              // The open file reads as selected without shouting: a tint of the
              // accent, not a solid fill that out-weighs the editor beside it.
              selected &&
                "bg-[color-mix(in_srgb,var(--accent)_16%,transparent)] text-foreground hover:bg-[color-mix(in_srgb,var(--accent)_22%,transparent)]",
              noise && "opacity-60",
            )}
          >
            {entry.is_dir ? (
              <>
                {expanded ? (
                  <ChevronDown size={13} className="shrink-0 text-muted-foreground" aria-hidden />
                ) : (
                  <ChevronRight size={13} className="shrink-0 text-muted-foreground" aria-hidden />
                )}
                <Folder size={13} className="shrink-0 text-muted-foreground" aria-hidden />
              </>
            ) : (
              <>
                <span className="w-[13px] shrink-0" aria-hidden />
                <FileGlyph path={entry.path} className="size-[13px]" />
              </>
            )}
            <span className="min-w-0 flex-1 truncate" title={entry.name}>
              {entry.name}
            </span>
            {scratch && <ScratchTreeBadge file={scratch} />}
          </button>
        );
        return (
          <li key={entry.path} role="none">
            {scratch && !scratch.usingTeam && wrapScratchRow ? wrapScratchRow(scratch, row) : row}
          </li>
        );
      })}
    </ul>
  );
}

/** The folders between `rootPath` and `path`, nearest the root first; empty
 *  for a path outside the root. */
function ancestorDirs(rootPath: string, path: string): string[] {
  if (!path.startsWith(`${rootPath}/`)) return [];
  const out: string[] = [];
  let at = path.indexOf("/", rootPath.length + 1);
  while (at !== -1) {
    out.push(path.slice(0, at));
    at = path.indexOf("/", at + 1);
  }
  return out;
}

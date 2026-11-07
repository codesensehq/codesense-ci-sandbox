// SPDX-License-Identifier: MIT
import { useContentSource } from "@/shared/hooks/use-content-source";
import { beginContextNavigation } from "@/shared/lib/context-navigation";
// ---------------------------------------------------------------------------
// RF-12: the pipeline-status-nav FILE-ACTION subsystem, relocated verbatim from
// `pipeline-status-nav.tsx` (no behavior change). Owns the dialog-target state
// (delete/rename/new-file/move/ingest) plus every mutating handler
// and the derived values the context menu + dialogs read. The orchestrator keeps
// owning the context-menu OPEN state (`contextMenu`/`setContextMenu`), the pins
// subsystem, and the virtual list — those are passed in.
// ---------------------------------------------------------------------------
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { createFile, createScratchFile, readFile, writeContentFile } from "@/tauri/commands";
import { markContentPathChanged } from "@/shared/stores/file-refresh-store";
import {
  baseName,
  createContent,
  createContentUnique,
  deleteContent,
  ingestFile,
  ingestFileUnique,
  moveContentTo,
  renameContent,
} from "@/features/dashboard/lib/structural-ops";
import { dirname } from "@/shared/lib/basename";
import { ONLY_ME, resolveAudience, type AudienceTeam } from "@/shared/components/who-can-use-it";
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useScratchTeamUseState } from "@/shared/stores/scratch-team-use-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { DerivedPipelineTree, StoryRef } from "@/shared/lib/pipeline-graph/types";
import { canExportCanvas, createCanvasForDoc } from "@/features/dashboard/lib/create-canvas";
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import { findEnclosingEpic } from "@/shared/lib/pipeline-graph/find-enclosing-epic";
import {
  collectNavDirs,
  findDocInTree,
  findScopeInTree,
  folderHasOwnFiles,
  inheritedFilesUnder,
  isOwnTeamFileInTree,
  navNodeKind,
} from "@/features/dashboard/lib/file-scope";
import { copyIntoTeam, parentMoveDir, teamCopyPath } from "@/features/dashboard/lib/inherited-file";
import { useAttachCanvasToStory } from "@/features/dashboard/lib/use-attach-canvas-to-story";
import type { QuickPickerItem } from "@/features/viewer/components/quick-open-dialog";
import type { NavContextMenuState } from "./types";

interface UseNavFileActionsArgs {
  repoPath: string;
  teamKey: string;
  tree: DerivedPipelineTree;
  activeTeamName: string | null;
  onFileSelect: (filePath: string) => void;
  contextMenu: NavContextMenuState | null;
  setContextMenu: (v: NavContextMenuState | null) => void;
  rootAnchor: string;
  toRel: (abs: string) => string | null;
}

export function useNavFileActions({
  repoPath,
  teamKey,
  tree,
  activeTeamName,
  onFileSelect,
  contextMenu,
  setContextMenu,
  rootAnchor,
  toRel,
}: UseNavFileActionsArgs) {
  const handleCopyPath = useCallback(async () => {
    const path = contextMenu?.path;
    setContextMenu(null);
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      toast.success("Path copied");
    } catch {
      toast.error("Failed to copy path");
    }
  }, [contextMenu, setContextMenu]);

  // SW-CP-14: structural content gestures (delete + rename). The op runs
  // through the propagating content commands; on success we bump this repo's
  // refresh key for an immediate re-scan. Orchestration + path logic
  // live in `structural-ops.ts`.
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [renameTarget, setRenameTarget] = useState<string | null>(null);
  const [renameBusy, setRenameBusy] = useState(false);
  // SW-VE-6: create + move gestures (extending SW-CP-14's delete/rename). New
  // file targets a DIRECTORY; move targets a FILE and opens a destination-dir
  // picker. Collisions surface a Replace confirm; a cross-scope move surfaces a
  // "changes which team owns this file" warning. All writes route through the gated
  // `create_file`/`move_content_file` commands via `structural-ops.ts`.
  const [newFileDir, setNewFileDirState] = useState<string | null>(null);
  const [newFileBusy, setNewFileBusy] = useState(false);
  // SLW-PT-21: "Who can use it" — the team (default) or only me (the scratch space).
  const [newFileChoice, setNewFileAudience] = useState<string | null>(null);
  const newFileTeams: AudienceTeam[] = useMemo(
    () => (activeTeamName ? [{ value: "team", label: activeTeamName }] : []),
    [activeTeamName],
  );
  // The team unless the user picked Only me (or there is no team).
  const newFileAudience = resolveAudience(newFileChoice, newFileTeams);
  // "Use my private items in ‹Team›" off: an Only me file won't apply in this team.
  const scratchOrg = useOrgStore((s) => s.activeOrgSlug);
  const scratchOn = useScratchTeamUseState(scratchOrg, activeTeamName);
  const newFileScratchOffFor = scratchOn === false ? activeTeamName : null;
  const cloneContentPath = useContentSource(repoPath)?.teamContentPath;
  const privateSource = useViewerStore((s) => s.navLens[teamKey]?.source?.kind === "private");
  const setNewFileDir = useCallback(
    (dir: string | null) => {
      setNewFileAudience(privateSource ? ONLY_ME : null);
      setNewFileDirState(dir);
    },
    [privateSource],
  );
  // SW-XS-20: "New file…" from a row. A file row targets its folder, a folder row
  // itself — always the ACTIVE team's copy of that folder. A row inherited from a
  // parent team lives in the parent's folder, so it maps to the same folder in
  // the active team (created on write); Only me then gets the team's folder too.
  const newFileFromRow = useCallback(
    (path: string) => {
      const kind = navNodeKind(tree, path);
      const isFile = kind === "file" || (kind === null && /\.[^/]+$/.test(path));
      if (!isFile) {
        setNewFileDir(path);
        return;
      }
      const team = parentTeamOf(tree, path, activeTeamName);
      if (!team) {
        setNewFileDir(dirname(path));
        return;
      }
      const copy = teamCopyPath(path, team, rootAnchor);
      if (!copy) {
        toast.error(`Couldn't find that folder in ${activeTeamName ?? "the team"}'s content.`);
        return;
      }
      setNewFileDir(dirname(copy));
    },
    [tree, activeTeamName, rootAnchor, setNewFileDir],
  );
  const [newFileCollision, setNewFileCollision] = useState<{ dir: string; name: string } | null>(
    null,
  );
  const [moveTarget, setMoveTarget] = useState<string | null>(null);
  const [moveBusy, setMoveBusy] = useState(false);
  // SW-XS-20: a menu move of a file inherited from a parent team stays in that
  // team (`toDir` is the matching folder in the parent's content) — confirmed
  // first, since it moves the file for every team that inherits it.
  const [moveCrossScope, setMoveCrossScope] = useState<{
    from: string;
    toDir: string;
    team: string;
  } | null>(null);
  // SW-VE-7: drag-drop OS-file ingest. `ingestQueue` holds the name collisions a
  // drop produced (usually 0–1); the head drives ONE non-destructive "Keep both"
  // dialog and is shifted as it's resolved/cancelled, so multi-file drops don't
  // stack modals. `ingestBusyRef` (kept in sync below) suppresses a second drop
  // while a collision dialog is open — read live by the once-on-mount listener.
  const [ingestQueue, setIngestQueue] = useState<{ src: string; dir: string; name: string }[]>([]);
  const [ingestBusy, setIngestBusy] = useState(false);
  // Synced post-commit so the once-on-mount drop listener reads it live (a
  // ref-write during render trips react-hooks). Suppresses a second drop while
  // a collision dialog is open / an ingest is in flight.
  const ingestBusyRef = useRef(false);
  useEffect(() => {
    ingestBusyRef.current = ingestQueue.length > 0 || ingestBusy;
  }, [ingestQueue, ingestBusy]);
  const confirmDelete = useCallback(async () => {
    if (!deleteTarget) return;
    beginContextNavigation();
    setDeleteBusy(true);
    const result = await deleteContent(deleteTarget);
    setDeleteBusy(false);
    if (result.ok) {
      toast.success("Deleted");
      markContentPathChanged(repoPath, deleteTarget);
      // SW-VE-6 (AC-3): the deleted file's open tab closes to empty state
      // (no-op when it wasn't open). Self-heals: the nav refresh above
      // reconciles the row independently.
      useViewerStore.getState().closeTab(teamKey, repoPath, deleteTarget);
    } else {
      toast.error(`Couldn't delete: ${result.error}`);
    }
    setDeleteTarget(null);
  }, [deleteTarget, repoPath, teamKey]);

  const confirmRename = useCallback(
    async (newName: string) => {
      if (!renameTarget) return;
      setRenameBusy(true);
      const result = await renameContent(renameTarget, newName);
      setRenameBusy(false);
      if (result.ok) {
        toast.success("Renamed");
        markContentPathChanged(repoPath, result.newPath ?? renameTarget);
        // SW-VE-6 (AC-2): the open tab follows the rename rather than stranding.
        if (result.newPath) {
          useViewerStore.getState().remapTabPath(teamKey, renameTarget, result.newPath);
        }
      } else {
        toast.error(`Couldn't rename: ${result.error}`);
      }
      setRenameTarget(null);
    },
    [renameTarget, repoPath, teamKey],
  );

  // SW-VE-6 (AC-1): create a new empty file in the target dir; open it read-only
  // on success, or surface a Replace/Cancel resolution on a name collision.
  const confirmNewFile = useCallback(
    async (name: string) => {
      const chosenDir = newFileDir;
      if (!chosenDir) return;
      // Choosing a team explicitly maps this private folder to that team's copy.
      const dir =
        privateSource &&
        newFileAudience !== ONLY_ME &&
        cloneContentPath &&
        chosenDir.startsWith(rootAnchor)
          ? `${cloneContentPath}${chosenDir.slice(rootAnchor.length)}`
          : chosenDir;
      if (newFileAudience === ONLY_ME) {
        // Only me: the file goes in the scratch space at this folder's place in the
        // team, never into the team's own files.
        const orgSlug = useOrgStore.getState().activeOrgSlug ?? parseTeamKey(teamKey).orgSlug;
        const team = activeTeamName ?? parseTeamKey(teamKey).teamName;
        setNewFileBusy(true);
        try {
          if (!orgSlug || !team) throw new Error("open a team first");
          const path = await createScratchFile(orgSlug, team, dir, name);
          markContentPathChanged(repoPath, path);
          setNewFileDir(null);
          const saved = baseName(path);
          const as = saved === name ? "" : ` as '${saved}'`;
          toast.success(
            newFileScratchOffFor
              ? `Saved to Private${as} — private items are off for ${newFileScratchOffFor}, so it won't apply there`
              : `Saved to Private${as} — only you see it`,
          );
          openManagementTab(path);
        } catch (e) {
          toast.error(`Couldn't create: ${e instanceof Error ? e.message : String(e)}`);
        } finally {
          setNewFileBusy(false);
        }
        return;
      }
      setNewFileBusy(true);
      const result = await createContent(dir, name);
      setNewFileBusy(false);
      setNewFileDir(null);
      if (result.ok) {
        toast.success("File created");
        markContentPathChanged(repoPath, result.path ?? dir);
        if (result.path) onFileSelect(result.path); // opens read-only (not edit mode)
      } else if (result.collision) {
        setNewFileCollision({ dir, name }); // → Replace / Cancel
      } else {
        toast.error(`Couldn't create: ${result.error}`);
      }
    },
    [
      newFileDir,
      privateSource,
      cloneContentPath,
      rootAnchor,
      newFileAudience,
      newFileScratchOffFor,
      teamKey,
      activeTeamName,
      setNewFileDir,
      repoPath,
      onFileSelect,
    ],
  );

  // SW-VE-6 (AC-1): NON-destructive collision resolution — "Keep both" creates a
  // numbered copy (`name-2.md`) and never deletes/overwrites the existing file.
  // This avoids the destructive-Replace hazards entirely (no data-loss window,
  // no worktree-fan-out divergence): to actually replace, the user Deletes the
  // old file explicitly (Delete is right here in the menu).
  const confirmKeepBothNewFile = useCallback(async () => {
    const c = newFileCollision;
    if (!c) return;
    setNewFileCollision(null);
    setNewFileBusy(true);
    const result = await createContentUnique(c.dir, c.name);
    setNewFileBusy(false);
    if (result.ok) {
      toast.success("File created");
      markContentPathChanged(repoPath, result.path ?? c.dir);
      if (result.path) onFileSelect(result.path);
    } else {
      toast.error(`Couldn't create: ${result.error}`);
    }
  }, [newFileCollision, repoPath, onFileSelect]);

  // SW-VE-6 (AC-2): the actual move, shared by the same-scope and cross-scope
  // paths. A same-scope move keeps the file in the visible tree, so the open tab
  // FOLLOWS it (`remapTabPath`). A cross-scope move takes it out of the current
  // view scope, so the tab CLOSES with a notice rather than stranding on a path
  // the visible tree no longer holds (`outOfView`).
  const performMove = useCallback(
    async (from: string, toDir: string, outOfView = false) => {
      beginContextNavigation();
      setMoveBusy(true);
      const result = await moveContentTo(from, toDir);
      setMoveBusy(false);
      if (result.ok) {
        markContentPathChanged(repoPath, result.newPath ?? from);
        if (outOfView) {
          // closeTab is a no-op when the file wasn't open, so keep the toast
          // neutral rather than asserting a tab was closed.
          useViewerStore.getState().closeTab(teamKey, repoPath, from);
          toast.success("Moved to another scope");
        } else {
          toast.success("Moved");
          if (result.newPath) useViewerStore.getState().remapTabPath(teamKey, from, result.newPath);
        }
      } else if (result.collision) {
        toast.error("A file with that name already exists there — pick another destination");
      } else {
        toast.error(`Couldn't move: ${result.error}`);
      }
    },
    [repoPath, teamKey],
  );

  // SW-SC-9: create a blank sibling canvas for the right-clicked doc, open it,
  // and attach it to the doc's `supporting_docs:` frontmatter — one gesture.
  // Fire-and-forget from the menu's onClick; the orchestration (collision
  // suffix, non-blocking create→open→attach order) lives in `createCanvasForDoc`.
  const handleCreateCanvas = useCallback(
    async (docPath: string) => {
      const result = await createCanvasForDoc(docPath, {
        createFile,
        readFile,
        writeContentFile,
        onOpen: onFileSelect,
      });
      if (!result.ok) {
        toast.error(
          result.reason === "name-exhausted"
            ? "Couldn't create the canvas — too many name collisions."
            : "Couldn't create the canvas.",
        );
        return;
      }
      if (result.attach === "failed") {
        toast.error("Canvas created, but couldn't attach it to the doc.");
      }
    },
    [onFileSelect],
  );

  // SW-SC-12: candidate stories for the right-clicked `.excalidraw` canvas —
  // the enclosing epic's full set, resolved from the already-derived `tree`
  // (a pure in-memory walk, no disk re-scan — AC-6). `null` when the context
  // target isn't a canvas node, so "Attach to story…" is hidden for docs,
  // folders, generic files, and `*.excalidraw.png` derived renders.
  const attachCanvas = useMemo<{ path: string; stories: StoryRef[] } | null>(() => {
    if (!contextMenu) return null;
    // SW-SC-13: the canvas-node gate is the shared `canExportCanvas` helper,
    // so "Attach to story…" and "Export as PNG/SVG" agree by construction on
    // what counts as a canvas (`.excalidraw`, excluding the derived render).
    if (!canExportCanvas(contextMenu.path)) {
      return null;
    }
    return {
      path: contextMenu.path,
      stories: collectAllStories(findEnclosingEpic(tree, contextMenu.path)),
    };
  }, [contextMenu, tree]);

  // The side-effecting half of the attach flow (idempotent write + picker
  // open-state). Resolution above is pure; the only I/O happens on a pick.
  const canvasAttach = useAttachCanvasToStory();

  // SW-VE-6 (AC-3): deleting one of the team's own docs or stories carries extra
  // caution (its agents may depend on it). A delete changes team content only
  // (workspaces keep their copies until an agent hydrates); every delete is
  // git-revertible. SW-XS-20: own files carry no `sourceTeam`, so this asks the
  // tree whether the path is a team file.
  const deleteMaterialized = useMemo(
    () => (deleteTarget ? isOwnTeamFileInTree(tree, deleteTarget, activeTeamName) : false),
    [deleteTarget, tree, activeTeamName],
  );

  // SW-XS-20: folder rename/delete act on the active team's files only; files
  // there that come from a parent team are left untouched, and the confirm says
  // so. `null` for a file target or a folder with no inherited files.
  const deleteKeepsInherited = useMemo(
    () => (deleteTarget ? keptInheritedNote(tree, deleteTarget, activeTeamName) : null),
    [deleteTarget, tree, activeTeamName],
  );
  const renameKeepsInherited = useMemo(
    () => (renameTarget ? keptInheritedNote(tree, renameTarget, activeTeamName) : null),
    [renameTarget, tree, activeTeamName],
  );
  // SW-XS-20: a FILE from a parent team is deleted/renamed in that team — for
  // it and every team that inherits from it. The parent team's name, or `null`.
  // SW-XS-20: deleting the team's own file that overrides a parent's file brings
  // the parent's version back into view. That parent team, or `null`.
  const deleteRevealsParent = useMemo(
    () => (deleteTarget ? (findDocInTree(tree, deleteTarget)?.overridesTeam ?? null) : null),
    [deleteTarget, tree],
  );
  const deleteFromParent = useMemo(
    () => (deleteTarget ? parentTeamOf(tree, deleteTarget, activeTeamName) : null),
    [deleteTarget, tree, activeTeamName],
  );
  const renameFromParent = useMemo(
    () => (renameTarget ? parentTeamOf(tree, renameTarget, activeTeamName) : null),
    [renameTarget, tree, activeTeamName],
  );

  // SW-VE-6: scope of the right-clicked row. SW-XS-20: a file from a parent
  // team keeps every gesture (they act on the parent's file); the menu labels
  // it. Pure O(depth) walk of the resident tree — no command, no I/O.
  const menuScope = useMemo(
    () =>
      contextMenu && !contextMenu.pin
        ? findScopeInTree(tree, contextMenu.path, activeTeamName)
        : null,
    [contextMenu, tree, activeTeamName],
  );
  const menuInherited = !!menuScope?.isInherited;
  // SW-XS-20: the right-clicked row's Move/Rename/Delete. A file row always has
  // them. A folder row keeps the old dotted-name rule (a dir header isn't
  // casually moved via right-click), read from the tree's node kind, and only
  // when the folder holds some of the team's own files — a folder only a parent
  // has has nothing of the team's to act on.
  // The right-clicked row's kind from the tree ("file" / "dir"), or null when
  // the tree doesn't know it (the starter page item then judges by its name).
  const menuNodeKind = useMemo(() => {
    if (!contextMenu || contextMenu.pin) return null;
    const kind = navNodeKind(tree, contextMenu.path);
    return kind === "file" || kind === "dir" ? kind : null;
  }, [contextMenu, tree]);

  const menuFileGestures = useMemo(() => {
    if (!contextMenu || contextMenu.pin) return false;
    const path = contextMenu.path;
    const kind = navNodeKind(tree, path);
    const dotted = /\.[^/]+$/.test(path);
    if (kind === "file") return true;
    if (kind === "dir") return dotted && folderHasOwnFiles(tree, path, activeTeamName);
    return dotted;
  }, [contextMenu, tree, activeTeamName]);

  // SW-VE-6 (AC-2): destination directories for the Move-to picker.
  const moveDirs = useMemo(() => (moveTarget ? collectNavDirs(tree) : []), [moveTarget, tree]);
  const moveDirItems = useMemo<QuickPickerItem[]>(() => {
    if (!moveTarget) return [];
    const fromDir = dirname(moveTarget);
    return moveDirs
      .filter(
        // exclude the current dir and browse-only installed (`installed://…`)
        // trees — the backend rejects a move into a non-content destination.
        (d) => d && d !== fromDir && !d.startsWith("installed://"),
      )
      .map((d) => ({ id: d, label: baseName(d), sublabel: toRel(d) || d }));
  }, [moveTarget, moveDirs, toRel]);

  // SW-VE-6/7: begin a move. Shared by the Move-to picker (VE-6) and the
  // drag-drop move (VE-7) so the rule lives in one place. SW-XS-20: folders are
  // shared across the team chain. An own file moves into the active team's copy
  // of the destination folder (created on write). A file inherited from a parent
  // team (menu only — it is never dragged) moves within that parent, into the
  // parent's matching folder, after a confirm naming the parent. A same-dir move
  // is a no-op. Either way only team content changes; workspaces keep their
  // copies until an agent hydrates.
  const beginMove = useCallback(
    (from: string, toDir: string) => {
      if (dirname(from) === toDir) return;
      const team = parentTeamOf(tree, from, activeTeamName);
      if (team) {
        const toDirRel = toRel(toDir);
        const parentDir =
          toDirRel === null ? null : parentMoveDir(from, team, toDirRel, rootAnchor);
        if (!parentDir) {
          toast.error(
            `Couldn't move it: pick a folder in ${activeTeamName ?? "the team"}'s content.`,
          );
          return;
        }
        if (parentDir !== dirname(from)) setMoveCrossScope({ from, toDir: parentDir, team });
        return;
      }
      void performMove(from, toDir);
    },
    [tree, activeTeamName, performMove, toRel, rootAnchor],
  );

  // SW-XS-20: "Copy into ‹team›" — copy a file inherited from a parent team to the
  // same path in the active team's content. The team's copy then wins for the
  // team and its agents; it opens. Refused (never overwritten) when the team
  // already has that path.
  const copyIntoActiveTeam = useCallback(
    async (filePath: string) => {
      const team = parentTeamOf(tree, filePath, activeTeamName);
      if (!team || !activeTeamName) return;
      const r = await copyIntoTeam(filePath, team, rootAnchor, activeTeamName);
      if (r.ok) {
        toast.success(`Copied into ${activeTeamName}`);
        markContentPathChanged(repoPath, r.path);
        onFileSelect(r.path);
      } else {
        toast.error(r.error);
      }
    },
    [tree, activeTeamName, rootAnchor, repoPath, onFileSelect],
  );

  // SW-VE-6 (AC-2): the Move-to picker delegates to the shared move flow.
  const confirmMovePick = useCallback(
    (destDir: string) => {
      const from = moveTarget;
      setMoveTarget(null);
      if (from) beginMove(from, destDir);
    },
    [moveTarget, beginMove],
  );

  // SW-VE-7 (AC-1/AC-4): ingest each dropped OS file into `dir` via the gated
  // server-side copy. A name collision queues the non-destructive Keep-both
  // dialog; a single successful ingest opens read-only (mirrors New file).
  const runIngest = useCallback(
    async (dir: string, srcPaths: string[]) => {
      const collisions: { src: string; dir: string; name: string }[] = [];
      let added = 0;
      let lastPath: string | null = null;
      for (const src of srcPaths) {
        const name = baseName(src);
        const r = await ingestFile(src, dir, name);
        if (r.ok) {
          added++;
          lastPath = r.path ?? null;
        } else if (r.collision) {
          collisions.push({ src, dir, name });
        } else {
          toast.error(`Couldn't add ${name}: ${r.error}`);
        }
      }
      if (added > 0) {
        toast.success(added === 1 ? "File added" : `${added} files added`);
        markContentPathChanged(repoPath, lastPath ?? dir);
        // Open a single ingested file read-only (not a batch — too noisy). Keyed
        // on the SUCCESS count, so a 2-file drop where one collides still opens
        // the one that landed.
        if (added === 1 && lastPath) onFileSelect(lastPath);
      }
      if (collisions.length > 0) setIngestQueue((q) => [...q, ...collisions]);
    },
    [repoPath, onFileSelect],
  );

  // SW-VE-7 (AC-1/AC-2/AC-5): a dropped OS file lands in a directory row (or
  // the nav root zone → the anchored content root). SW-XS-20: every folder row
  // is the active team's place in the merged tree — a folder only a parent team
  // has sits at the active team's path — so a drop lands in the active team's
  // copy of that folder (the ingest creates it when needed).
  const handleOsDrop = useCallback(
    (target: { dir: string } | { root: true }, paths: string[]) => {
      const dir = "dir" in target ? target.dir : rootAnchor;
      void runIngest(dir, paths);
    },
    [rootAnchor, runIngest],
  );

  // SW-VE-7 (AC-4): resolve the head collision non-destructively (Keep both),
  // then advance the queue by exactly one. `ingestConfirmInFlight` is a
  // SYNCHRONOUS re-entrancy guard (the `loading`-disabled button only commits on
  // the next render, so a fast double-click / Enter-repeat could otherwise fire
  // twice — ingesting the head twice AND slicing twice, silently dropping the
  // next queued collision).
  const ingestConfirmInFlight = useRef(false);
  const confirmKeepBothIngest = useCallback(async () => {
    if (ingestConfirmInFlight.current) return;
    const head = ingestQueue[0];
    if (!head) return;
    ingestConfirmInFlight.current = true;
    setIngestBusy(true);
    try {
      const r = await ingestFileUnique(head.src, head.dir, head.name);
      setIngestQueue((q) => q.slice(1));
      if (r.ok) {
        toast.success("Added a copy");
        markContentPathChanged(repoPath, r.path ?? head.dir);
      } else {
        toast.error(`Couldn't add: ${r.error}`);
      }
    } finally {
      setIngestBusy(false);
      ingestConfirmInFlight.current = false;
    }
  }, [ingestQueue, repoPath]);

  // SW-VE-7 (AC-3): an INTERNAL drag of a listed file onto a directory row is a
  // MOVE — the HTML5-channel twin of the OS-drop ingest above (the two never
  // cross: separate drag channels), delegating to the same `beginMove` flow as
  // the Move-to menu. `NavRowDnd` calls this as `(toDir, draggedFrom)`.
  const handleMoveDrop = useCallback(
    (toDir: string, from: string) => beginMove(from, toDir),
    [beginMove],
  );

  return {
    // Delete
    deleteTarget,
    setDeleteTarget,
    deleteBusy,
    deleteMaterialized,
    deleteKeepsInherited,
    deleteFromParent,
    deleteRevealsParent,
    confirmDelete,
    // Rename
    renameTarget,
    setRenameTarget,
    renameBusy,
    renameKeepsInherited,
    renameFromParent,
    confirmRename,
    // New file
    newFileDir,
    setNewFileDir,
    newFileAudience,
    setNewFileAudience,
    newFileTeams,
    newFileScratchOffFor,
    newFileBusy,
    confirmNewFile,
    newFileCollision,
    setNewFileCollision,
    confirmKeepBothNewFile,
    // Move
    moveTarget,
    setMoveTarget,
    moveBusy,
    moveCrossScope,
    setMoveCrossScope,
    moveDirItems,
    performMove,
    beginMove,
    copyIntoActiveTeam,
    confirmMovePick,
    handleMoveDrop,
    // Ingest (OS drop)
    ingestQueue,
    setIngestQueue,
    ingestBusy,
    ingestBusyRef,
    handleOsDrop,
    confirmKeepBothIngest,
    // Canvas
    attachCanvas,
    canvasAttach,
    handleCreateCanvas,
    // Menu-scope gating
    menuScope,
    menuInherited,
    menuFileGestures,
    menuNodeKind,
    newFileFromRow,
    activeTeamName,
    // Misc
    handleCopyPath,
  };
}

/** SW-XS-20: the confirm note for a folder rename/delete when the folder shows
 *  files from parent teams — those live in the parent and are left untouched. */
function keptInheritedNote(
  tree: DerivedPipelineTree,
  dirPath: string,
  activeTeam: string | null,
): string | null {
  const groups = inheritedFilesUnder(tree, dirPath, activeTeam);
  if (groups.length === 0) return null;
  const parts = groups.map(
    ({ team, count }) => `${count} ${count === 1 ? "file" : "files"} here come from ${team}`,
  );
  return `${parts.join("; ")}; they're not changed.`;
}

/** SW-XS-20: the parent team a FILE comes from, or `null` for an own file. */
function parentTeamOf(
  tree: DerivedPipelineTree,
  filePath: string,
  activeTeam: string | null,
): string | null {
  const scope = findScopeInTree(tree, filePath, activeTeam);
  return scope?.isInherited ? (scope.sourceTeam ?? null) : null;
}

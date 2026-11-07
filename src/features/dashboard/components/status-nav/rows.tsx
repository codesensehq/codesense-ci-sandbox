// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// RF-12: the pipeline-status-nav ROW LAYER, relocated verbatim from
// `pipeline-status-nav.tsx` (no behavior change). Owns the row-dispatch
// (`renderRow` + `RowInteractionModel`), the stable-key helper, the drag-to-link
// wrapper (`NavRowDnd`), and every memoized row variant + shared affordance.
// ---------------------------------------------------------------------------
import { cn } from "@/lib/utils";
import { memo, useCallback, useMemo, useState } from "react";
import {
  ChevronRight,
  ChevronDown,
  MoreHorizontal,
  SquareTerminal,
  Pin,
  PinOff,
  Users,
  Layers,
  AlertTriangle,
  GitMerge,
  Link2,
  FolderInput,
  House,
} from "lucide-react";
import { type FileEntry } from "@/tauri/commands";
import type {
  StoryRef,
  DerivedPipelineTree,
  PipelineStatus,
} from "@/shared/lib/pipeline-graph/types";
import {
  formatStoryDisplayTitle,
  resolveDeclaredStoryTitle,
  storyMetadataTitle,
} from "@/shared/lib/pipeline-graph/story-display-title";
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import { useStatusState } from "@/shared/stores/status-taxonomy-store";
import { EpicAgentMark, StoryAgentMark } from "./nav-agent-mark";
import { ReadinessTip } from "./nav-readiness-tip";
import {
  normalizeStatus,
  stripPipelineExtension,
} from "@/shared/lib/pipeline-graph/pipeline-utils";
import { StatusSelectorPopover } from "@/shared/components/status-selector-popover";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
import {
  countDependencies,
  isReadyEligibleStatus,
  readyTitle,
  waitingTitle,
  type BlockedDep,
} from "@/shared/lib/pipeline-graph/resolve-nav-readiness";
import { type FlatNavItem, type StageGroupKey } from "@/features/dashboard/lib/flatten-nav-tree";
import { StageRail, StageRailShape } from "@/shared/components/stage-rail";
import {
  CS_FILEPATH_MIME,
  dragSourcePath,
  relatableTargetPath,
  isValidRelateDrop,
} from "@/features/dashboard/lib/nav-relate-dnd";
import { isInternalMoveToDir } from "@/features/dashboard/lib/nav-file-drop";
import { type PinnedNavItem } from "@/features/dashboard/lib/nav-pins";
import {
  isInstalledNavPath,
  installedRepoSlugFromNavPath,
} from "@/shared/lib/pipeline-graph/composite-to-repo-scan";

import { ScopeBadge, SourceRepoTint, type NavProvenance } from "../scope-badge";
import { resolveFileScope } from "@/features/dashboard/lib/file-scope";
import { FileKindIcon, fileKindOf, type FileKind } from "@/shared/lib/icons/file-kind";
import { useNavChange } from "@/features/git-sync/store/use-context-git-store";
import { CHANGE_KIND_WORD, type NavChange } from "@/features/git-sync/lib/nav-changes";

// ---------------------------------------------------------------------------
// Row dispatch
// ---------------------------------------------------------------------------

/** Stable React key per flat item, so virtualizer key changes track items
 *  rather than positions (preserves per-row local state across reorders). */
export function itemStableKey(item: FlatNavItem): string {
  switch (item.type) {
    case "folder-header":
      return `folder:${item.node.absoluteDirPath}`;
    case "story":
      return `story:${item.story.filePath}`;
    case "doc":
      return `doc:${item.filePath}`;
    case "main-doc":
      return `main-doc:${item.filePath}`;
    case "generic-dir":
      return `generic-dir:${item.entry.path}`;
    case "generic-file":
      return `generic-file:${item.entry.path}`;
    case "generic-loading":
      return `generic-loading:${item.parentPath}`;
    case "generic-empty":
      return `generic-empty:${item.parentPath}`;
    case "pinned-header":
      return "pinned-header";
    case "pinned-row":
      return `pinned:${item.pin.scope}:${item.pin.path}`;
    case "stage-group":
      return `stage-group:${item.key}`;
    case "section-note":
      return `note:${item.key}`;
  }
}

export interface RowInteractionModel {
  /** The team whose story workspaces the agent marks read. */
  teamKey: string;
  selectedPath: string | null;
  /** Open a story's workspace: the row's hover action and a click on its agent mark. */
  onOpenStoryWorkspace?: (filePath: string) => void;
  onFileSelect: (filePath: string) => void;
  onFileDoubleClick?: (filePath: string) => void;
  onDrillPathChange?: (absolutePath: string) => void;
  onStatusChange?: (filePath: string, newStatus: PipelineStatus) => void;
  onContextMenu: (e: React.MouseEvent, filePath: string, epicDirPath?: string) => void;
  toggleExpand: (absolutePath: string) => void;
  toggleGenericExpand: (absolutePath: string) => void;
  /** NB-7 / AC-5: Per-story popover open state hoisted from StoryRow so
   *  popover state survives virtualizer overscan eviction. */
  statusPopoverOpenMap: Map<string, boolean>;
  setStatusPopoverOpen: (filePath: string, open: boolean) => void;
  /** SLW-NC-1 Phase 3: provenance inputs for the source-repo tint on merged rows. */
  provenance?: NavProvenance;
  /** SW-VE-5: the active team name — the reference for the inherited-file gate
   *  (`sourceTeam !== activeTeam` ⇒ from a parent team). `null` when no
   *  team is active (nothing reads as inherited then — never over-warn). */
  activeTeam: string | null;
  /** SLW-NC-7: collapse/expand the Pinned section. */
  onTogglePinnedSection: () => void;
  /** SLW-NC-7: click a pinned row → reveal/open the real node. */
  onPinnedSelect: (pin: PinnedNavItem) => void;
  /** SLW-NC-7: unpin (hover icon / menu) at the pin's own scope. */
  onPinnedUnpin: (pin: PinnedNavItem) => void;
  /** SLW-NC-7: right-click a pinned row → unpin/remove menu. */
  onPinnedContextMenu: (e: React.MouseEvent, pin: PinnedNavItem) => void;
  /** SW-CP-27: is this absolute content path merge-marked (CP-24)? Drives the
   *  in-tree merge badge on generic config rows. Undefined ⇒ no active team /
   *  marks unavailable (no badge). */
  isMergeMarked?: (filePath: string) => boolean;
  /** Is this absolute path the team's starter page (the file it opens when nothing
   *  is open)? Drives the small house marker. Undefined ⇒ no marker. */
  isStarterPage?: (filePath: string) => boolean;
  /** SLW-PT-18: the per-file ⋯ "Move to… / Copy to… / Remove" affordance for a
   *  content file row. Returns the mounted control (source context + sheets) or
   *  `null` when the row can't carry it. Undefined ⇒ mobility not wired here. */
  moveActionSlot?: (filePath: string, fileName: string) => React.ReactNode;
  /** Arranged by stage: open or collapse a group (only Done collapses). */
  onToggleStageGroup?: (key: StageGroupKey) => void;
  /** The Changes view: changed rows also show their line counts. */
  showChangeLines?: boolean;
  /** The Changes view's per-row action (Attach a loose doc to a story or epic). */
  changeAction?: (filePath: string) => React.ReactNode;
}

export function renderRow(item: FlatNavItem, cb: RowInteractionModel): React.ReactElement {
  switch (item.type) {
    case "folder-header": {
      return (
        <FolderHeaderRow
          node={item.node}
          expanded={item.expanded}
          total={item.total}
          done={item.done}
          onToggleExpand={cb.toggleExpand}
          onDrillPathChange={cb.onDrillPathChange}
          onContextMenu={cb.onContextMenu}
          provenance={cb.provenance}
          teamKey={cb.teamKey}
        />
      );
    }
    case "story":
      return (
        <StoryRow
          story={item.story}
          isSelected={item.story.filePath === cb.selectedPath}
          onFileSelect={cb.onFileSelect}
          onFileDoubleClick={cb.onFileDoubleClick}
          onStatusChange={cb.onStatusChange}
          onContextMenu={cb.onContextMenu}
          isReady={item.isReady}
          blockedBy={item.blockedBy}
          isPopoverOpen={cb.statusPopoverOpenMap.get(item.story.filePath) ?? false}
          onPopoverOpenChange={cb.setStatusPopoverOpen}
          teamKey={cb.teamKey}
          onOpenWorkspace={cb.onOpenStoryWorkspace}
          showChangeLines={cb.showChangeLines}
        />
      );
    case "doc":
    case "main-doc":
      return (
        <DocRow
          filePath={item.filePath}
          label={item.label}
          isSelected={item.filePath === cb.selectedPath}
          onFileSelect={cb.onFileSelect}
          onFileDoubleClick={cb.onFileDoubleClick}
          // SLW-NC-1: installed docs are browse-only — no mutating context menu.
          onContextMenu={item.installed ? undefined : cb.onContextMenu}
          // SLW-NC-1 Phase 3: provenance tint + shadowed hint on source docs.
          sourceRepo={item.sourceRepo}
          shadows={item.shadows}
          overrides={item.type === "doc" ? item.overrides : undefined}
          // SW-XS-19: the owning team → the inherited marker naming it.
          sourceTeam={item.sourceTeam}
          provenance={cb.provenance}
          // SW-VE-5: reference for the scope badge's inherited marker.
          activeTeam={cb.activeTeam}
          // SLW-MD-1: plain-doc invalid-metadata badge (AC-16). `main-doc` items
          // don't carry validity, so this is undefined for them (no badge).
          metadataValid={item.type === "doc" ? item.metadataValid : undefined}
          metadataInvalidReason={item.type === "doc" ? item.metadataInvalidReason : undefined}
          starter={cb.isStarterPage?.(item.filePath) ?? false}
          // SLW-PT-18: mount the ⋯ move/copy/remove action on real content docs
          // only (installed source docs stay browse-only, like their context menu).
          moveAction={item.installed ? undefined : cb.moveActionSlot}
          teamKey={cb.teamKey}
          showChangeLines={cb.showChangeLines}
          changeAction={cb.changeAction}
        />
      );
    case "generic-dir":
      return (
        <GenericDirRow
          entry={item.entry}
          expanded={item.expanded}
          onToggleExpand={cb.toggleGenericExpand}
          onDrillPathChange={cb.onDrillPathChange}
          onContextMenu={cb.onContextMenu}
        />
      );
    // SW-VE-5: generic rows carry no owning team → no scope badge, by the graceful-degrade contract. They still share
    // `FileRowBase`, so the badge slot is ready when scope data becomes available.
    case "generic-file":
      return (
        <GenericFileRow
          entry={item.entry}
          isSelected={item.entry.path === cb.selectedPath}
          onFileSelect={cb.onFileSelect}
          onFileDoubleClick={cb.onFileDoubleClick}
          onContextMenu={cb.onContextMenu}
          mergeMarked={cb.isMergeMarked?.(item.entry.path) ?? false}
          starter={cb.isStarterPage?.(item.entry.path) ?? false}
          moveAction={cb.moveActionSlot}
          teamKey={cb.teamKey}
          showChangeLines={cb.showChangeLines}
          changeAction={cb.changeAction}
        />
      );
    // SW-VE-1: skeleton while an expanded generic dir's children load; muted
    // "Empty folder" row once resolved-empty — never a dead, blank open expand.
    case "generic-loading":
      return <GenericLoadingRow />;
    case "generic-empty":
      return <GenericEmptyRow />;
    case "pinned-header":
      return (
        <PinnedSectionHeader
          count={item.count}
          collapsed={item.collapsed}
          onToggle={cb.onTogglePinnedSection}
        />
      );
    case "stage-group":
      return (
        <StageGroupHeader
          groupKey={item.key}
          label={item.label}
          count={item.count}
          collapsed={item.collapsed}
          onToggle={cb.onToggleStageGroup}
        />
      );
    case "section-note":
      return (
        <SectionNoteRow
          tone={item.tone}
          text={item.text}
          count={item.count}
          trailingAction={item.trailingAction}
        />
      );
    case "pinned-row":
      return (
        <PinnedRow
          pin={item.pin}
          onSelect={cb.onPinnedSelect}
          onUnpin={cb.onPinnedUnpin}
          onContextMenu={cb.onPinnedContextMenu}
        />
      );
  }
}

// ---------------------------------------------------------------------------
// SLW-FX-21 — drag-to-link. A single wrapper carries the native HTML5 DnD for
// EVERY row type (added once here, not per row component). A leaf file row is a
// drag source; a relatable md/html row (story / non-installed doc / non-installed
// main-doc) is a drop target. Dropping A on B relates A INTO B (B.supporting_docs).
// The pure predicates live in `../lib/nav-relate-dnd` (unit-tested there).
// ---------------------------------------------------------------------------

/**
 * SLW-FX-21: wraps one nav row with drag-source + drop-target behavior. When
 * `onRelateDrop` is absent, or the row is neither a source nor a target, it
 * renders a bare passthrough (zero listeners). `draggedPathRef` is shared across
 * rows so `dragover` can read the in-flight path synchronously — HTML5 forbids
 * `dataTransfer.getData()` during dragover, so the ref is the O(1) source of
 * truth for the self-drop / same-root checks.
 */
export function NavRowDnd({
  item,
  rootDir,
  draggedPathRef,
  onRelateDrop,
  moveTargetDir,
  onMoveDrop,
  activeTeam = null,
  children,
}: {
  item: FlatNavItem;
  rootDir: string;
  draggedPathRef: React.MutableRefObject<string | null>;
  onRelateDrop?: (targetFilePath: string, draggedFilePath: string) => void;
  // SW-VE-7: when this row is a writable directory, its absolute path — an
  // INTERNAL file drag dropped here is a MOVE (not a relate). `null` for
  // file/story rows (which keep their relate-drop behavior).
  moveTargetDir?: string | null;
  onMoveDrop?: (toDir: string, draggedFilePath: string) => void;
  /** SW-XS-20: the active team — a doc inherited from a parent team is not a
   *  drag source (a drop would move the parent's file into a folder the parent
   *  may not have). */
  activeTeam?: string | null;
  children: React.ReactNode;
}) {
  const [isDropTarget, setIsDropTarget] = useState(false);

  const sourcePath = onRelateDrop ? dragSourcePath(item, activeTeam) : null;
  const relateTarget = onRelateDrop ? relatableTargetPath(item) : null;
  // SW-VE-7: a directory row is a move-drop target (mutually exclusive with a
  // relate target — dirs are never relate targets). A valid internal move needs
  // an in-flight internal path (OS drags never set `draggedPathRef`) that isn't
  // already in this dir. `installed://` dirs come through as `null` upstream.
  const moveDir = onMoveDrop ? (moveTargetDir ?? null) : null;
  const isValidMoveDrop = (dragged: string | null): boolean =>
    isInternalMoveToDir(dragged, moveDir);

  // Not a drag source, relate target, or move target → don't pay for handlers.
  if (!sourcePath && !relateTarget && !moveDir) return <>{children}</>;

  const droppable = relateTarget != null || moveDir != null;

  return (
    <div
      className={`relative rounded-md ${isDropTarget ? "cs-sidebar-drop-ring" : ""}`}
      draggable={sourcePath != null}
      onDragStart={
        sourcePath
          ? (e) => {
              e.dataTransfer.setData(CS_FILEPATH_MIME, sourcePath);
              e.dataTransfer.effectAllowed = moveDir != null ? "copyMove" : "link";
              draggedPathRef.current = sourcePath;
            }
          : undefined
      }
      onDragEnd={sourcePath ? () => (draggedPathRef.current = null) : undefined}
      onDragOver={
        droppable
          ? (e) => {
              // Cheap O(1) validity gate — read the in-flight path from the ref.
              const dragged = draggedPathRef.current;
              const ok = relateTarget
                ? isValidRelateDrop(dragged, relateTarget, rootDir)
                : isValidMoveDrop(dragged);
              if (ok) {
                e.preventDefault();
                e.dataTransfer.dropEffect = relateTarget ? "link" : "move";
                if (!isDropTarget) setIsDropTarget(true);
              } else if (isDropTarget) {
                setIsDropTarget(false);
              }
            }
          : undefined
      }
      onDragLeave={droppable ? () => setIsDropTarget(false) : undefined}
      onDrop={
        droppable
          ? (e) => {
              setIsDropTarget(false);
              // Authoritative read on drop (dataTransfer is readable here);
              // fall back to the ref if the payload didn't survive.
              const dragged = e.dataTransfer.getData(CS_FILEPATH_MIME) || draggedPathRef.current;
              if (relateTarget) {
                if (isValidRelateDrop(dragged, relateTarget, rootDir)) {
                  e.preventDefault();
                  onRelateDrop?.(relateTarget, dragged as string);
                }
              } else if (moveDir && isValidMoveDrop(dragged)) {
                e.preventDefault();
                onMoveDrop?.(moveDir, dragged as string);
              }
            }
          : undefined
      }
    >
      {children}
      {isDropTarget && (
        <span
          className="pointer-events-none absolute right-1.5 top-1/2 -translate-y-1/2 z-10"
          data-testid={relateTarget ? "relate-drop-icon" : "move-drop-icon"}
          aria-hidden
        >
          {relateTarget ? (
            <Link2
              size={13}
              style={{ color: "var(--cs-sidebar-selected-text, var(--foreground))" }}
            />
          ) : (
            <FolderInput
              size={13}
              style={{ color: "var(--cs-sidebar-selected-text, var(--foreground))" }}
            />
          )}
        </span>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// SLW-NC-1 Phase 3 — provenance affordances shared by folder + doc rows.
// ---------------------------------------------------------------------------

/** The "shadowed" hint badge (P3-AC-3b): a source winner is covering a
 *  team-workspace version at the same path. Hint only — the layered view is
 *  NC-2. */
function ShadowedBadge({ label = "Shadows a team-workspace version" }: { label?: string }) {
  return (
    <Layers
      size={10}
      className="flex-shrink-0 text-muted-foreground"
      data-testid="shadowed-badge"
      aria-label={label}
    />
  );
}

/** The team's starter page: a quiet house right after the file name, in the
 *  file icon's neutral ink — present, never louder than the name. */
function StarterPageMark() {
  return (
    <span
      className="inline-flex flex-shrink-0 items-center"
      title="Team starter page: opens whenever the team has nothing open"
      role="img"
      aria-label="Team starter page"
      data-testid="starter-page-mark"
    >
      <House size={11} className="cs-nav-ink" aria-hidden />
    </span>
  );
}

/** SW-CP-27: the merge-mark badge. A 10px sky `GitMerge` glyph marking a content
 *  file that projects via structured deep-merge (CP-24) instead of a whole-file
 *  clobber — so "this file merges" is legible in the tree, not hidden state.
 *  Supplementary (kept out of the roving tab order), matching `ShadowedBadge`. */
function MergeMarkedBadge() {
  return (
    <span
      className="inline-flex flex-shrink-0 items-center"
      title="Merges with the source file instead of overwriting it"
      role="img"
      aria-label="Merge file"
      data-testid="merge-marked-badge"
    >
      <GitMerge size={10} className="text-sky-500" aria-hidden />
    </span>
  );
}

// SLW-MD-1 (Task 9 / AC-11): the invalid-metadata badge. A 10px amber
// AlertTriangle in the right-aligned badge span, DISTINCT in color from the
// grey shadow-layers badge. Tooltip/aria-label names the first offending field.
// Kept out of the roving tab order (supplementary), matching ShadowedBadge.
function MetadataInvalidBadge({ reason }: { reason?: string | null }) {
  const label = reason ?? "Invalid metadata";
  return (
    <span
      className="inline-flex flex-shrink-0 items-center"
      title={label}
      role="img"
      aria-label={label}
      data-testid="metadata-invalid-badge"
    >
      <AlertTriangle size={10} className="text-[var(--amber,#d97706)]" aria-hidden />
    </span>
  );
}

// ---------------------------------------------------------------------------
// Git changes — the name's tint, the old name, the line counts.
// ---------------------------------------------------------------------------

/** The tint a changed name takes: green new, blue modified or renamed, dusty-rose struck
 *  deleted, violet conflict. Colour on the right of a row stays the agents'. */
const CHANGE_NAME_CLASS: Record<NavChange["kind"], string> = {
  added: "text-[var(--cs-git-added)]",
  modified: "text-[var(--cs-git-modified)]",
  renamed: "text-[var(--cs-git-modified)]",
  deleted: "text-[var(--cs-git-deleted)] line-through decoration-1",
  conflict: "text-[var(--cs-git-conflict)]",
};

export function changeNameClass(change: NavChange | undefined): string {
  return change ? CHANGE_NAME_CLASS[change.kind] : "";
}

/** What follows a changed name: "← old name" for a rename, "conflict" for a conflict. Words,
 *  so the change never rests on its colour alone. */
function ChangeTrail({ change }: { change: NavChange | undefined }) {
  if (!change) return null;
  if (change.kind === "conflict")
    return (
      <span className="flex-none text-[11px] font-semibold text-[var(--cs-git-conflict)]">
        conflict
      </span>
    );
  if (change.kind === "renamed" && change.renamedFrom)
    return (
      <span className="min-w-0 flex-shrink truncate text-[11px] text-muted-foreground">
        ← {stripViewerExtension(change.renamedFrom.split("/").pop() ?? change.renamedFrom)}
      </span>
    );
  return null;
}

/** `+41 −6` beside a changed row in the Changes view; the narrow nav drops it, the name keeps
 *  its tint and words. */
function ChangeLines({ change }: { change: NavChange | undefined }) {
  if (!change || (change.added === undefined && change.removed === undefined)) return null;
  const parts = [
    change.added ? `+${change.added}` : null,
    change.removed ? `−${change.removed}` : null,
  ].filter(Boolean);
  if (parts.length === 0) return null;
  return (
    <span
      className="flex-none text-[11px] tabular-nums text-muted-foreground @max-[15rem]:hidden"
      data-testid="nav-change-lines"
      aria-label={`${change.added ?? 0} lines added, ${change.removed ?? 0} removed`}
    >
      {parts.join(" ")}
    </span>
  );
}

/** The change, spoken: appended to a changed row's accessible name. */
function changeSpoken(change: NavChange | undefined): string | undefined {
  return change ? CHANGE_KIND_WORD[change.kind] : undefined;
}

/** A line of words in the list: a group's heading with its count, or a closing note. */
function SectionNoteRow({
  tone,
  text,
  count,
  trailingAction,
}: {
  tone: "heading" | "note";
  text: string;
  count?: number;
  trailingAction?: React.ReactNode;
}) {
  return tone === "heading" ? (
    <div className="flex h-[26px] items-center justify-between gap-1 px-2">
      <div
        role="heading"
        aria-level={3}
        className="flex min-w-0 items-center gap-1.5 text-[11px] font-semibold text-muted-foreground select-none"
        data-testid="nav-section-heading"
      >
        <span className="truncate">{text}</span>
        {count !== undefined && <span className="font-normal tabular-nums">{count}</span>}
      </div>
      {trailingAction}
    </div>
  ) : (
    <p
      className="px-2 pt-2 pb-1 text-[11px] leading-normal text-muted-foreground"
      data-testid="nav-section-note"
    >
      {text}
    </p>
  );
}

// ---------------------------------------------------------------------------
// Arranged by stage — a group's header.
// ---------------------------------------------------------------------------

/** One stage group's header: its rail (so header and rows agree), its name and its count.
 *  Done collapses; its header is the toggle. */
function StageGroupHeader({
  groupKey,
  label,
  count,
  collapsed,
  onToggle,
}: {
  groupKey: StageGroupKey;
  label: string;
  count: number;
  collapsed: boolean;
  onToggle?: (key: StageGroupKey) => void;
}) {
  const body = (
    <>
      {groupKey !== "needs" && <StageRailShape role={groupKey === "off" ? undefined : groupKey} />}
      <span>{label}</span>
      <span className="font-normal tabular-nums">
        {count}
        {groupKey === "done" && (collapsed ? " · collapsed" : "")}
      </span>
    </>
  );
  const cls =
    "flex h-[26px] w-full items-end gap-1.5 px-2 pb-1 text-[11px] font-semibold tracking-[.01em] text-muted-foreground select-none";
  return groupKey === "done" && onToggle ? (
    <button
      type="button"
      className={`${cls} rounded text-left hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring`}
      aria-expanded={!collapsed}
      data-testid="nav-stage-group-done"
      onClick={() => onToggle(groupKey)}
    >
      {body}
    </button>
  ) : (
    <div className={cls} role="heading" aria-level={3} data-testid={`nav-stage-group-${groupKey}`}>
      {body}
    </div>
  );
}

// ---------------------------------------------------------------------------
// SLW-NC-7 — the "Pinned" section (header + leaf shortcut rows).
// ---------------------------------------------------------------------------

/** The collapsible "Pinned" section header. Shows a count badge when collapsed
 *  (the rows are hidden), mirroring the notes rail's section-label pattern. */
function PinnedSectionHeader({
  count,
  collapsed,
  onToggle,
}: {
  count: number;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={!collapsed}
      data-testid="pinned-section-header"
      className="flex w-full items-center gap-1 px-1 py-1 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground hover:text-foreground"
    >
      {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
      <Pin size={11} className="flex-shrink-0" aria-hidden />
      <span>Pinned</span>
      {collapsed && (
        <span className="ml-1 rounded bg-muted px-1 text-[10px] leading-none tabular-nums">
          {count}
        </span>
      )}
    </button>
  );
}

/** One pinned shortcut row. Click reveals/opens the real node; a subtle team
 *  marker distinguishes shared pins; a hover pin-off icon unpins at the row's
 *  scope. A stale pin (target gone from the nav) renders dimmed. */
const PinnedRow = memo(function PinnedRow({
  pin,
  onSelect,
  onUnpin,
  onContextMenu,
}: {
  pin: PinnedNavItem;
  onSelect: (pin: PinnedNavItem) => void;
  onUnpin: (pin: PinnedNavItem) => void;
  onContextMenu: (e: React.MouseEvent, pin: PinnedNavItem) => void;
}) {
  const kind: FileKind = pin.isDir ? "folder" : fileKindOf(pin.path);
  return (
    <div
      role="button"
      tabIndex={0}
      data-testid="pinned-row"
      title={pin.stale ? `${pin.path} — no longer in the nav` : pin.path}
      onClick={() => onSelect(pin)}
      onContextMenu={(e) => onContextMenu(e, pin)}
      className={`group ${NAV_ROW_CLASS} cursor-pointer ${pin.stale ? "opacity-50" : ""}`}
    >
      <FileKindIcon kind={kind} size={14} className="cs-nav-ink" />
      <span className="flex-1 truncate text-sm">{pin.label}</span>
      {pin.scope === "team" && !pin.stale && (
        <span title="Pinned for the team" className="flex-shrink-0">
          <Users size={11} className="text-muted-foreground" aria-label="Pinned for the team" />
        </span>
      )}
      <button
        type="button"
        aria-label={pin.stale ? "Remove pin" : "Unpin"}
        title={pin.stale ? "Remove pin" : "Unpin"}
        onClick={(e) => {
          e.stopPropagation();
          onUnpin(pin);
        }}
        className="flex-shrink-0 opacity-0 group-hover:opacity-100 focus:opacity-100"
      >
        <PinOff size={12} />
      </button>
    </div>
  );
});

// ---------------------------------------------------------------------------
// FolderHeaderRow — replaces SubEpicFolder's header rendering
// ---------------------------------------------------------------------------

interface FolderHeaderRowProps {
  node: DerivedPipelineTree;
  expanded: boolean;
  total: number;
  done: number;
  onToggleExpand: (absolutePath: string) => void;
  onDrillPathChange?: (absolutePath: string) => void;
  onContextMenu: (
    e: React.MouseEvent,
    filePath: string,
    epicDirPath?: string,
    isEpic?: boolean,
    epicFilePath?: string,
  ) => void;
  /** SLW-NC-1 Phase 3: provenance inputs for the source-dir tint. */
  provenance?: NavProvenance;
  teamKey: string;
}

/** The row chrome every nav row shares: 24px, a 6px gutter, a ring (not a fill) on keyboard focus. */
export const NAV_ROW_CLASS =
  "cs-nav-row flex h-6 w-full items-center gap-1.5 rounded-[5px] px-1.5 cs-sidebar-item transition-colors select-none outline-none focus-visible:shadow-[inset_0_0_0_1.5px_var(--ring)] has-[:focus-visible]:shadow-[inset_0_0_0_1.5px_var(--ring)]";

/** A source repo's top folder in the nav (`installed://owner/repo/<folder>`): drawn as the repo. */
function isInstalledGroup(absPath: string): boolean {
  if (!isInstalledNavPath(absPath)) return false;
  return absPath.slice("installed://".length).split("/").filter(Boolean).length === 3;
}

/** The disclosure chevron of a folder row. */
function Disclosure({ expanded }: { expanded: boolean }) {
  const Chevron = expanded ? ChevronDown : ChevronRight;
  return <Chevron size={12} className="-mx-0.5 flex-none text-muted-foreground" aria-hidden />;
}

const FolderHeaderRow = memo(function FolderHeaderRow({
  node,
  expanded,
  total,
  done,
  onToggleExpand,
  onDrillPathChange,
  onContextMenu,
  provenance,
  teamKey,
}: FolderHeaderRowProps) {
  const isEpic = node.mainDoc !== null;
  // SLW-NC-1: installed groups are browse-only. Their synthetic `installed://`
  // absoluteDirPath is not a real cwd, so path-based affordances (cmd+click →
  // terminal `cd`, and the context menu's copy-path / delete / rename) are
  // suppressed for them; expand/collapse (opaque key) and drill (guarded in the
  // handler) still work. View/edit of installed content arrives in NC-2.
  const isInstalled = isInstalledNavPath(node.absoluteDirPath);
  // SLW-NC-1 Phase 3 (P3-AC-4): after the flat unified merge a source-repo dir
  // keeps its real folder name and is TINTED with its provenance color (no
  // `owner/repo` wrapper group anymore). Context dirs are untinted.
  const sourceRepoSlug = isInstalled ? installedRepoSlugFromNavPath(node.absoluteDirPath) : null;
  const kind: FileKind = isInstalledGroup(node.absoluteDirPath)
    ? "repo"
    : isEpic
      ? "epic"
      : expanded
        ? "folderOpen"
        : "folder";
  // The epic's roll-up mark reads every story under it, nested sub-epics included.
  const stories = useMemo(
    () =>
      total > 0
        ? collectAllStories(node).map((st) => ({
            filePath: st.filePath,
            storyId: st.storyId ?? undefined,
          }))
        : [],
    [node, total],
  );

  return (
    <div role="listitem" className="group relative">
      <button
        type="button"
        className={`${NAV_ROW_CLASS} text-left font-[450]`}
        onClick={(e) => {
          if (e.metaKey) {
            if (!isInstalled) sendTextToTerminal(node.absoluteDirPath);
            return;
          }
          onToggleExpand(node.absoluteDirPath);
        }}
        onContextMenu={(e) => {
          if (isInstalled) {
            e.preventDefault();
            return;
          }
          onContextMenu(e, node.absoluteDirPath, node.dirPath, isEpic, node.mainDoc?.filePath);
        }}
        aria-expanded={expanded}
      >
        <Disclosure expanded={expanded} />
        <FileKindIcon kind={kind} className="cs-nav-ink" />
        <span className="min-w-0 flex-1 truncate">{node.dirName}</span>
        {total > 0 && (
          <span className="ml-1 flex flex-none items-center gap-1.5">
            <EpicAgentMark teamKey={teamKey} stories={stories} epicLabel={node.dirName} />
            <span
              className={`min-w-[22px] text-right text-[11.5px] font-normal tabular-nums text-muted-foreground @max-[13rem]:hidden ${
                onDrillPathChange
                  ? "transition-opacity group-hover:opacity-0 group-has-[:focus-visible]:opacity-0"
                  : ""
              }`}
              data-testid="nav-epic-progress"
            >
              {done}/{total}
            </span>
          </span>
        )}
        {/* SLW-NC-1 Phase 3: source-repo provenance tint, right-aligned. A source
            dir is docs-only (never stories) so it never has a count chip — the
            tint always takes the right edge. */}
        {sourceRepoSlug && (
          <span className="ml-auto flex-shrink-0">
            <SourceRepoTint slug={sourceRepoSlug} provenance={provenance} />
          </span>
        )}
      </button>

      {/* Drill button — hover-revealed on right edge, same pattern as file-tree */}
      {onDrillPathChange && (
        <button
          type="button"
          className="absolute right-0.5 top-1/2 -translate-y-1/2 p-1 rounded opacity-0 group-hover:opacity-100 group-has-[:focus-visible]:opacity-100 focus-visible:opacity-100 transition-opacity text-muted-foreground hover:text-foreground"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onDrillPathChange(node.absoluteDirPath);
          }}
          aria-label={`Drill into ${node.dirName}`}
          title={`Drill into ${node.dirName}`}
        >
          <ChevronRight size={12} />
        </button>
      )}
    </div>
  );
});

// ---------------------------------------------------------------------------
// StoryRow
// ---------------------------------------------------------------------------

const lowerFirst = (s: string) => s.charAt(0).toLowerCase() + s.slice(1);

interface StoryRowProps {
  story: StoryRef;
  isSelected: boolean;
  onFileSelect: (filePath: string) => void;
  onFileDoubleClick?: (filePath: string) => void;
  onStatusChange?: (filePath: string, newStatus: PipelineStatus) => void;
  onContextMenu?: (e: React.MouseEvent, filePath: string, epicDirPath?: string) => void;
  isReady?: boolean;
  /** Deps blocking readiness — used to name an off-screen (out-of-drill)
   *  blocker in the row tooltip (SW-FX-1 AC-7). */
  blockedBy?: BlockedDep[];
  /** NB-7 / AC-5: controlled popover open state — preserved across
   *  virtualizer overscan eviction by the parent's keyed Map. */
  isPopoverOpen?: boolean;
  onPopoverOpenChange?: (filePath: string, open: boolean) => void;
  teamKey: string;
  onOpenWorkspace?: (filePath: string) => void;
  /** The Changes view: show the line counts of a changed story. */
  showChangeLines?: boolean;
}

/** The hover action button of a story row. */
const ROW_ACTION_CLASS =
  "grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring";

const StoryRow = memo(function StoryRow({
  story,
  isSelected,
  onFileSelect,
  onFileDoubleClick,
  onStatusChange,
  onContextMenu,
  isReady,
  blockedBy,
  isPopoverOpen,
  onPopoverOpenChange,
  teamKey,
  onOpenWorkspace,
  showChangeLines,
}: StoryRowProps) {
  const status = story.status?.trim() || normalizeStatus(story.status);
  const change = useNavChange(teamKey, story.filePath);
  const stage = useStatusState(status);
  const isDone = stage?.role === "done";

  // A not-started story says whether it can start: ready (every dependency done), or what it
  // waits on, naming an off-screen (out-of-drill) dependency too.
  const readiness = isReady
    ? readyTitle(countDependencies(story))
    : isReadyEligibleStatus(status) && blockedBy && blockedBy.length > 0
      ? waitingTitle(blockedBy)
      : undefined;
  // The rail names the stage; waiting, it adds what on.
  const railLabel =
    readiness && !isReady ? `${stage?.label ?? status} · ${lowerFirst(readiness)}` : readiness;

  // R-14: scanner no longer populates `title`. A story with a metadata title
  // (frontmatter `title:` / HTML `cs:title`, carried on the scan) reads
  // "<id> <title>" with the id in muted ink; otherwise the filename stem.
  // Deprecated test fixtures may still pass title.
  const titleMeta = storyMetadataTitle(story);
  const id = story.storyId?.trim() || undefined;
  const declared = titleMeta ? resolveDeclaredStoryTitle({ storyId: id, titleMeta }) : undefined;
  const title =
    story.title ??
    (titleMeta
      ? formatStoryDisplayTitle({ storyId: id, titleMeta, fileName: story.fileName })
      : stripPipelineExtension(story.fileName));
  const tooltip =
    title === stripPipelineExtension(story.fileName) ? title : `${title}\n${story.fileName}`;
  const shortLabel = id ?? title;

  const handleStatusChange = useCallback(
    (newStatus: PipelineStatus) => {
      onStatusChange?.(story.filePath, newStatus);
    },
    [onStatusChange, story.filePath],
  );

  const handlePopoverOpenChange = useCallback(
    (open: boolean) => onPopoverOpenChange?.(story.filePath, open),
    [onPopoverOpenChange, story.filePath],
  );

  // The rail names the readiness in its accessible name; the card on hover says it in full.
  const rail = <StageRail status={status} ready={isReady} label={railLabel} titled={!readiness} />;

  const markStory = useMemo(
    () => ({ filePath: story.filePath, storyId: id }),
    [story.filePath, id],
  );
  const openWorkspace = onOpenWorkspace ? () => onOpenWorkspace(story.filePath) : undefined;

  return (
    <div
      role="listitem"
      data-filepath={story.filePath}
      // Ready (every dependency done) is drawn in the rail's ink, never in colour: in the
      // nav, colour only ever means an agent.
      data-ready={isReady || undefined}
      className={`group/row relative ${NAV_ROW_CLASS} ${
        isSelected ? "cs-sidebar-item-selected font-medium" : ""
      }`}
      onContextMenu={onContextMenu ? (e) => onContextMenu(e, story.filePath) : undefined}
    >
      <span className="-ml-0.5 flex w-[18px] flex-none justify-center">
        <ReadinessTip
          ready={!!isReady}
          dependencyCount={countDependencies(story)}
          blockedBy={readiness ? blockedBy : undefined}
        >
          {onStatusChange ? (
            <StatusSelectorPopover
              currentStatus={status}
              onSelect={handleStatusChange}
              open={isPopoverOpen ?? false}
              onOpenChange={handlePopoverOpenChange}
              triggerAriaLabel="Change status"
            >
              {rail}
            </StatusSelectorPopover>
          ) : (
            rail
          )}
        </ReadinessTip>
      </span>

      <button
        type="button"
        className="flex items-center gap-1.5 min-w-0 flex-1 text-left cursor-pointer outline-none"
        onClick={(e) => {
          if (e.metaKey) {
            sendTextToTerminal(story.filePath);
            return;
          }
          onFileSelect(story.filePath);
        }}
        onDoubleClick={onFileDoubleClick ? () => onFileDoubleClick(story.filePath) : undefined}
        title={tooltip}
      >
        <span
          className={cn(
            "truncate",
            change
              ? changeNameClass(change)
              : isDone && "text-[color-mix(in_srgb,var(--foreground)_62%,transparent)]",
          )}
          data-testid="nav-story-title"
          data-change={change?.kind}
        >
          {id && declared ? (
            <>
              <span
                className={cn(
                  "mr-[5px] text-[12px] font-normal tabular-nums",
                  !change && "text-muted-foreground",
                )}
              >
                {id}
              </span>
              {declared}
            </>
          ) : (
            title
          )}
        </span>
        {change && <span className="sr-only">, {changeSpoken(change)}</span>}
        <ChangeTrail change={change} />
      </button>
      {/* SLW-MD-1: invalid-metadata badge (AC-11). */}
      {story.metadataValid === false && (
        <span className="flex flex-shrink-0 items-center">
          <MetadataInvalidBadge reason={story.metadataInvalidReason} />
        </span>
      )}
      <span className="ml-1 flex flex-none items-center gap-1.5">
        {/* Hover actions sit LEFT of the mark, so the mark never moves. */}
        <span className="hidden items-center gap-0.5 group-hover/row:flex group-focus-within/row:flex">
          {openWorkspace && (
            <button
              type="button"
              className={ROW_ACTION_CLASS}
              aria-label={`Open ${shortLabel}'s workspace`}
              title={`Open ${shortLabel}'s workspace`}
              data-testid="nav-story-open-workspace"
              onClick={(e) => {
                e.stopPropagation();
                openWorkspace();
              }}
            >
              <SquareTerminal size={13} aria-hidden />
            </button>
          )}
          {onContextMenu && (
            <button
              type="button"
              className={ROW_ACTION_CLASS}
              aria-label={`More for ${shortLabel}`}
              title="More"
              onClick={(e) => {
                e.stopPropagation();
                onContextMenu(e, story.filePath);
              }}
            >
              <MoreHorizontal size={14} aria-hidden />
            </button>
          )}
        </span>
        <StoryAgentMark
          teamKey={teamKey}
          story={markStory}
          storyLabel={shortLabel}
          onOpen={openWorkspace}
        />
        {showChangeLines && <ChangeLines change={change} />}
      </span>
    </div>
  );
});

// ---------------------------------------------------------------------------
// DocRow
// ---------------------------------------------------------------------------

/** A row action shown on hover or keyboard focus (or while its menu is open), so a quiet
 *  row carries no chrome. */
function HoverReveal({ children }: { children: React.ReactNode }) {
  return (
    <span className="flex opacity-0 transition-opacity group-hover/row:opacity-100 group-focus-within/row:opacity-100 has-[[data-state=open]]:opacity-100">
      {children}
    </span>
  );
}

/** SW-VE-1: a skeleton row shown under an expanded generic dir while its
 *  children are still being fetched — mirrors the worktree-tree / source-diff
 *  `animate-pulse` idiom. Non-interactive; the row wrapper applies the depth
 *  indent. */
function GenericLoadingRow() {
  return (
    <div className="px-2 py-1" aria-hidden data-testid="generic-loading-row">
      <div className="h-4 w-2/3 animate-pulse rounded bg-muted/50" />
    </div>
  );
}

/** SW-VE-1: muted "Empty folder" row for an expanded dir that resolved to zero
 *  children (or errored — the nav's calm "reads as empty" contract), so the
 *  expand never renders as a dead, blank open (the `bmad/_config` bug). Mirrors
 *  the source-diff "No source changes" idiom. */
function GenericEmptyRow() {
  return (
    <div
      className="px-2 py-1 text-xs italic text-muted-foreground select-none"
      data-testid="generic-empty-row"
    >
      Empty folder
    </div>
  );
}

// ---------------------------------------------------------------------------
// SW-VE-5 (AC-5) — `FileRowBase`: the shared leaf-file row shape both `DocRow`
// and `GenericFileRow` converge on (the convergence VE-1 deliberately deferred
// here). A div wrapper (role=listitem, data-filepath, selection, onContextMenu)
// + an inner click button (icon · truncated name · ⌘-click→terminal) + an
// optional right-badge slot. Owning the ⌘-click/select/dblclick behavior once
// dedups it out of the two callers. Matches `DocRow`'s proven div+inner-button
// shape (it already hosts nested popover triggers), so the nav's roving
// `role="listitem"` focus + selection are preserved byte-for-behavior.
// ---------------------------------------------------------------------------
function FileRowBase({
  filePath,
  label,
  isSelected,
  onSelect,
  onFileDoubleClick,
  onContextMenu,
  rightSlot,
  change,
  showChangeLines,
  changeAction,
  starter,
}: {
  filePath: string;
  label: string;
  isSelected: boolean;
  onSelect: (filePath: string) => void;
  onFileDoubleClick?: (filePath: string) => void;
  onContextMenu?: (e: React.MouseEvent, filePath: string) => void;
  /** Right-aligned badges (scope / shadow / metadata). Omitted ⇒ no badge span. */
  rightSlot?: React.ReactNode;
  /** The file's change against GitHub, if any: tints the name. */
  change?: NavChange;
  /** The Changes view: the line counts and the per-row action. */
  showChangeLines?: boolean;
  changeAction?: React.ReactNode;
  /** The team's starter page: a small house after the name. */
  starter?: boolean;
}) {
  // A deleted file has nothing on disk for the file menu to act on.
  const menu = change?.kind === "deleted" ? undefined : onContextMenu;
  return (
    <div
      role="listitem"
      data-filepath={filePath}
      className={`group/row ${NAV_ROW_CLASS} ${isSelected ? "cs-sidebar-item-selected font-medium" : ""}`}
      onContextMenu={menu ? (e) => menu(e, filePath) : undefined}
    >
      <button
        type="button"
        className="flex min-w-0 flex-1 items-center gap-1.5 text-left cursor-pointer outline-none"
        onClick={(e) => {
          if (e.metaKey) {
            sendTextToTerminal(filePath);
            return;
          }
          onSelect(filePath);
        }}
        onDoubleClick={onFileDoubleClick ? () => onFileDoubleClick(filePath) : undefined}
      >
        <FileKindIcon kind={fileKindOf(filePath)} className="cs-nav-ink" />
        <span className={cn("truncate", changeNameClass(change))} data-change={change?.kind}>
          {label}
        </span>
        {starter && <StarterPageMark />}
        {change && <span className="sr-only">, {changeSpoken(change)}</span>}
        <ChangeTrail change={change} />
      </button>
      {rightSlot}
      {changeAction && <HoverReveal>{changeAction}</HoverReveal>}
      {showChangeLines && <ChangeLines change={change} />}
    </div>
  );
}

const DocRow = memo(function DocRow({
  filePath,
  label,
  isSelected,
  onFileSelect,
  onFileDoubleClick,
  onContextMenu,
  sourceRepo,
  shadows,
  overrides,
  sourceTeam,
  provenance,
  activeTeam,
  metadataValid,
  metadataInvalidReason,
  moveAction,
  teamKey,
  showChangeLines,
  changeAction,
  starter,
}: {
  filePath: string;
  label: string;
  isSelected: boolean;
  onFileSelect: (filePath: string) => void;
  onFileDoubleClick?: (filePath: string) => void;
  onContextMenu?: (
    e: React.MouseEvent,
    filePath: string,
    epicDirPath?: string,
    storyId?: string,
  ) => void;
  /** SLW-NC-1 Phase 3: source-repo slug → provenance color tint (P3-AC-4). */
  sourceRepo?: string;
  /** SLW-NC-1 Phase 3: this source doc shadows a team-workspace version (P3-AC-3b). */
  shadows?: boolean;
  /** SW-XS-20: this own doc overrides the named parent team's file at the same path. */
  overrides?: string;
  /** SW-XS-19: the team that owns this doc (feeds the inherited marker). */
  sourceTeam?: string | null;
  /** SLW-NC-1 Phase 3: provenance inputs for the source-repo tint. */
  provenance?: NavProvenance;
  /** SW-VE-5: active team — reference for the scope badge's inherited marker. */
  activeTeam: string | null;
  /** SLW-MD-1: `false` ⇒ render the amber invalid-metadata badge (AC-16). */
  metadataValid?: boolean;
  metadataInvalidReason?: string | null;
  /** SLW-PT-18: the per-file ⋯ move/copy/remove affordance (content docs only). */
  moveAction?: (filePath: string, fileName: string) => React.ReactNode;
  teamKey: string;
  showChangeLines?: boolean;
  changeAction?: (filePath: string) => React.ReactNode;
  /** The team's starter page → the house marker. */
  starter?: boolean;
}) {
  const change = useNavChange(teamKey, filePath);
  // SW-VE-5: project the owning team into a scope badge (source-repo tint /
  // inherited-from-parent). `ScopeBadge` subsumes the old bare `SourceRepoTint`
  // (via its `sourceRepo` fallback) so a source row with no team signal still
  // tints exactly as before — no regression.
  const scope = resolveFileScope(sourceTeam, sourceRepo, activeTeam);
  const scopeRenders = !!scope?.isInherited || !!(scope?.repoSlug ?? sourceRepo);
  // SLW-PT-18: the ⋯ move/copy/remove control for this content doc (or nothing).
  // SLW-PT-B5: the slot keys the backend on the doc's real on-disk logical name,
  // so hand it the extension-bearing basename from the path — NOT `label`, which
  // is extension-stripped for display (`movable-doc` vs `movable-doc.md`) and
  // would make the move backend join `content/<stem>` (a file that isn't on disk).
  const moveControl = moveAction?.(filePath, filePath.split("/").pop() ?? label);
  const rightSlot =
    shadows || overrides || metadataValid === false || scopeRenders || moveControl ? (
      <span className="ml-auto flex flex-shrink-0 items-center gap-1.5">
        {/* SLW-MD-1: invalid-metadata badge (AC-11/16), before the shadow/scope. */}
        {metadataValid === false && <MetadataInvalidBadge reason={metadataInvalidReason} />}
        {/* SW-XS-8c: no backend produces layered versions any more (SW-XS-15),
            so the shadow badge has no detail popover. */}
        {shadows && <ShadowedBadge />}
        {!shadows && overrides && <ShadowedBadge label={`Overrides ${overrides}'s version`} />}
        {/* SW-VE-5: scope legibility (subsumes the old sourceRepo-only tint). */}
        <ScopeBadge scope={scope} sourceRepo={sourceRepo} provenance={provenance} />
        {/* SLW-PT-18: the per-file ⋯ move/copy/remove action, last in the slot. */}
        {moveControl && <HoverReveal>{moveControl}</HoverReveal>}
      </span>
    ) : undefined;
  return (
    <FileRowBase
      filePath={filePath}
      label={label}
      isSelected={isSelected}
      onSelect={onFileSelect}
      onFileDoubleClick={onFileDoubleClick}
      onContextMenu={onContextMenu}
      rightSlot={rightSlot}
      change={change}
      showChangeLines={showChangeLines}
      changeAction={showChangeLines ? changeAction?.(filePath) : undefined}
      starter={starter}
    />
  );
});

// ---------------------------------------------------------------------------
// GenericDirRow / GenericFileRow — replace the recursive GenericFileEntry
// ---------------------------------------------------------------------------

interface GenericDirRowProps {
  entry: FileEntry;
  expanded: boolean;
  onToggleExpand: (absolutePath: string) => void;
  onDrillPathChange?: (absolutePath: string) => void;
  onContextMenu: (e: React.MouseEvent, filePath: string) => void;
}

const GenericDirRow = memo(function GenericDirRow({
  entry,
  expanded,
  onToggleExpand,
  onDrillPathChange,
  onContextMenu,
}: GenericDirRowProps) {
  return (
    <div role="listitem" className="group relative">
      <button
        type="button"
        className={`${NAV_ROW_CLASS} text-left ${onDrillPathChange ? "pr-6" : ""}`}
        onClick={(e) => {
          if (e.metaKey) {
            sendTextToTerminal(entry.path);
            return;
          }
          onToggleExpand(entry.path);
        }}
        aria-expanded={expanded}
        onContextMenu={(e) => onContextMenu(e, entry.path)}
      >
        <Disclosure expanded={expanded} />
        <FileKindIcon kind={expanded ? "folderOpen" : "folder"} className="cs-nav-ink" />
        <span className="truncate">{entry.name}</span>
      </button>
      {onDrillPathChange && (
        <button
          type="button"
          className="absolute right-0.5 top-1/2 -translate-y-1/2 p-1 rounded opacity-0 group-hover:opacity-100 group-has-[:focus-visible]:opacity-100 focus-visible:opacity-100 transition-opacity text-muted-foreground hover:text-foreground"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onDrillPathChange(entry.path);
          }}
          aria-label={`Drill into ${entry.name}`}
          title={`Drill into ${entry.name}`}
        >
          <ChevronRight size={12} />
        </button>
      )}
    </div>
  );
});

interface GenericFileRowProps {
  entry: FileEntry;
  isSelected: boolean;
  onFileSelect: (filePath: string) => void;
  onFileDoubleClick?: (filePath: string) => void;
  onContextMenu: (
    e: React.MouseEvent,
    filePath: string,
    epicDirPath?: string,
    storyId?: string,
  ) => void;
  /** SW-CP-27: this file projects via structured deep-merge (CP-24) → merge badge. */
  mergeMarked?: boolean;
  /** The team's starter page → the house marker. */
  starter?: boolean;
  /** SLW-PT-18: the per-file ⋯ move/copy/remove affordance. */
  moveAction?: (filePath: string, fileName: string) => React.ReactNode;
  teamKey: string;
  showChangeLines?: boolean;
  changeAction?: (filePath: string) => React.ReactNode;
}

const GenericFileRow = memo(function GenericFileRow({
  entry,
  isSelected,
  onFileSelect,
  onFileDoubleClick,
  onContextMenu,
  mergeMarked,
  starter,
  moveAction,
  teamKey,
  showChangeLines,
  changeAction,
}: GenericFileRowProps) {
  const change = useNavChange(teamKey, entry.path);
  // Strip only the `.excalidraw` extension — md/html in generic folders keep
  // their extension (the disambiguator) as before.
  const label =
    entry.extension?.toLowerCase() === "excalidraw" ? stripViewerExtension(entry.name) : entry.name;
  // SW-VE-5: generic rows carry no owning team → no scope badge (graceful degrade);
  // they still share `FileRowBase`. SW-CP-27 uses that badge slot for the merge
  // indicator on a marked config file.
  // SLW-PT-B5: key the move slot on the real on-disk basename (extension intact),
  // not the display `label` (which strips `.excalidraw`) — see the DocRow note.
  // A deleted file has nothing on disk to move.
  const moveControl =
    change?.kind === "deleted"
      ? null
      : moveAction?.(entry.path, entry.path.split("/").pop() ?? label);
  const rightSlot =
    mergeMarked || moveControl ? (
      <span className="ml-auto flex flex-shrink-0 items-center gap-1.5">
        {mergeMarked && <MergeMarkedBadge />}
        {moveControl && <HoverReveal>{moveControl}</HoverReveal>}
      </span>
    ) : undefined;
  return (
    <FileRowBase
      filePath={entry.path}
      label={label}
      isSelected={isSelected}
      onSelect={onFileSelect}
      onFileDoubleClick={onFileDoubleClick}
      onContextMenu={onContextMenu}
      rightSlot={rightSlot}
      change={change}
      showChangeLines={showChangeLines}
      changeAction={showChangeLines ? changeAction?.(entry.path) : undefined}
      starter={starter}
    />
  );
});

// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// RF-12: the pipeline-status-nav CONTEXT MENU, relocated verbatim from
// `pipeline-status-nav.tsx` (no behavior change). Presentational — the
// orchestrator owns the menu state, the pin/inherited gating, and every action
// handler; this component renders the menu items and calls back. Pure predicates
// and icons are imported directly (they need no orchestrator state).
// ---------------------------------------------------------------------------
import {
  BookMinus,
  BookPlus,
  CalendarMinus,
  CalendarPlus,
  Download,
  FilePlus,
  FolderInput,
  GitMerge,
  ImageDown,
  CopyPlus,
  CornerLeftUp,
  Paperclip,
  Pencil,
  Pin,
  PinOff,
  Shapes,
  Route,
  SquareTerminal,
  Trash2,
  Users,
} from "lucide-react";
import {
  PositionedContextMenu,
  contextMenuItemClass,
  PositionedMenuItem,
} from "@/shared/components/positioned-context-menu";
import { canExportToPdf } from "@/features/viewer/lib/export-pdf";
import { canCreateCanvas, canExportCanvas } from "@/features/dashboard/lib/create-canvas";
import { exportCanvas } from "@/features/dashboard/lib/export-canvas";
import type { PinnedNavItem } from "@/features/dashboard/lib/nav-pins";
import type { SprintTargetOption } from "@/shared/lib/pipeline-graph/sprint-targets";
import type { NavContextMenuState } from "./types";
import type { useNavFileActions } from "./use-nav-file-actions";
import { LaunchAgentButtonList } from "@/features/story-launch/launch-agent-menu";
import { StarterPageMenuItems } from "@/features/viewer/components/starter-page-menu-items";
import { useStoryAgentLaunchActions } from "@/features/story-launch/use-story-agent-launch";
import type { ReactNode } from "react";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";

/**
 * SW-SB-15: the nav context menu's sprint-tag section. The orchestrator computes
 * the right-clicked row's story/epic context and the targets; this component only
 * renders + calls back through the `sprint-tag-actions` seam.
 */
export interface NavSprintMenu {
  /** Concrete sprint targets (active → planning → closed). */
  targets: SprintTargetOption[];
  /** The right-clicked STORY's context, or null when the row isn't a story. */
  story: { filePath: string; current: string | null } | null;
  /** The right-clicked EPIC-dir's context, or null when the row isn't an epic. */
  epic: { unassigned: number; total: number } | null;
  /** Add / move a single story to a concrete sprint (`current` = its sprint now). */
  onSetStory: (filePath: string, current: string | null, sprintId: string) => void;
  /** Remove a single story from its sprint. */
  onRemoveStory: (filePath: string, current: string) => void;
  /** Open the epic bulk-add confirm (unassigned-only vs re-point-all). */
  onBulkEpic: () => void;
}

// RF-12b: collapsed from 25 individual props to bundles grouped by source
// (`menu`/`onClose` + `actions` + `pins` + `parent`). The bundles are destructured-with-alias back to the exact local
// names the JSX body already used, so the body below is unchanged.
interface NavContextMenuProps {
  extraActions?: ReactNode;
  menu: NavContextMenuState | null;
  onClose: () => void;
  // The file-action subsystem (dialog openers, canvas handlers,
  // menu-scope gating). Every member the menu reads is on this hook return.
  actions: ReturnType<typeof useNavFileActions>;
  // Pin gating + togglers (SLW-NC-7). The four flags are menu-derived, so the
  // orchestrator (which owns the menu state) builds this bundle.
  pins: {
    pinnable: boolean;
    pinKey: string | null;
    isTeamPinned: boolean;
    isPersonalPinned: boolean;
    teamKey: string;
    toggleTeamPin: (absPath: string, pinned: boolean) => void;
    togglePersonalPin: (teamKey: string, rel: string) => void;
    unpinPinnedRow: (pin: PinnedNavItem) => void;
  };
  // Parent-supplied optional actions (shared provenance → one bundle).
  parent: {
    onOpenStoryWorkspace?: (filePath: string) => void;
    onOpenTrail?: (filePath: string) => void;
    /** The story row's "Open CLI" item, supplied by a host that offers where to open it. */
    renderOpenCli?: (filePath: string, onClose: () => void) => ReactNode;
    isStoryFilePath?: (filePath: string) => boolean;
    onOpenEpicTerminal?: (epicDirName: string, epicFilePath: string) => void;
    onExportPdf?: (filePath: string) => void;
  };
  // SW-SB-15: sprint-tag section (omitted → no sprint items appear).
  sprint?: NavSprintMenu;
  // SW-CP-27: merge-mark toggle for a structured config row. Omitted (→ no item)
  // for a non-markable file, a pinned/inherited row, or when no team is active —
  // the orchestrator gates on the CP-24 format list, so the item never offers a
  // mark CP-24 would reject.
  mergeMark?: { marked: boolean; onToggle: () => void };
  // SW-XS-15: "Add to / Remove from ‹Agent›'s context" (and "… for ‹Repo› only") for the
  // right-clicked team-content row. Omitted (Team (everything), a pinned or repo-tier row)
  // → no items.
  agentContext?: { context: NavAgentContext; rel: string };
  // Story authoring on an epic row: "New story…". Omitted → no item.
  storyAuthoring?: {
    onNewStory: (epicDirAbs: string) => void;
  };
}

export function NavContextMenu({
  extraActions,
  menu,
  onClose,
  actions,
  pins,
  parent,
  sprint,
  mergeMark,
  agentContext,
  storyAuthoring,
}: NavContextMenuProps) {
  // Alias the bundle members back to the body's original local names so the JSX
  // below stays byte-identical to the pre-bundle version.
  const {
    menuInherited: inherited,
    menuScope: scope,
    activeTeamName,
    copyIntoActiveTeam: onCopyIntoTeam,
    menuFileGestures,
    newFileFromRow,
    attachCanvas,
    canvasAttach,
    handleCreateCanvas: onCreateCanvas,
    handleCopyPath: onCopyPath,
    setMoveTarget: onMove,
    setRenameTarget: onRename,
    setDeleteTarget: onDelete,
  } = actions;
  const onAttachOpen = canvasAttach.open;
  const {
    pinnable,
    pinKey,
    isTeamPinned,
    isPersonalPinned,
    teamKey,
    toggleTeamPin: onToggleTeamPin,
    togglePersonalPin: onTogglePersonalPin,
    unpinPinnedRow: onUnpin,
  } = pins;
  const {
    onOpenStoryWorkspace,
    onOpenTrail,
    renderOpenCli,
    isStoryFilePath,
    onOpenEpicTerminal,
    onExportPdf,
  } = parent;
  const launchActions = useStoryAgentLaunchActions();
  return (
    <PositionedContextMenu
      owner={menu?.owner}
      position={menu}
      onClose={onClose}
      label="File actions"
    >
      {extraActions}
      {/* SLW-NC-7: unpin/remove for a right-clicked PINNED row. */}
      {menu?.pin && (
        <PositionedMenuItem
          className="flex items-center gap-2"
          onClick={() => {
            const pin = menu.pin;
            onClose();
            if (pin) onUnpin(pin);
          }}
        >
          <PinOff size={14} className="flex-shrink-0" />
          {menu.pin.stale ? "Remove pin" : menu.pin.scope === "team" ? "Unpin (team)" : "Unpin"}
        </PositionedMenuItem>
      )}
      {/* SLW-NC-7: pin/unpin actions for a pinnable tree row (dir or file). */}
      {pinnable && menu && (
        <>
          <PositionedMenuItem
            className="flex items-center gap-2"
            onClick={() => {
              const p = menu.path;
              const pinned = !isTeamPinned;
              onClose();
              onToggleTeamPin(p, pinned);
            }}
          >
            <Users size={14} className="flex-shrink-0" />
            {isTeamPinned ? "Unpin from top (team)" : "Pin to top (team)"}
          </PositionedMenuItem>
          <PositionedMenuItem
            className="flex items-center gap-2"
            onClick={() => {
              const rel = pinKey;
              onClose();
              if (rel) onTogglePersonalPin(teamKey, rel);
            }}
          >
            <Pin size={14} className="flex-shrink-0" />
            {isPersonalPinned ? "Unpin (for me only)" : "Pin for me only"}
          </PositionedMenuItem>
        </>
      )}
      {/* SLW-NC-7: a pinned-row menu shows ONLY unpin/remove (above) — suppress the
          tree-row gestures, whose predicates would otherwise misfire on a pin's
          repo-relative path (e.g. a spurious "Delete"/"Rename" on a pinned file). */}
      {!menu?.pin && (
        <>
          {onOpenTrail && menu && isStoryFilePath?.(menu.path) && (
            <PositionedMenuItem
              onClick={() => {
                onOpenTrail(menu.path);
                onClose();
              }}
            >
              <Route size={14} className="flex-shrink-0" />
              Trail
            </PositionedMenuItem>
          )}
          {isStoryFilePath &&
            menu &&
            isStoryFilePath(menu.path) &&
            (renderOpenCli
              ? renderOpenCli(menu.path, onClose)
              : onOpenStoryWorkspace && (
                  <PositionedMenuItem
                    className="flex items-center gap-2"
                    data-testid="nav-open-cli"
                    onClick={() => {
                      onOpenStoryWorkspace(menu.path);
                      onClose();
                    }}
                  >
                    <SquareTerminal size={14} className="flex-shrink-0" />
                    Open CLI
                  </PositionedMenuItem>
                ))}
          {launchActions &&
            launchActions.agents.length > 0 &&
            isStoryFilePath &&
            menu &&
            isStoryFilePath(menu.path) && (
              <div role="group" aria-label="Launch agent" data-testid="nav-launch-agent">
                <div className="px-2 pb-0.5 pt-1.5 text-[11px] font-medium text-muted-foreground">
                  Launch agent
                </div>
                <LaunchAgentButtonList
                  actions={launchActions}
                  target={{ storyFilePath: menu.path }}
                  itemClassName={contextMenuItemClass}
                  itemRole="menuitem"
                  onChosen={onClose}
                />
              </div>
            )}
          {onOpenEpicTerminal && menu?.isEpic && menu?.epicDirPath && menu?.epicFilePath && (
            <PositionedMenuItem
              onClick={() => {
                const dirPath = menu?.epicDirPath;
                const docPath = menu?.epicFilePath;
                if (dirPath && docPath) {
                  // Color/identity key is the bare dirName; tolerate a trailing slash.
                  const dirName = dirPath.replace(/\/+$/, "").split("/").pop() || dirPath;
                  onOpenEpicTerminal(dirName, docPath);
                }
                onClose();
              }}
            >
              Open epic workspace
            </PositionedMenuItem>
          )}
          {storyAuthoring && menu?.isEpic && (
            <PositionedMenuItem
              className="flex items-center gap-2"
              onClick={() => {
                const dir = menu.path;
                onClose();
                storyAuthoring.onNewStory(dir);
              }}
            >
              <FilePlus size={14} className="flex-shrink-0" />
              New story…
            </PositionedMenuItem>
          )}
          {/* SW-SB-15 · AC-1/AC-5: a STORY row's sprint items — one write each,
              instant + undoable (the toast lives in the shared action seam). The
              label is context-sensitive: "Add to…" when untagged, "Move to…" when
              already in a sprint. The row's current sprint is never re-offered. */}
          {sprint?.story &&
            (() => {
              const story = sprint.story;
              return (
                <>
                  {sprint.targets
                    .filter((t) => t.id !== story.current)
                    .map((t) => (
                      <PositionedMenuItem
                        key={t.id}
                        className="flex items-center gap-2"
                        onClick={() => {
                          onClose();
                          sprint.onSetStory(story.filePath, story.current, t.id);
                        }}
                      >
                        <CalendarPlus size={14} className="flex-shrink-0" />
                        {story.current ? `Move to ${t.label}` : `Add to ${t.label}`}
                      </PositionedMenuItem>
                    ))}
                  {story.current && (
                    <PositionedMenuItem
                      className="flex items-center gap-2"
                      onClick={() => {
                        const current = story.current;
                        onClose();
                        if (current) sprint.onRemoveStory(story.filePath, current);
                      }}
                    >
                      <CalendarMinus size={14} className="flex-shrink-0" />
                      Remove from sprint
                    </PositionedMenuItem>
                  )}
                </>
              );
            })()}
          {/* SW-SB-15 · AC-2: an EPIC-dir row's bulk add. Opens a confirm (never
              instant) that tags only unassigned stories by default, with a stronger
              re-point option. Gated on an active sprint existing + the epic holding
              stories; the confirm owns the count copy. */}
          {sprint?.epic && sprint.targets.some((t) => t.active) && sprint.epic.total > 0 && (
            <PositionedMenuItem
              className="flex items-center gap-2"
              onClick={() => {
                onClose();
                sprint.onBulkEpic();
              }}
            >
              <CalendarPlus size={14} className="flex-shrink-0" />
              Add to sprint&hellip;
            </PositionedMenuItem>
          )}
          {onExportPdf && menu && canExportToPdf(menu.path) && (
            <PositionedMenuItem
              onClick={() => {
                if (menu) onExportPdf(menu.path);
                onClose();
              }}
            >
              Export to PDF
            </PositionedMenuItem>
          )}
          {menu && canCreateCanvas(menu.path) && (
            <PositionedMenuItem
              className="flex items-center gap-2"
              onClick={() => {
                const docPath = menu.path;
                onClose();
                void onCreateCanvas(docPath);
              }}
            >
              <Shapes size={14} className="flex-shrink-0" />
              Create canvas
            </PositionedMenuItem>
          )}
          {attachCanvas && (
            <PositionedMenuItem
              className="flex items-center gap-2 disabled:opacity-50 disabled:hover:bg-transparent disabled:hover:text-inherit"
              disabled={attachCanvas.stories.length === 0}
              onClick={() => {
                onAttachOpen(attachCanvas.path, attachCanvas.stories);
                onClose();
              }}
            >
              <Paperclip size={14} className="flex-shrink-0" />
              Attach to story&hellip;
            </PositionedMenuItem>
          )}
          {menu && canExportCanvas(menu.path) && (
            <>
              <PositionedMenuItem
                className="flex items-center gap-2"
                onClick={() => {
                  const path = menu.path;
                  onClose();
                  void exportCanvas(path, "png");
                }}
              >
                <ImageDown size={14} className="flex-shrink-0" />
                Export as PNG
              </PositionedMenuItem>
              <PositionedMenuItem
                className="flex items-center gap-2"
                onClick={() => {
                  const path = menu.path;
                  onClose();
                  void exportCanvas(path, "svg");
                }}
              >
                <Download size={14} className="flex-shrink-0" />
                Export as SVG
              </PositionedMenuItem>
            </>
          )}
          {agentContext && <AgentContextItems {...agentContext} onClose={onClose} />}
          {/* The team's starter page: set it from any of the team's files. */}
          {menu && (
            <StarterPageMenuItems
              path={menu.path}
              onClose={onClose}
              isFile={actions.menuNodeKind ? actions.menuNodeKind === "file" : undefined}
              separator="before"
            />
          )}
          <PositionedMenuItem onClick={onCopyPath}>Copy path</PositionedMenuItem>
          {/* SW-VE-6: file-lifecycle gestures. All writes route through the gated
              content commands. SW-XS-20: a file from a parent team keeps every
              gesture — they act on the parent's file — under a note naming that
              team, plus "Copy into ‹team›" to give the team its own copy. */}
          {menu && inherited && scope?.sourceTeam && (
            <>
              <div
                className={`${contextMenuItemClass} flex cursor-default items-center gap-2 text-muted-foreground`}
                data-testid="nav-menu-inherited-note"
              >
                <CornerLeftUp size={14} className="flex-shrink-0" aria-hidden />
                From {scope.sourceTeam} · edits save there
              </div>
              {activeTeamName && (
                <PositionedMenuItem
                  className="flex items-center gap-2"
                  title={`Make ${activeTeamName}'s own copy at the same path. It replaces ${scope.sourceTeam}'s version for ${activeTeamName}.`}
                  onClick={() => {
                    const p = menu.path;
                    onClose();
                    void onCopyIntoTeam(p);
                  }}
                >
                  <CopyPlus size={14} className="flex-shrink-0" />
                  Copy into {activeTeamName}
                </PositionedMenuItem>
              )}
            </>
          )}
          {menu && (
            <>
              {/* New file — available on any tree row; a file row targets its
                  folder, a dir row itself, always in the active team (SW-XS-20). */}
              <PositionedMenuItem
                className="flex items-center gap-2"
                onClick={() => {
                  const p = menu.path;
                  onClose();
                  newFileFromRow(p);
                }}
              >
                <FilePlus size={14} className="flex-shrink-0" />
                New file&hellip;
              </PositionedMenuItem>
              {/* SW-CP-14 + SW-VE-6: file-node gestures (a basename with an
                  extension) so an epic/dir header isn't casually moved/deleted via
                  a right-click — directory structural ops are available via MCP. */}
              {menuFileGestures && (
                <>
                  {/* SW-CP-27: mark a structured config file (JSON/JSONC/JSON5/
                      YAML/TOML) so projection deep-merges it with the source file
                      instead of clobbering. Shown only for markable formats (the
                      orchestrator gates on CP-24's format list). */}
                  {mergeMark && (
                    <PositionedMenuItem
                      className="flex items-center gap-2"
                      onClick={() => {
                        onClose();
                        mergeMark.onToggle();
                      }}
                    >
                      <GitMerge size={14} className="flex-shrink-0" />
                      {mergeMark.marked ? "Unmark merge file" : "Mark as merge file"}
                    </PositionedMenuItem>
                  )}
                  <PositionedMenuItem
                    className="flex items-center gap-2"
                    onClick={() => {
                      const p = menu.path;
                      onClose();
                      onMove(p);
                    }}
                  >
                    <FolderInput size={14} className="flex-shrink-0" />
                    Move to&hellip;
                  </PositionedMenuItem>
                  <PositionedMenuItem
                    className="flex items-center gap-2"
                    onClick={() => {
                      const p = menu.path;
                      onClose();
                      onRename(p);
                    }}
                  >
                    <Pencil size={14} className="flex-shrink-0" />
                    Rename&hellip;
                  </PositionedMenuItem>
                  <PositionedMenuItem
                    className="flex items-center gap-2 text-[var(--destructive)]"
                    onClick={() => {
                      const p = menu.path;
                      onClose();
                      onDelete(p);
                    }}
                  >
                    <Trash2 size={14} className="flex-shrink-0" />
                    Delete
                  </PositionedMenuItem>
                </>
              )}
            </>
          )}
        </>
      )}
    </PositionedContextMenu>
  );
}

/**
 * SW-XS-15: the agent context edits for one row. A row in the agent's want set offers
 * "Remove from ‹Agent›'s context", one outside it "Add to ‹Agent›'s context"; with a repo
 * picked, each also comes "… for ‹Repo› only" (the agent's override for that repo). An edit
 * that cannot be made here stays listed, marked unavailable, with the reason under it.
 */
function AgentContextItems({
  context,
  rel,
  onClose,
}: {
  context: NavAgentContext;
  rel: string;
  onClose: () => void;
}) {
  // In the set: Remove. Out: Add. A folder with only some files in: both.
  const state = context.membership(rel).state;
  const actions: Array<"add" | "remove"> =
    state === "in" ? ["remove"] : state === "out" ? ["add"] : ["add", "remove"];
  const variants = actions.flatMap((action) => {
    const verb =
      action === "remove"
        ? `Remove from ${context.agentName}'s context`
        : `Add to ${context.agentName}'s context`;
    return [
      { action, repoOnly: false, label: verb },
      ...(context.repoName
        ? [{ action, repoOnly: true, label: `${verb} for ${context.repoName} only` }]
        : []),
    ];
  });
  return (
    <div role="group" aria-label={`${context.agentName}'s context`} data-testid="nav-agent-context">
      {variants.map(({ action, repoOnly, label }) => {
        const plan = context.plan(rel, action, repoOnly);
        const blocked = plan?.kind === "blocked" ? plan.reason : null;
        const note = blocked
          ? blocked
          : plan?.kind === "edit" && plan.makesExplicit
            ? `Makes ${context.agentName}'s context explicit for every launch.`
            : null;
        const Icon = action === "remove" ? BookMinus : BookPlus;
        return (
          <PositionedMenuItem
            key={label}
            className={`flex items-start gap-2 ${blocked ? "text-muted-foreground" : ""}`}
            aria-disabled={blocked ? true : undefined}
            data-testid={`nav-agent-context-${action}${repoOnly ? "-repo" : ""}`}
            onClick={() => {
              onClose();
              // An unavailable edit already says why, in the item itself.
              if (!blocked) context.edit(rel, action, repoOnly);
            }}
          >
            <Icon size={14} className="mt-0.5 flex-shrink-0" aria-hidden="true" />
            <span className="min-w-0">
              <span className="block">{label}</span>
              {note && <span className="block max-w-64 text-xs text-muted-foreground">{note}</span>}
            </span>
          </PositionedMenuItem>
        );
      })}
    </div>
  );
}

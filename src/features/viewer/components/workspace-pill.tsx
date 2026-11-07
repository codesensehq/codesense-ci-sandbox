// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useCallback, useId, useRef } from "react";
import {
  MachineMark,
  useRemoteMachineIdentity,
} from "@/features/machines/components/machine-location-badge";
import { Archive, Folder, FolderOpen, Loader2, AlertCircle, X } from "lucide-react";
import type { Workspace } from "@/shared/stores/viewer-store";
import {
  isWorkspaceArchived,
  selectArchivedAtMs,
  useWorkspaceArchiveStore,
} from "@/shared/stores/workspace-archive-store";
import { showsStoryStatus } from "@/shared/lib/workspace-kind-capabilities";
import { openWorkspaceDoc } from "../lib/story-doc-gesture";
import { useWorkspaceLabel } from "../hooks/use-workspace-label";
import { RepoGlyph } from "@/shared/components/repo-glyph";
import { StatusMark } from "@/shared/components/status-mark";
import { type DotScope, useScopeMark } from "../hooks/use-active-workspace-terminal-tabs";
import { OperationChip } from "@/features/shell/components/operation-chip";
import { SetupMark, slotsOf, useDisplayedLine } from "./setup-line";
import { describeSetup } from "../lib/workspace-setup-state";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { StageRail } from "@/shared/components/stage-rail";
import { StatusSelectorPopover } from "@/shared/components/status-selector-popover";
import type { PipelineStatus } from "@/shared/lib/pipeline-graph/types";
import { InfoTip } from "@/components/ui/tooltip";
import { closeTooltip, type CloseVerb } from "../lib/terminal-close";
import type { PillCrew } from "../hooks/use-workspace-crews";
import { CrewStack } from "./agent-identity";

const NO_SCOPE: DotScope = { sessionIds: [] };

/** How many of a workspace's agents wait on you, in words: "1 needs you", "2 need you". Every
 *  pill variant says it the same way. */
export function needsYouWords(count: number): string {
  return `${count} need${count === 1 ? "s" : ""} you`;
}

/** A workspace's name as a pill shows it: a root's source repo slug, else its shared label (the
 *  one the tile chrome shows). The tray's other words about a pill use the same rule. */
export function usePillLabel(ws: Workspace): string {
  const resolvedLabel = useWorkspaceLabel(ws);
  return ws.kind === "root" ? (ws.sourceRepoSlug ?? "~") : resolvedLabel;
}

export interface WorkspacePillProps {
  workspace: Workspace;
  isActive: boolean;
  /** The sessions the pill's status mark rolls up (its tabs, open and Hidden). */
  markScope?: DotScope;
  /** PT-10 live terminal count, shown as the running-terminals chip. */
  liveTerminalCount?: number;
  /** The verb the close `×` takes (Hide when running, End when idle) — names it
   *  in the tooltip + aria-label. Unprobed reads as Hide. */
  closeVerb?: CloseVerb;
  /** Hover of the close `×` (or focus of the pill) — lets the parent probe
   *  which verb it takes. */
  onCloseHover?: () => void;
  /** PT-10 render as a closed-but-running ("close, keep running") row —
   *  dimmed, status dot still lit, no close × (click reopens via `onSelect`). */
  dimmed?: boolean;
  /** T.3 D-3: 2-char source-repo glyph text (story pills only, multi-repo). */
  glyph?: string;
  /** SW-EW-1 deterministic repo-glyph color from the identity-color
   *  primitive. Story pills only; undefined falls back to the chrome default. */
  glyphColor?: string;
  /** Status popover open-state is owned by the parent so only one pill's
   *  popover is open at a time across both the WorkspaceBar and the panel. */
  isStatusPopoverOpen: boolean;
  onStatusPopoverOpenChange: (open: boolean) => void;
  /** `via.keyboard`: activated by Enter/Space (a click event with `detail` 0), which can
   *  never turn into a double-click. */
  onSelect: (workspaceId: string, via?: { keyboard: boolean }) => void;
  onClose: (workspaceId: string) => void;
  onContextMenu?: (workspaceId: string, x: number, y: number) => void;
  onStoryStatusChange?: (workspaceId: string, newStatus: PipelineStatus) => void;
  onOpenStoryDoc?: (storyFilePath: string) => void;
  /** Drag-reorder start handler (WorkspaceBar within-repo reorder, T.3 D-4).
   *  Omitted by the panel, which does not reorder pills. */
  onPointerDown?: (e: React.PointerEvent) => void;
  /** SW-EW-1: when true, tag the button `data-ws-row` so the management
   *  panel's arrow-key navigation can include it as a focusable row. */
  navRow?: boolean;
  /** The agents working here: the lead's face and piece, a stack of the others' faces (the
   *  one-row bar shows the lead alone), how many there are, and how many wait on you. Absent
   *  for a workspace of plain shells. */
  crew?: PillCrew;
  /** A crowded bottom bar's compact pill: a shorter label, and no faces (the crew stays in words
   *  for assistive tech). On the one-row bar it keeps its tally, the agent count included. The
   *  focused pill is never compact. */
  compact?: boolean;
  /** On the one-row bottom bar even a full pill caps its label (truncating, full name on
   *  hover) so one long name can't outgrow the strip. */
  oneLine?: boolean;
  /** Existing tray-owned hidden membership/menu control, composed with all card slots. */
  hiddenTabsControl?: React.ReactNode;
  /** Tray-owned decorative shortcut, allocated with the other identity marks. */
  shortcutHint?: React.ReactNode;
  /** More words the pill's selection button is described by (the keys that move it). */
  description?: string;
}

/**
 * The single, shared workspace pill — extracted from `WorkspaceBar` (SW-EW-1)
 * so the flat console bar and the epic-workspace management panel render
 * the identical affordance set: status icon + `StatusSelectorPopover`, AI notif
 * dot, repo-glyph chip, setup ring / error state, `OperationChip`, and
 * the hover-reveal close ×. Presentational: all state (active, popover-open,
 * drag) is owned by the parent.
 */
export function WorkspacePill({
  workspace: ws,
  isActive,
  markScope,
  liveTerminalCount = 0,
  closeVerb = "Hide",
  onCloseHover,
  dimmed = false,
  glyph,
  glyphColor,
  isStatusPopoverOpen,
  onStatusPopoverOpenChange,
  onSelect,
  onClose,
  onContextMenu,
  onStoryStatusChange,
  onOpenStoryDoc,
  onPointerDown,
  navRow,
  crew,
  compact = false,
  oneLine = false,
  hiddenTabsControl,
  shortcutHint,
  description,
}: WorkspacePillProps) {
  const handleClose = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      onClose(ws.id);
    },
    [onClose, ws.id],
  );

  const isRoot = ws.kind === "root";
  const Icon = isRoot ? Folder : FolderOpen;
  const crewWordsId = useId();
  const descriptionId = useId();
  // T.3 D-11: root label IS the source repo slug; non-root labels route through
  // the shared `useWorkspaceLabel` so they match the tile chrome — story
  // workspaces collapse alpha-prefixed/path-derived storyIds, and an epic
  // (integration) workspace shows its epic doc's real H1 title (matching the
  // tile chrome + HUD), falling back to the prettified epic-dir leaf until it
  // resolves — never the raw `integration:/…/epic.md` path id (SW-EW-35).
  const label = usePillLabel(ws);
  const groupName = useViewerStore(
    (s) =>
      (
        s.customGroupsByTeam[markScope && "teamKey" in markScope ? markScope.teamKey : ""] ?? []
      ).find((g) => g.workspaceIds.includes(ws.id))?.name,
  );

  // A workspace on a remote machine wears that machine's color — a faint wash
  // and ring — and a small server mark; its name is in the tooltip and the
  // accessible name. A workspace on This Mac looks as it always has.
  const remote = useRemoteMachineIdentity(ws);
  const remoteStyle: React.CSSProperties | undefined = remote
    ? {
        boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${remote.color} ${isActive ? 55 : 35}%, transparent)`,
        ...(isActive
          ? {}
          : {
              backgroundImage: `linear-gradient(color-mix(in srgb, ${remote.color} 14%, transparent), color-mix(in srgb, ${remote.color} 14%, transparent))`,
            }),
      }
    : undefined;

  // SW-EW-12 the status mark, rolled up over the workspace's tabs. On the
  // management card (navRow) it's the prominent LEADING "who needs me" headline; on
  // the flat console bar it stays the small TRAILING indicator. A minimized card
  // always has a mark; ordinary pills show every resolved agent state.
  const pillId = useId();
  const wordsId = useId();
  const labelClass = oneLine
    ? `cs-workspace-pill-label ${compact ? "max-w-[6rem]" : "max-w-[12rem]"} truncate`
    : compact
      ? "max-w-[5rem] truncate"
      : "truncate";
  // A capped label names itself in full on hover.
  const baseLabelTitle =
    ws.kind === "context"
      ? `${label} · context worktree · branch ${ws.contextWorktreeBranch ?? "unknown"} · ${liveTerminalCount} agents`
      : compact || oneLine
        ? label
        : undefined;
  const labelTitle = groupName ? `${baseLabelTitle ?? label}\nGroup: ${groupName}` : baseLabelTitle;
  const status = useScopeMark(markScope ?? NO_SCOPE);
  const hasDot = dimmed || (status.mark !== undefined && ws.status === "ready");
  // Agents waiting on you outrank whatever the tabs are doing: the one-row pill always draws the
  // needs mark for them, and its words say how many.
  const needs = oneLine && !!crew && crew.needCount > 0;
  const needsMark = needs ? { mark: crew!.mark, label: needsYouWords(crew!.needCount) } : undefined;
  const marked = hasDot || needs;
  // SW-WC-1 : a subtle muted "archived" badge whenever the operator has
  // marked this workspace archived — no color alarm, just a quiet marker so
  // the chat compose box's locked state has a visible companion in the pill.
  // Reads from the dedicated archive store so one event tick re-renders every
  // pill that cares (no per-pill IPC round-trip).
  const archivedAtMs = useWorkspaceArchiveStore((s) => selectArchivedAtMs(s, ws.id));
  const isArchived = isWorkspaceArchived(archivedAtMs);
  // SW-XS-10: the setup line's mark in the tray — the `--accent` ring while the
  // workspace is set up or its agent is prepared, a warning when a step failed.
  // The words ride along in the tooltip; the line itself is on the tile.
  const setupView = describeSetup(useDisplayedLine(slotsOf(ws)));
  const setupMark = setupView ? (
    <InfoTip label={setupView.text}>
      <span
        className="flex flex-shrink-0 items-center"
        data-testid={`workspace-tab-${ws.id}-setup`}
        data-tone={setupView.tone}
      >
        <SetupMark view={setupView} size={11} />
        <span className="sr-only">{setupView.text}</span>
      </span>
    </InfoTip>
  ) : null;
  const renderStatusMark = (
    extraClass: string,
    size: "sm" | "md",
    override?: { mark: NonNullable<typeof status.mark>; label: string },
  ) => {
    if (!hasDot && !override) return null;
    return (
      <>
        <StatusMark
          mark={override?.mark ?? status.mark ?? "ready"}
          size={size}
          label={override?.label ?? status.label}
          className={extraClass}
          data-testid={
            dimmed
              ? `workspace-tab-${ws.id}-soft-closed-dot`
              : `workspace-tab-${ws.id}-ai-status-dot`
          }
        />
        <span id={wordsId} hidden>
          {override?.label ?? status.words}
        </span>
      </>
    );
  };

  const statusLabel = useStatusTaxonomyStore(
    (s) => s.states.find((state) => state.id === ws.storyStatus)?.label ?? ws.storyStatus,
  );
  const CloseControl = oneLine ? "button" : "span";
  const select = (e: React.MouseEvent) => {
    // Portaled details preserve React ancestry but are not pill surfaces.
    if (!e.currentTarget.contains(e.target as Node)) return;
    // Double-click opens the workspace doc — a story file or an epic README
    // (+ recenters the pipeline); a single click selects. Shared gesture so
    // the pill + the split-tile chrome agree.
    if (e.detail === 2 && openWorkspaceDoc(ws, onOpenStoryDoc)) return;
    onSelect(ws.id, { keyboard: e.detail === 0 });
  };
  // A root workspace can't be hidden or ended: its menu opens only to restore its Hidden tabs
  // (on a strip too tight for their badge it is the one way to them).
  const hasMenu = !!onContextMenu && (!isRoot || !!hiddenTabsControl);
  // The menu key (or Shift+F10) opened the menu and no pointer has pressed since: the platform's
  // own `contextmenu` for that key (one no pointer press led to; a right-click always starts with
  // one) is its echo, not a second open, however late it comes.
  const keyMenuPending = useRef(false);
  const pointerPressed = () => {
    keyMenuPending.current = false;
  };
  const contextMenu = (e: React.MouseEvent) => {
    if (!e.currentTarget.contains(e.target as Node)) return;
    // Never the WebView's own menu over a pill, whether or not the pill has one.
    e.preventDefault();
    if (keyMenuPending.current) {
      keyMenuPending.current = false;
      return;
    }
    if (!hasMenu || !onContextMenu) return;
    const rect = e.currentTarget.getBoundingClientRect();
    onContextMenu(ws.id, e.clientX || rect.left, e.clientY || rect.bottom);
  };
  const state = (
    <span className={oneLine ? "cs-workspace-pill-state" : "contents"}>
      {navRow && !oneLine && renderStatusMark("flex-shrink-0", "md")}
      {ws.status === "creating" ? (
        (setupMark ?? (
          <Loader2
            size={11}
            className="flex-shrink-0 opacity-70 animate-spin"
            data-testid={`workspace-tab-${ws.id}-creating`}
          />
        ))
      ) : ws.status === "error" ? (
        <AlertCircle
          size={11}
          className="flex-shrink-0 text-red-400"
          data-testid={`workspace-tab-${ws.id}-error`}
        />
      ) : showsStoryStatus(ws) ? (
        <StatusSelectorPopover
          currentStatus={ws.storyStatus}
          triggerAriaLabel={oneLine ? `Change status for ${label}: ${statusLabel}` : undefined}
          onSelect={(newStatus) => {
            onStatusPopoverOpenChange(false);
            onStoryStatusChange?.(ws.id, newStatus);
          }}
          open={isStatusPopoverOpen}
          onOpenChange={onStatusPopoverOpenChange}
          disabled={!onStoryStatusChange}
        >
          <StageRail status={ws.storyStatus} size={12} />
        </StatusSelectorPopover>
      ) : (
        <Icon size={11} className="flex-shrink-0 opacity-70" />
      )}
    </span>
  );
  const glyphMark = (
    <RepoGlyph
      glyph={glyph}
      glyphColor={glyphColor}
      slug={ws.sourceRepoSlug}
      testId={`workspace-tab-${ws.id}-repo-glyph`}
    />
  );
  const identity = remote ? (
    // The InfoTip carries a capped label's full name (no second, native tooltip).
    <InfoTip label={labelTitle ? `${labelTitle} · lives ${remote.words}` : `Lives ${remote.words}`}>
      <span
        className={`flex min-w-0 items-center gap-1 ${oneLine ? "cs-workspace-pill-identity" : ""}`}
        data-testid={`workspace-tab-${ws.id}-machine`}
      >
        <MachineMark identity={remote} testId={`workspace-tab-${ws.id}-machine-mark`} />
        <span className={labelClass}>{label}</span>
      </span>
    </InfoTip>
  ) : (
    <span className={labelClass} title={labelTitle}>
      {label}
    </span>
  );
  const meta = (
    <span className={oneLine ? "cs-workspace-pill-meta contents" : "contents"}>
      {isArchived && (
        // SW-WC-1 : subtle archived marker — muted + no fill, so an
        // archived workspace reads as "quieted", not "something is wrong".
        <InfoTip label="Workspace archived">
          <span
            className="flex flex-shrink-0 items-center text-muted-foreground opacity-70"
            data-testid={`workspace-tab-${ws.id}-archived`}
            aria-label="Workspace archived"
          >
            <Archive size={11} aria-hidden="true" />
            <span className="sr-only">Workspace archived</span>
          </span>
        </InfoTip>
      )}
      {crew && compact && (
        <span id={crewWordsId} className="sr-only">
          {crew.summary}
        </span>
      )}
      {crew && !compact && (
        <span
          className="cs-agent-identity ml-0.5 flex flex-shrink-0 items-center gap-1 [--ag-crew-cut:var(--cs-workspace-card-bg,var(--ag-bg))]"
          data-testid={`workspace-tab-${ws.id}-crew`}
          title={oneLine ? crew.summary : undefined}
        >
          <CrewStack
            lead={{
              face: crew.leadFace,
              piece: crew.leadPiece,
              label: crew.leadLabel,
              hue: crew.hue,
              tag: crew.leadTag,
            }}
            // On the one-row bar the lead's face stands for the crew; the count says how many.
            crew={oneLine ? [] : crew.crew}
            more={oneLine ? 0 : crew.more}
            size={oneLine ? 16 : 15}
          />
          <span id={crewWordsId} className="sr-only">
            {crew.summary}
          </span>
          {/* On the one-row bar the count rides in the pill's tally, beside its status mark. */}
          {!oneLine && (
            <span
              className="text-[10px] tabular-nums text-muted-foreground"
              data-testid={`workspace-tab-${ws.id}-count`}
              aria-hidden
            >
              {crew.count}
            </span>
          )}
          {!oneLine && crew.needCount > 0 && (
            <span
              className="text-[10px] font-semibold tabular-nums text-[var(--cs-status-needs)]"
              data-testid={`workspace-tab-${ws.id}-need-you`}
              title={needsYouWords(crew.needCount)}
            >
              <span aria-hidden>{crew.needCount}</span>
              <span className="sr-only">{needsYouWords(crew.needCount)}</span>
            </span>
          )}
        </span>
      )}
    </span>
  );
  const runningCount = dimmed && liveTerminalCount > 0 && (
    // PT-4: right-aligned running-terminal count — the "still has terminals"
    // signal for a minimized card (replaces the old flat-green dot's meaning).
    <span
      data-testid={`workspace-tab-${ws.id}-terminal-count`}
      aria-label={`${liveTerminalCount} running terminal${liveTerminalCount === 1 ? "" : "s"}`}
      className="ml-auto tabular-nums text-[0.65rem] text-muted-foreground"
    >
      {liveTerminalCount}
    </span>
  );
  const close = !isRoot && !dimmed && !compact && (
    <InfoTip label={closeTooltip(closeVerb)}>
      <CloseControl
        data-testid={`workspace-tab-${ws.id}-close`}
        type={oneLine ? "button" : undefined}
        role={oneLine ? undefined : "button"}
        aria-label={`${closeVerb} workspace ${label}`}
        onPointerEnter={onCloseHover}
        onFocus={oneLine ? onCloseHover : undefined}
        tabIndex={oneLine ? undefined : -1}
        onPointerDown={oneLine ? (e) => e.stopPropagation() : undefined}
        onClick={handleClose}
        className="shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring text-muted-foreground hover:text-foreground transition-opacity ml-0.5"
      >
        <X size={12} />
      </CloseControl>
    </InfoTip>
  );
  const operation = !isRoot && ws.status === "ready" && (
    <span
      className={`flex items-center self-center ${oneLine ? "cs-workspace-pill-operation-one-line min-w-0 empty:hidden" : "shrink-0 pr-1"}`}
    >
      <OperationChip workspace={ws} />
    </span>
  );
  const selection = (children: React.ReactNode) => (
    <button
      type="button"
      id={pillId}
      aria-label={`Workspace ${label}${groupName ? `, Group ${groupName}` : ""}${remote ? `, ${remote.words}` : ""}${marked ? "," : ""}`}
      // Named by its own label, then the agent dot's words without their
      // ticking age ("Workspace SW-12, Needs approval").
      aria-labelledby={`${pillId} ${wordsId}`}
      aria-describedby={
        [crew ? crewWordsId : null, description ? descriptionId : null].filter(Boolean).join(" ") ||
        undefined
      }
      data-pill-select=""
      // Probe on focus as well as hover, so keyboard users get the right verb.
      onFocus={onCloseHover}
      {...(navRow ? { "data-ws-row": "" } : {})}
      data-machine={remote?.machineId}
      style={oneLine ? undefined : remoteStyle}
      className={
        // In `navRow` (management-panel) mode the pill is `flex-1` so it FILLS
        // its card → the whole card is clickable; the wrapping card owns the
        // raised surface + hover via `.cs-workspace-card`, so the pill must not
        // paint its own `hover:bg-accent` on top. The flat console bar (navRow
        // falsy) stays content-width with its pill-local hover.
        `group flex items-center gap-1.5 px-2 py-1 transition-colors min-w-0 rounded ${oneLine ? "cs-workspace-pill-selection" : ""} ${
          navRow ? "flex-1" : ""
        } ${
          isActive
            ? "text-foreground"
            : navRow
              ? "text-muted-foreground hover:text-foreground"
              : "text-muted-foreground hover:text-foreground hover:bg-accent"
        }`
        // SW-EW-12: no dim/mute on minimized cards — the whole panel is the
        // backgrounded set, so dimming every card signals nothing (it just
        // washes the panel out). The agent-state dot + count carry the state.
      }
      onClick={select}
      onPointerDown={onPointerDown}
      onPointerDownCapture={pointerPressed}
      onContextMenu={oneLine ? undefined : contextMenu}
      // The menu key (or Shift+F10) opens the pill's menu from the keyboard, under the pill: on a
      // strip too tight for the Hidden-tabs badge it is where those tabs are restored.
      onKeyDown={(e) => {
        if (e.key !== "ContextMenu" && !(e.shiftKey && e.key === "F10")) return;
        // The key never reaches the WebView's own menu, and opens the pill's once.
        e.preventDefault();
        e.stopPropagation();
        keyMenuPending.current = true;
        if (!hasMenu) return;
        const r = e.currentTarget.getBoundingClientRect();
        onContextMenu?.(ws.id, r.left, r.bottom);
      }}
    >
      {children}
      {description && (
        <span id={descriptionId} hidden>
          {description}
        </span>
      )}
    </button>
  );
  if (oneLine) {
    // As the Agents pane's card reads: the story's stage (its control) or the folder leads, and
    // after the name one tally carries what the agents are doing (the status mark) and how many
    // agents work here. While any of them waits on you the mark is the needs mark (drawn even
    // when the tabs show nothing), the tally takes the needs-you ink, and the pill's words say
    // how many need you. A compact pill keeps the tally; its face goes first.
    const tally = (marked || crew) && (
      <span
        className="cs-workspace-pill-tally"
        data-has-needs={needs || undefined}
        data-testid={`workspace-tab-${ws.id}-tally`}
      >
        {renderStatusMark("", "sm", needsMark)}
        {crew && (
          <span className="tabular-nums" data-testid={`workspace-tab-${ws.id}-count`} aria-hidden>
            {crew.count}
          </span>
        )}
        {needsMark && <span className="sr-only">{needsMark.label}</span>}
      </span>
    );
    return (
      <div
        className={`cs-workspace-pill-one-line group ${remote ? "cs-workspace-pill-remote" : ""}`}
        style={remoteStyle}
        data-has-hidden={hiddenTabsControl ? "true" : undefined}
        onPointerDownCapture={pointerPressed}
        onClick={(e) => {
          // Decorative state space retains the selection/document gesture;
          // independent native controls handle their own events exactly once.
          if (
            e.currentTarget.contains(e.target as Node) &&
            !(e.target as Element).closest("button, [role=button]")
          )
            select(e);
        }}
        // A right-click anywhere on the pill opens its menu, as the menu key does, whatever parts a
        // tight strip has shed; a part that opened a menu of its own keeps it.
        onContextMenu={(e) => {
          if (!e.defaultPrevented) contextMenu(e);
        }}
      >
        <span
          className="cs-workspace-pill-state-slot"
          onPointerDown={(e) => {
            if ((e.target as Element).closest("button")) e.stopPropagation();
          }}
        >
          {state}
        </span>
        {selection(
          <>
            {glyphMark}
            {identity}
            {tally}
            {meta}
            {ws.status === "ready" && setupMark}
            {runningCount}
          </>,
        )}
        {hiddenTabsControl && (
          <span
            className="cs-workspace-pill-hidden-slot"
            onPointerDown={(e) => {
              if ((e.target as Element).closest("button")) e.stopPropagation();
            }}
          >
            {hiddenTabsControl}
          </span>
        )}
        <span className="cs-workspace-pill-actions" onPointerDown={(e) => e.stopPropagation()}>
          {operation}
        </span>
        {/* One slot at the end, never squeezed: the ⌘ number at rest; hover or focus swaps in the
            close ×. */}
        {(shortcutHint || close) && (
          <span
            className="cs-workspace-pill-end"
            data-has-close={close ? "" : undefined}
            onPointerDown={(e) => e.stopPropagation()}
          >
            {shortcutHint}
            {close}
          </span>
        )}
      </div>
    );
  }
  return (
    <>
      {selection(
        <>
          {state}
          {glyphMark}
          {identity}
          {meta}
          {!navRow && renderStatusMark("ml-0.5", "sm")}
          {runningCount}
          {ws.status === "ready" && setupMark}
          {close}
        </>,
      )}
      {operation}
      {hiddenTabsControl}
    </>
  );
}

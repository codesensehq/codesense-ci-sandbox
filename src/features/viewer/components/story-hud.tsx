// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { WorkspaceMachineMark } from "@/features/machines/components/machine-location-badge";
import { FolderKanban, Maximize, Minimize } from "lucide-react";

import type { Workspace } from "@/shared/stores/viewer-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { InfoTip } from "@/components/ui/tooltip";
import { StageRail } from "@/shared/components/stage-rail";
import { useStoryDetails } from "@/shared/lib/pipeline-graph/use-story-details";
import { useSummaryHeadings } from "@/shared/hooks/use-summary-headings";
import { MarkdownViewer } from "@/features/dashboard/components/markdown-viewer";
import { AssigneeAvatar } from "@/shared/components/person-avatar";
import { shownPaneFrame } from "../lib/shown-pane-frame";
import { useScopeActive } from "@/shared/lib/warm-scopes";

/** A small icon button with a hotkey tooltip, shared by the HUD's controls. */
function HudIconButton({
  testId,
  label,
  tooltip,
  onClick,
  children,
}: {
  testId: string;
  label: string;
  tooltip: React.ReactNode;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <InfoTip label={tooltip} side="top">
      <button
        type="button"
        data-testid={testId}
        aria-label={label}
        onClick={onClick}
        className="ml-auto inline-flex shrink-0 items-center justify-center rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
      >
        {children}
      </button>
    </InfoTip>
  );
}

interface StoryHudProps {
  hostTeamKey?: string;
  workspace: Workspace;
  /** Is THIS tile the focused one — Escape only dismisses the focused tile's
   *  expanded panel (consistent with the focus-driven hotkeys). */
  isFocused?: boolean;
  /** SW-EW-28: synthetic repo key driving the file-refresh live-reload signal so
   *  the expanded doc hot-reloads on an edit (the SW-SC-4 gate). */
  repoPath?: string;
  /** SW-EW-28: a tile is zoomed (effectiveZoomId set). While zoomed, Escape is
   *  owned by zoom-restore — the HUD defers so one Escape doesn't both un-zoom
   *  and hide the HUD. Defaults false. */
  zoomActive?: boolean;
}

/**
 * SW-EW-28: a per-tile, story-context overlay floated over the terminal pane
 * (zero reflow — an absolute child of the tile's terminal-body box). Visibility
 * and size are per-workspace (`storyHudShown` / `storyHudExpanded`) so each
 * epic/story tile is independent; the show/hide TOGGLE lives in the tile chrome
 * (next to the story-id), not here. Two sizes:
 *
 *  • Compact card — a pipeline-story-node-style card: the lifecycle status icon
 *    (or the epic kanban icon) + the story's `## Story` summary (or an epic's
 *    `## Purpose`), in a real-height, scrollable card. Title is omitted (it's
 *    already in the chrome titlebar).
 *  • Expanded — the WHOLE story / epic README rendered by REUSING the markdown
 *    viewer, as an INSET floating panel that reads as a temporary peek on top of
 *    the CLI. Escape (on the focused tile) dismisses it back to the chrome icon.
 *
 * Git branch is intentionally NOT shown here — it belongs in the terminal's own
 * statusline (a separate follow-up); the HUD stays purely story-focused.
 */
export function StoryHud({
  workspace: ws,
  hostTeamKey,
  isFocused = false,
  repoPath,
  zoomActive = false,
}: StoryHudProps) {
  const shown = useViewerStore((s) => s.storyHudShown[ws.id] ?? false);
  const expanded = useViewerStore((s) => s.storyHudExpanded[ws.id] ?? false);
  const toggleSize = useViewerStore((s) => s.toggleStoryHudSize);
  const toggleShown = useViewerStore((s) => s.toggleStoryHudShown);
  const sizeHotkey = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-story-hud-size"));

  const isEpic = ws.kind === "epic";
  // The doc the expanded panel renders + the source for the compact summary: the
  // story doc for a story tile, the epic README (`epicDocPath`, SW-EW-15) for an
  // epic/integration tile.
  const docPath = isEpic ? ws.epicDocPath : ws.storyFilePath;
  // SW-EW-32: a single, lazy, module-cached, watcher-invalidated body fetch for
  // BOTH tile kinds — the same hook the pipeline story node uses, now keyed off
  // the configured summary headings (first match wins). This unifies the epic
  // and story cards on one path: the epic card no longer needs the tree's
  // parent-resolved purpose plumbing (`epicPurpose`/`epicPurposeById`), and the
  // epic summary now honors the configured list too. The summary falls back to
  // the doc's H1 title so the card is never empty (SW-EW-28 behavior preserved:
  // the rework doc has no `## Story`; the git-sync-workbench README had no
  // `## Purpose`).
  const hudRef = useRef<HTMLDivElement>(null);
  const summaryHeadings = useSummaryHeadings();
  const details = useStoryDetails(docPath, summaryHeadings);
  const description =
    details.data?.storyText || details.data?.titleMeta || details.data?.title || undefined;
  const descLoading = details.loading && !description;

  // Escape (on the focused tile) hides the HUD from EITHER size — same
  // per-workspace state the chrome toggle drives. Gated so it never steals
  // Escape from the terminal (vim/less/TUIs) or a text input: those keep their
  // own Escape; the HUD only reacts when focus is elsewhere (e.g. just after the
  // chrome toggle, or while reading the expanded panel).
  // A warm team's HUD stays mounted but owns no keys or pointer presses.
  const ownsKeys = useScopeActive(hostTeamKey);
  useEffect(() => {
    // While a tile is zoomed, Escape belongs to zoom-restore (SW-EW-4) — defer so
    // one keypress doesn't both un-zoom and hide the HUD.
    if (!shown || !isFocused || zoomActive || !ownsKeys) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (t?.closest?.(".xterm")) return;
      const hadFocus = !!t && !!hudRef.current?.contains(t);
      toggleShown(ws.id, false);
      // Focus goes back to the control that opened it, never to the page body.
      if (hadFocus) focusHudToggle(ws.id);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [shown, isFocused, zoomActive, ownsKeys, toggleShown, ws.id]);

  // The expanded doc is a temporary peek: a press anywhere outside it (other
  // than its own caption toggle, which toggles) folds it back to the compact
  // card. The compact card stays put while you work — it is the tile's
  // standing context, closed by its toggle or Esc.
  useEffect(() => {
    if (!shown || !expanded || !ownsKeys) return;
    const onDown = (e: PointerEvent) => {
      const t = e.target as Element | null;
      if (!t || hudRef.current?.contains(t)) return;
      // Its own toggle toggles; another tile's toggle is just "outside".
      const toggle = t.closest?.('[data-testid^="terminal-grid-caption-hud-"]');
      if (toggle && toggle.closest("[data-pane-id]")?.getAttribute("data-pane-id") === ws.id)
        return;
      if (toggle && !toggle.closest("[data-pane-id]")) return;
      toggleSize(ws.id, false);
    };
    document.addEventListener("pointerdown", onDown, true);
    return () => document.removeEventListener("pointerdown", onDown, true);
  }, [shown, expanded, ownsKeys, toggleSize, ws.id]);

  // Hidden → nothing floats over the terminal; the chrome toggle is the only
  // affordance (TerminalTileChrome).
  if (!shown) return null;

  if (expanded && docPath) {
    return (
      <StoryHudExpanded
        panelRef={hudRef}
        workspace={ws}
        hostTeamKey={hostTeamKey}
        docPath={docPath}
        repoPath={repoPath}
        sizeHotkey={sizeHotkey}
        onCollapse={() => toggleSize(ws.id, false)}
      />
    );
  }

  return (
    <div
      ref={hudRef}
      data-testid={`story-hud-${ws.id}`}
      // Below the tab captions (`--tile-caption-height` + a gap), never over
      // them: the caption's own Story toggle sits at the top right, and a card
      // covering it could not be closed the way it was opened.
      className="pointer-events-auto absolute right-2 top-[calc(var(--tile-caption-height,32px)+8px)] z-10 flex max-h-[210px] w-[300px] select-text flex-col gap-1.5 overflow-hidden rounded-md border border-[var(--border)] bg-[var(--popover)] px-3 py-2 text-[var(--popover-foreground)] opacity-95 shadow-md"
    >
      <div className="flex select-none items-center gap-1.5">
        <HudIcon workspace={ws} />
        <span className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
          {isEpic ? "Epic" : "Story"}
        </span>
        <WorkspaceMachineMark workspace={ws} />
        {/* SW-ID-6 AC-1: the story's assignee avatar in its header (story tiles
            only — an epic has no assignee). Reconciled onto the workspace from the
            scan, so it's already resident — no fetch. */}
        {!isEpic && ws.storyAssignedTo && (
          <AssigneeAvatar login={ws.storyAssignedTo} size="sm" className="ml-auto shrink-0" />
        )}
        {docPath && (
          <HudIconButton
            testId={`story-hud-expand-${ws.id}`}
            label="Expand story HUD"
            tooltip={<>Expand · {sizeHotkey}</>}
            onClick={() => toggleSize(ws.id, true)}
          >
            <Maximize size={13} />
          </HudIconButton>
        )}
      </div>
      <div className="overflow-y-auto whitespace-pre-line text-xs leading-relaxed">
        {description ? (
          description
        ) : (
          <span className="italic text-muted-foreground">
            {descLoading ? "Loading…" : "No summary."}
          </span>
        )}
      </div>
    </div>
  );
}

/** Focus the caption control that toggles this tile's story card (the shown
 *  tab's caption first). */
function focusHudToggle(workspaceId: string): void {
  if (typeof document === "undefined") return;
  const frame = shownPaneFrame(workspaceId);
  const toggle =
    frame?.querySelector<HTMLElement>(
      '[data-pane-active] [data-testid^="terminal-grid-caption-hud-"]',
    ) ?? frame?.querySelector<HTMLElement>('[data-testid^="terminal-grid-caption-hud-"]');
  toggle?.focus({ preventScroll: true });
}

/** The pipeline-node status glyph — the stage rail for a story,
 *  the `FolderKanban` kanban icon for an epic/integration tile. */
function HudIcon({ workspace: ws }: { workspace: Workspace }) {
  if (ws.kind === "epic") {
    return <FolderKanban size={13} className="shrink-0 text-muted-foreground" />;
  }
  return ws.storyStatus ? <StageRail status={ws.storyStatus} /> : null;
}

/**
 * SW-EW-28: the expanded view — the WHOLE story (or an epic's README) rendered
 * by REUSING the markdown viewer (no fork), as an INSET floating panel. The
 * `inset-3` margin + rounded border + shadow make it read as a temporary peek ON
 * the terminal, not part of it; the terminal stays visible at the margins.
 * Escape (on the focused tile) dismisses it back to the chrome icon. Shares the
 * SW-SC-4 reload gate (via the file-refresh signal keyed on `repoPath`) so the
 * rendered doc stays live on edits.
 */
function StoryHudExpanded({
  panelRef,
  workspace: ws,
  hostTeamKey,
  docPath,
  repoPath,
  sizeHotkey,
  onCollapse,
}: {
  panelRef: React.RefObject<HTMLDivElement | null>;
  workspace: Workspace;
  hostTeamKey?: string;
  docPath: string;
  repoPath: string | undefined;
  sizeHotkey: string;
  onCollapse: () => void;
}) {
  // SR-9: the expanded HUD renders a live `MarkdownViewer` of the open doc, so it
  // keys on `contentRefreshKeys` (the viewer content-version), NOT the
  // `fileRefreshKeys` scan counter — otherwise a counter-only ContentLayerEvent
  // materialize storm would re-read this doc ~1×/sec (the exact SR-9 defect) via
  // the viewer's null-changedPaths broad-refresh fallback.
  const refreshKey =
    useFileRefreshStore((s) => (repoPath ? s.contentRefreshKeys[repoPath] : undefined)) ?? 0;
  const changedPaths = useFileRefreshStore((s): Set<string> | null =>
    repoPath ? (s.lastChangedContextFile[repoPath] ?? null) : null,
  );

  const isEpic = ws.kind === "epic";

  return (
    <div
      ref={panelRef}
      data-testid={`story-hud-expanded-${ws.id}`}
      // Inset like a peek, but starting under the captions so their toggles
      // stay reachable to close it.
      className="pointer-events-auto absolute inset-x-3 bottom-3 top-[calc(var(--tile-caption-height,32px)+8px)] z-10 flex flex-col overflow-hidden rounded-lg border border-[var(--border)] bg-[var(--popover)] text-[var(--popover-foreground)] opacity-95 shadow-xl"
    >
      <div className="flex select-none items-center gap-1.5 border-b border-[var(--border)] px-2.5 py-1.5 text-xs">
        <HudIcon workspace={ws} />
        <span className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">
          {isEpic ? "Epic" : "Story"}
        </span>
        <HudIconButton
          testId={`story-hud-collapse-${ws.id}`}
          label="Collapse story HUD"
          tooltip={<>Collapse · {sizeHotkey} · Esc to hide</>}
          onClick={onCollapse}
        >
          <Minimize size={13} />
        </HudIconButton>
      </div>
      <div className="min-h-0 flex-1">
        <MarkdownViewer
          filePath={docPath}
          refreshKey={refreshKey}
          changedPaths={changedPaths}
          teamContentPath=""
          hostTeamKey={hostTeamKey}
        />
      </div>
    </div>
  );
}

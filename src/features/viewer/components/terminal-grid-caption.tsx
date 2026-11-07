// SPDX-License-Identifier: MIT
import { useRunningAgent } from "@/features/viewer/lib/hand-agents";
import { useEffect, useId, useRef, type KeyboardEvent, type MouseEvent } from "react";
import { AlarmClock, Code, GitCompareArrows, Inbox, PanelTopOpen, X } from "lucide-react";
import type { TerminalTab } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { InfoTip } from "@/components/ui/tooltip";
import { isMac } from "@/shared/lib/shortcuts/keystroke";
import { tabCloseTooltip, useCloseVerb } from "../lib/terminal-close";
import { ScopedStatusMark } from "./session-status-mark";
import { MiddleTruncate } from "./middle-truncate";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";
import { useSessionPrompt } from "@/features/viewer/hooks/use-session-prompt";
import { useMachine } from "@/features/machines/stores/machines-store";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { toast } from "sonner";
import { abbreviatePromptPath } from "../lib/prompt-path";
import { cliHostRoot } from "../lib/cli-host";
import { FOCUS_PANE_ATTR } from "../lib/focus-indicator";
import { GitPromptSegment, MachineChip, PromptPathLabel } from "./terminal-prompt-segment";
import { setSourceDiffReturnFocus, takeSourceDiffReturnFocus } from "./source-diff-return-focus";
import { useSessionProgress } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { activityLine } from "./agents-tree-view";
import {
  useSourceDiffShown,
  useWorktreeEditorOpen,
  useStoryHudShown,
  toggleSourceDiffShownAction,
  toggleWorktreeEditorOpenAction,
  toggleStoryHudShownAction,
  type Workspace,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import {
  closeAgentInboxRail,
  openAgentInboxRail,
  useAgentInboxRailStore,
} from "@/features/viewer/stores/agent-inbox-rail-store";
import { useTabInboxCounts } from "@/features/inbox/hooks/use-tab-inbox-unread";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { cn } from "@/lib/utils";
import { useTabTimerAgent } from "@/features/viewer/hooks/use-tab-timer-agent";
import { useTabTimers } from "@/features/viewer/hooks/use-session-timers";
import { useSessionTimersStore } from "@/features/viewer/stores/session-timers-store";
import { SessionTimerChip } from "./session-timers-popover";
import { wakeName } from "@/features/viewer/lib/wake-marker";
import { useLingering } from "@/features/viewer/hooks/use-lingering";

/** "Woken by" fades out over --ib-motion-fade (200 ms). */
const WOKEN_FADE_OUT_MS = 200;

export interface TerminalGridCaptionProps {
  tab: TerminalTab;
  /** This cell owns the keyboard (the workspace's active tab). */
  isActive: boolean;
  /** The terminal theme's chrome colours, so the caption reads as the cell's own bar. */
  background: string;
  foreground: string;
  /** Make this cell the focused one. */
  onFocusCell: () => void;
  /** Zoom this cell: it fills the tile, the other cells kept running behind it. */
  onZoomCell: () => void;
  /** The cell's default close (Hide when running, End when idle). Resolves
   *  false when nothing closed (End's ask cancelled). */
  onClose: () => void | Promise<boolean>;
  /** The cell the keyboard moves to when this one is closed from its caption. */
  neighbourTabId?: string;
  /** SW-TP CAP-6: the workspace that owns this tab. Required to wire the
   *  Editor / Diff / HUD / Inbox icon cluster into store actions. */
  workspace: Workspace;
  /** SW-TP CAP-6: the team key for workspace-scoped store actions. */
  teamKey: string;
  /** SW-TP CAP-12: the owning tile's focus indicator (`"on"` / `"dim"` /
   *  undefined). The caption carries the focus-blue bottom-border now that the
   *  chrome row drops it — only the active-tab caption in a focused tile
   *  shows the signal. */
  tileFocusIndicator?: "on" | "dim";
}

/** Focus the caption of the cell showing `tabId`, else its terminal. */
function focusCaptionOf(tabId: string): void {
  const root = cliHostRoot();
  if (!root) return;
  const caption = root.querySelector<HTMLElement>(
    `[data-testid="terminal-grid-caption-focus-${CSS.escape(tabId)}"]`,
  );
  if (caption) caption.focus();
  else focusCellTerminal(root.querySelector(`[data-pane-id="${CSS.escape(tabId)}"]`));
}

/** The terminal input of the cell holding `el`. */
function cellTerminalOf(el: Element | null): HTMLElement | null {
  return el?.closest(`[${FOCUS_PANE_ATTR}]`)?.querySelector<HTMLElement>(".xterm textarea") ?? null;
}

/** Put the keyboard in the terminal of the cell holding `el`. */
function focusCellTerminal(el: Element | null): void {
  cellTerminalOf(el)?.focus();
}

/**
 * The slim bar on top of a side-by-side cell: the tab's name (middle-truncated,
 * so names sharing a prefix stay apart), its status dot — idle included, so every
 * cell is badged — and a mouse-only × taking the tab's close verb.
 *
 * Double-click rule: a click on the caption focuses the cell, a double-click
 * zooms it — the caption's one double-click meaning, as a tile's chrome bar zooms
 * the tile. Renaming stays on the tab label.
 *
 * Keyboard: the caption's name is one tab stop — Enter / Space focus the cell's
 * terminal, Delete (⌫ on macOS) closes it with its close verb. The × stays out of
 * the tab order (the closable-tab pattern), its key named in its tooltip.
 */
export function TerminalGridCaption({
  tab,
  isActive,
  background,
  foreground,
  onFocusCell,
  onZoomCell,
  onClose,
  neighbourTabId,
  workspace: ws,
  teamKey,
  tileFocusIndicator,
}: TerminalGridCaptionProps) {
  // SW-VE-12: Editor renders on every folder-rooted kind (`sourceCapable`,
  // TRUE on all 5 live kinds). The Diff icon still gates on `diffCapable`
  // (ONLY `root`/`story`/`generic`) — `epic`/`git-sync` cwd dirs live
  // inside their own git parent (the context repo), but a diff of the
  // enclosing repo in those tabs is misleading. The gate is NOT
  // resolver-based (`!!repo?.isRoot`) anymore: the Diff icon lights on
  // kind alone and the overlay's "No git repo at this path." body state
  // handles cwds that happen not to be a working tree.
  const caps = getKindCapabilities(ws.kind);
  const canDiff = caps.diffCapable;
  const canEditor = caps.sourceCapable;
  const diffOpen = useSourceDiffShown(tab.sessionId);
  const editorOpen = useWorktreeEditorOpen(tab.sessionId);
  const hudShown = useStoryHudShown(ws.id);
  const runningAgent = useRunningAgent(tab.sessionId);
  const inboxOpen = useAgentInboxRailStore((s) => !!s.bySession[tab.sessionId]);
  const inboxCounts = useTabInboxCounts(tab.sessionId);
  const inboxUnread = inboxCounts.direct + inboxCounts.group;
  const diffShortcut = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-source-diff"));
  const editorShortcut = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-code-editor"));
  const hudShortcut = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-story-hud"));
  const inboxShortcut = formatKeystrokeDisplay(useEffectiveShortcut("viewer.toggle-agent-inbox"));
  const toggleDiff = () =>
    toggleSourceDiffShownAction({ sessionId: tab.sessionId, workspaceId: ws.id, teamKey });
  const toggleEditor = () =>
    toggleWorktreeEditorOpenAction({ sessionId: tab.sessionId, workspaceId: ws.id, teamKey });
  const toggleHud = () => toggleStoryHudShownAction(ws.id);
  const toggleInbox = () => {
    if (inboxOpen) closeAgentInboxRail(tab.sessionId);
    else openAgentInboxRail(tab.sessionId);
  };

  // SW-TP CAP-11 / CAP-13: the live cwd for this tab's session, replacing the
  // duplicated agent name in the caption's identity slot. Lazy on-mount resolve;
  // when the host doesn't yet know (fresh session, OSC 7 not emitted yet), fall
  // back to the tab name so the slot is never empty.
  // The prompt segment: cwd, the session account's home and its checkout's
  // Git status, read on the host that runs the session (a remote tab's
  // Machine included) and refreshed on OSC 133 marks / a visible-only poll.
  const captionRef = useRef<HTMLDivElement>(null);
  const prompt = useSessionPrompt(tab.sessionId, captionRef);
  const liveCwd = prompt?.cwd?.trim() ? prompt.cwd : null;
  const promptPath = liveCwd ? abbreviatePromptPath(liveCwd, prompt?.home) : null;
  const git = prompt?.git ?? null;
  const machine = useMachine(tab.origin?.machineId ?? "");
  const machineLabel = tab.origin ? machine?.label || tab.origin.machineId : null;
  const copyText = (text: string, what: string) => {
    const clipboard = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
    if (!clipboard) {
      toast.error(`Couldn't copy the ${what}`);
      return;
    }
    void clipboard
      .writeText(text)
      .then(() => toast.success(`Copied ${what}`))
      .catch(() => toast.error(`Couldn't copy the ${what}`));
  };
  // The git segment opens the tab's source diff (its existing toggle, only
  // ever opening from here) when the tab has one and it isn't already open —
  // an open diff leaves the segment a plain readout, not a dead button.
  const openDiff = canDiff && !diffOpen ? toggleDiff : undefined;
  // Set when the caption menu's "Open diff" closes the menu: the overlay
  // focuses itself, and the menu's close must not hand focus back to the
  // caption (j/k would then reach the hidden terminal).
  const menuOpenedDiffRef = useRef(false);
  // What had focus before the menu opened: the diff opened from the menu
  // returns there on close (the menu item focused at its opening unmounts).
  const preMenuFocusRef = useRef<Element | null>(null);
  // Where focus goes once the menu has closed on "Open diff", when nothing took it.
  const diffReturnRef = useRef<HTMLElement | null>(null);
  // A return target nothing consumed is dropped with the caption, never holding its element.
  useEffect(() => () => void takeSourceDiffReturnFocus(tab.sessionId), [tab.sessionId]);
  // SW-TP CAP-7: the per-tab progress note from the last signal_progress emit.
  // Scalar selector so only this caption re-renders on a tick for this session.
  const progress = useSessionProgress(tab.sessionId);
  const progressNote = activityLine(progress);
  const progressSettled = progress?.settled === true;
  // SW-AGR-6: the tab's timers (an agent tab's, saved or unnamed, local or
  // remote) and, for 10 s after a delivery, "Woken by ‹name›" in the progress slot.
  const timerAgent = useTabTimerAgent(tab.sessionId);
  const timers = useTabTimers(tab);
  const wokenNow = useSessionTimersStore((s) => (timers.ended ? undefined : s.woken[timers.key]));
  // It fades out over --ib-motion-fade before the progress note returns.
  const { value: woken, leaving: wokenLeaving } = useLingering(wokenNow, WOKEN_FADE_OUT_MS);
  const timerRows = timerAgent ? timers.rows : [];
  const idBase = useId();
  const nameId = `${idBase}-name`;
  const statusId = `${idBase}-status`;
  const scope = { sessionIds: [tab.sessionId] };
  const { verbFor, probe } = useCloseVerb();

  const focus = (e: MouseEvent<HTMLElement>) => {
    onFocusCell();
    focusCellTerminal(e.currentTarget);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
    if (e.key === "Delete" || (e.key === "Backspace" && isMac())) {
      e.preventDefault();
      e.stopPropagation();
      // Closed from the keyboard: the keyboard moves on to the neighbouring cell.
      void Promise.resolve(onClose()).then((closed) => {
        if (closed !== false && neighbourTabId) focusCaptionOf(neighbourTabId);
      });
    }
  };

  // SW-IB1-D1: the caption's bottom border carries the focus-line rule.
  // The 2px underline REPLACES the 1px baseline — no stacking. Exactly one
  // caption on screen may carry the 2px underline: the active tab in a focused
  // tile. Driven by `tileFocusIndicator`, which the mount site threads only
  // for that caption. Color is supplied by `.cs-focus-line` in
  // `styles/focus-indicator.css` (default border-neutral; on → focus-blue;
  // dim → focus-blue at reduced opacity) — the one focus-blue token is not
  // inlined here (the "no blue outside focus-visible" invariant test guards
  // against stray blues). Width is swapped via Tailwind so the DESIGN rule
  // "2px eats 1px" holds: we never emit `border-b` and `border-b-2` together.
  const hasFocusLine = !!tileFocusIndicator;
  const borderBottomClass = hasFocusLine ? "border-b-2" : "border-b";
  return (
    <div
      data-surface="terminal"
      data-testid={`terminal-grid-caption-${tab.id}`}
      data-active={isActive ? "true" : "false"}
      // `cs-focus-line` paints the bottom-border color per the
      // `data-focus-indicator` attribute. See the `borderBottomClass` above
      // for the width swap (2px replaces 1px; no stacking).
      className={cn(
        // Height is the shared `--tile-caption-height` token (inbox-tokens.css).
        // The tab's agent inbox pane sits in the region below the caption, so
        // it starts under it by construction.
        "cs-focus-line group @container/caption relative box-border flex h-[var(--tile-caption-height)] flex-shrink-0 select-none items-center text-xs",
        borderBottomClass,
      )}
      data-focus-indicator={tileFocusIndicator}
      data-focus-line={hasFocusLine ? "on" : undefined}
      ref={captionRef}
      style={{ backgroundColor: background, color: foreground }}
      onClick={focus}
      onDoubleClick={(e) => {
        onZoomCell();
        focusCellTerminal(e.currentTarget);
      }}
    >
      <ContextMenu
        onOpenChange={(open) => {
          if (open) preMenuFocusRef.current = document.activeElement;
        }}
      >
        {/* No hover card on the caption's name (#1973): it popped over the tile on
            every pass of the pointer. The full path is the button's accessible
            description, and right-click copies it. */}
        <ContextMenuTrigger asChild>
          <button
            type="button"
            data-testid={`terminal-grid-caption-focus-${tab.id}`}
            aria-labelledby={`${nameId} ${statusId}`}
            aria-current={isActive ? "true" : undefined}
            aria-description={liveCwd ?? undefined}
            aria-keyshortcuts={isMac() ? "Backspace" : "Delete"}
            onKeyDown={onKeyDown}
            onFocus={() => probe(tab.id, [tab.sessionId])}
            // A light ring, not the focus blue: the cell's outline already shows
            // the focused pane in blue, and one blue per level is the rule.
            // SW-IB1-D1: bar left pad = 8px (--ib-space-3). Right pad is 0: the
            // progress-note slot carries its own `pl-2` (8px) and the icon
            // cluster its own `pr-1` (4px), so the identity cluster has clean
            // 8px separation on either side without doubling. Inactive opacity
            // 0.7 lifts to 1.0 on caption-level (group) hover — DESIGN § cwd
            // rule names this "cell hover," and the caption IS the cell bar.
            className={`flex h-full min-w-0 shrink items-center gap-1.5 pl-2 text-left outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring ${
              isActive ? "" : "opacity-70 group-hover:opacity-100"
            } ${progressNote || woken ? "max-w-[50%]" : git ? "max-w-[65%]" : "flex-1"}`}
          >
            <ScopedStatusMark
              scope={scope}
              wordsId={statusId}
              className="flex-shrink-0"
              data-testid={`terminal-grid-caption-status-${tab.id}`}
            />
            {/* SW-IB1-D1: cwd typography — ib-mono-sm (mono 12/16/500) when the
              live cwd is known; sans 12/500 fallback when it isn't (the slot
              is never empty). Weight 600 when this tab is the active one. */}
            {machineLabel && <MachineChip label={machineLabel} />}
            {/* The cwd reads like a shell prompt: `~/…/parent/leaf`, the leaf in
              the foreground at weight 500 (600 on the active tab), the rest
              muted; it gives way from the left, never wrapping. Without a cwd
              yet, the tab name holds the slot so it is never empty. */}
            {promptPath ? (
              <PromptPathLabel
                id={nameId}
                path={promptPath}
                emphasis={isActive ? "semibold" : "medium"}
              />
            ) : (
              <MiddleTruncate
                id={nameId}
                text={tab.name}
                className={cn(
                  "min-w-0 text-[12px] leading-4",
                  isActive ? "font-semibold" : "font-medium",
                )}
              />
            )}
          </button>
        </ContextMenuTrigger>
        <ContextMenuContent
          // The menu is portalled, but React events still bubble to the
          // caption: an item click would focus the cell, a double-click zoom it.
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onCloseAutoFocus={(e) => {
            if (!menuOpenedDiffRef.current) return;
            menuOpenedDiffRef.current = false;
            e.preventDefault();
            // An overlay that takes focus on open keeps it. One that doesn't would leave focus
            // on the unmounted menu item (the body): hand it back to where it was before the
            // menu opened, as a diff opened any other way leaves the terminal focused.
            const back = diffReturnRef.current;
            diffReturnRef.current = null;
            requestAnimationFrame(() => {
              const active = document.activeElement;
              if (back?.isConnected && (!active || active === document.body || !active.isConnected))
                back.focus();
            });
          }}
        >
          <ContextMenuItem
            disabled={!liveCwd}
            onSelect={() => liveCwd && copyText(liveCwd, "path")}
          >
            Copy path
          </ContextMenuItem>
          {git && !git.detached && git.branch && (
            <ContextMenuItem onSelect={() => copyText(git.branch!, "branch name")}>
              Copy branch name
            </ContextMenuItem>
          )}
          {git?.sha && (
            <ContextMenuItem onSelect={() => copyText(git.sha!, "commit SHA")}>
              Copy commit SHA
            </ContextMenuItem>
          )}
          {openDiff && (
            <ContextMenuItem
              onSelect={() => {
                menuOpenedDiffRef.current = true;
                const pre = preMenuFocusRef.current;
                const back =
                  pre instanceof HTMLElement && pre !== document.body && pre.isConnected
                    ? pre
                    : cellTerminalOf(captionRef.current);
                if (back) setSourceDiffReturnFocus(tab.sessionId, back);
                diffReturnRef.current = back;
                openDiff();
              }}
            >
              Open diff
            </ContextMenuItem>
          )}
        </ContextMenuContent>
      </ContextMenu>
      {git && (
        <GitPromptSegment
          status={git}
          // One persistent button while the tab has a diff: the open diff
          // returns focus to it on Esc, so it must not unmount on opening.
          onOpen={canDiff ? toggleDiff : undefined}
          openDisabled={diffOpen}
          testId={`terminal-grid-caption-git-${tab.id}`}
        />
      )}
      {/* SW-TP CAP-7: the per-tab progress note from the last signal_progress
          emit. Flex-1 so it absorbs the row's growing space; truncate on
          overflow. ▸ glyph signals "live async work in progress." Hidden when
          the session hasn't emitted a progress note. */}
      {woken ? (
        <div
          key={woken.deliveryId}
          data-testid={`terminal-grid-caption-woken-${tab.id}`}
          className={cn(
            "flex min-w-0 flex-1 items-center gap-1.5 truncate pl-2 pr-2 text-[12px] opacity-90",
            wokenLeaving ? "cs-woken-out" : "cs-woken-in",
          )}
          title={`Woken by ${wakeName(woken.marker)}`}
        >
          <AlarmClock aria-hidden size={12} className="shrink-0" />
          <span className="truncate">
            Woken by <span className="font-semibold">{wakeName(woken.marker)}</span>
          </span>
        </div>
      ) : (
        progressNote && (
          <div
            data-testid={`terminal-grid-caption-progress-${tab.id}`}
            className={cn(
              "flex min-w-0 flex-1 items-center gap-1.5 truncate pl-2 pr-2 text-[12px]",
              progressSettled ? "italic opacity-60" : "opacity-85",
            )}
            title={progress?.note ?? undefined}
          >
            <span aria-hidden="true" className="shrink-0 text-[10px] opacity-60">
              ▸
            </span>
            <span className="truncate">{progressNote}</span>
          </div>
        )
      )}
      {/* SW-AGR-6: says once per delivery who woke the agent (the caption's
          words are visual; this is what a screen reader hears). */}
      <span className="sr-only" aria-live="polite">
        {/* Keyed by the delivery, so a second wake with the same name speaks again. */}
        {wokenNow && (
          <span key={wokenNow.deliveryId}>
            {`${timerAgent?.name ?? tab.name} woken by ${wakeName(wokenNow.marker)}`}
          </span>
        )}
      </span>
      {/* SW-AGR-6: the timer chip, between the progress note and the icons, so
          the note yields width first. Hidden while the tab has no timers. */}
      {timerRows.length > 0 && (
        <div
          className="flex shrink-0 items-center pl-1"
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") e.stopPropagation();
          }}
        >
          <SessionTimerChip
            tabId={tab.id}
            target={timers.target}
            rows={timerRows}
            agent={timerAgent}
            offline={timers.offline}
            workspaceCwd={ws.cwd}
          />
        </div>
      )}
      {/* SW-TP CAP-6: the per-tab icon cluster. Each icon gates on the tab's
          workspace kind + lazy resolver state; clicks stop propagation so the
          caption's focus-cell handler doesn't swallow them. Keyboard-reachable
          as individual tab stops. */}
      <div
        data-testid={`terminal-grid-caption-icons-${tab.id}`}
        className="flex shrink-0 items-center gap-0.5 pl-1"
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
        // SW-TP CAP-10 polish: Enter / Space on a focused icon toggles the
        // overlay; without stopPropagation the caption's onFocusCapture would
        // also fire `selectCell`, double-dispatching the activation and
        // forcing the cell active for what should be a passive icon click.
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") e.stopPropagation();
        }}
      >
        {caps.hudToggle && (
          <CaptionIconButton
            testId={`terminal-grid-caption-hud-${tab.id}`}
            label={ws.kind === "epic" ? "Epic summary" : "Story summary"}
            shortcut={hudShortcut}
            icon={<PanelTopOpen size={16} strokeWidth={1.75} />}
            active={hudShown}
            onClick={toggleHud}
          />
        )}
        {canEditor && (
          <CaptionIconButton
            testId={`terminal-grid-caption-editor-${tab.id}`}
            label="Code editor"
            shortcut={editorShortcut}
            icon={<Code size={16} strokeWidth={1.75} />}
            active={editorOpen}
            onClick={toggleEditor}
          />
        )}
        {canDiff && (
          <CaptionIconButton
            testId={`terminal-grid-caption-diff-${tab.id}`}
            label="Diff"
            shortcut={diffShortcut}
            icon={<GitCompareArrows size={16} strokeWidth={1.75} />}
            active={diffOpen}
            onClick={toggleDiff}
          />
        )}
        {runningAgent && (
          <CaptionIconButton
            testId={`terminal-grid-caption-inbox-${tab.id}`}
            label={
              inboxUnread > 0
                ? `Inbox — ${inboxCounts.direct} messages · ${inboxCounts.group} in group chat`
                : "Inbox"
            }
            word="Inbox"
            count={inboxUnread}
            shortcut={inboxShortcut}
            icon={<Inbox size={16} strokeWidth={1.75} />}
            active={inboxOpen}
            onClick={toggleInbox}
          />
        )}
      </div>
      <InfoTip label={tabCloseTooltip(verbFor(tab.id))}>
        {/* The close × — 12px glyph in a 20px hit box, 4px from the bar edge
            (`mr-1`) and 2px from the icon cluster. Faint at rest (0.35) so the
            space it holds reads as the close, not as stray margin after the
            last icon; caption hover lifts it to 0.7, hovering the × itself to
            1.0. 80ms ease-out opacity transition. `tabIndex={-1}` + `aria-hidden`
            keep it off the tab order (closable-tab pattern). */}
        <button
          type="button"
          aria-hidden
          tabIndex={-1}
          data-testid={`terminal-grid-caption-close-${tab.id}`}
          onPointerEnter={() => probe(tab.id, [tab.sessionId])}
          onClick={(e) => {
            e.stopPropagation();
            void onClose();
          }}
          onDoubleClick={(e) => e.stopPropagation()}
          style={{ transitionDuration: "80ms", transitionTimingFunction: "ease-out" }}
          className="ml-0.5 mr-1 inline-flex h-5 w-5 shrink-0 items-center justify-center rounded opacity-35 transition-opacity hover:bg-[color-mix(in_srgb,currentColor_15%,transparent)] hover:opacity-100 group-hover:opacity-70"
        >
          <X size={12} />
        </button>
      </InfoTip>
    </div>
  );
}

/** SW-IB1-D1: one toolbar-style toggle in the per-tab caption. 16px lucide
 *  glyph in a 28×28 square button (DESIGN § components.terminal-grid-caption).
 *  Capability-gated: when the capability is OFF the button is NOT rendered —
 *  the caller elides the branch; this component never ships a dim-disabled
 *  button. The pressed state (`active`) paints a color-mixed accent fill at
 *  85% opacity with `var(--foreground)` text, with NO scale transform —
 *  press-is-color-not-motion, per the feel brief. Hover paints a color-mixed
 *  muted fill at 25% and lifts opacity to 1.0 (default is 0.7). Shows its
 *  action label on line 1 and the shortcut on line 2 (11px, 0.6 opacity). */
function CaptionIconButton({
  testId,
  label,
  word,
  count = 0,
  shortcut,
  icon,
  active,
  onClick,
}: {
  testId: string;
  label: string;
  /** A word naming the toggle beside its glyph once the caption has room (it folds
   *  to the glyph alone in a narrow tile). */
  word?: string;
  /** Unread items behind the toggle: a count chip, kept even when the word folds. */
  count?: number;
  shortcut?: string;
  icon: React.ReactNode;
  active: boolean;
  onClick: () => void;
}) {
  // Two-register tooltip layout — label on line 1, keystroke on a muted
  // second line. Keeps the shortcut discoverable without reading as part of
  // the label.
  const tooltipContent = shortcut ? (
    <span className="flex flex-col items-start gap-0.5 text-left">
      <span>{label}</span>
      <span className="text-[11px] opacity-60">{shortcut}</span>
    </span>
  ) : (
    label
  );
  return (
    <InfoTip label={tooltipContent} side="bottom">
      <button
        type="button"
        data-testid={testId}
        aria-label={label}
        aria-pressed={active}
        onClick={onClick}
        // Pressed background uses `color-mix` with 85% accent (not full accent)
        // and foreground text, matching DESIGN § toolbar-icon-pressed-bg.
        // Hover uses `color-mix` with 25% muted — a softer tint than full
        // `--muted`, letting the toolbar bg show through. We pin the exact
        // mix strings on the className via Tailwind arbitrary values so the
        // SSR-serialized style attribute keeps the token names intact (jsdom
        // drops `color-mix(...)` when parsed from a style= attribute, which
        // makes the colors unassertable in unit tests). The tokens
        // (`--accent`, `--muted`, `--foreground`) are declared globally in
        // `index.css` for every shipped theme; no hex fallback is embedded
        // here (the "no blue outside focus-visible" invariant test guards
        // the file against stray hex blues). No `transform: scale(...)`
        // anywhere: press is color, not motion.
        className={cn(
          "inline-flex h-7 min-w-7 items-center justify-center gap-1 rounded transition-colors",
          (word || count > 0) && "px-1",
          word && "@[18rem]/caption:px-1.5",
          "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
          active
            ? "bg-[color-mix(in_srgb,var(--accent)_85%,transparent)] text-[var(--accent-foreground)]"
            : "opacity-70 hover:opacity-100 hover:bg-[color-mix(in_srgb,var(--muted)_25%,transparent)]",
        )}
      >
        {icon}
        {word && (
          <span className="hidden text-[12px] font-medium leading-4 @[18rem]/caption:inline">
            {word}
          </span>
        )}
        {count > 0 && (
          <span
            data-testid={`${testId}-count`}
            aria-hidden="true"
            // The app's neutral attention fill (never alarm-red), on the terminal's chrome.
            className="flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-semibold tabular-nums leading-none text-primary-foreground"
          >
            {count > 99 ? "99+" : count}
          </span>
        )}
      </button>
    </InfoTip>
  );
}

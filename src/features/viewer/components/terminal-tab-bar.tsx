// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useState, useRef, useEffect, useId, useMemo, type KeyboardEvent } from "react";
import { AlarmClock, MailX, Server, TerminalSquare, X } from "lucide-react";
import type { TerminalTab } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { InfoTip } from "@/components/ui/tooltip";
import { tabCloseTooltip, useCloseVerb } from "../lib/terminal-close";
import { SessionStatusMark } from "./session-status-mark";
import { MiddleTruncate, isMiddleTruncated } from "./middle-truncate";
import { AgentTabLabel, TabFace } from "./agent-tab-label";
import { TAB_PILL_MIN_W } from "@/features/viewer/lib/tile-chrome-fit";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { resolveAgentFace, type AgentFace } from "@/features/viewer/lib/agent-list-view";
import { agentRowLabel, agentRowNames } from "@/features/viewer/lib/agent-row-presentation";
import { isAgentSession } from "@/features/viewer/lib/hand-agents";
import { isTypedByHand, TYPED_BY_HAND_TAB } from "@/features/viewer/lib/session-states";
import { useFaceTags, useTabTeamOf } from "@/features/viewer/hooks/use-face-tags";
import { useAgentIdentityChoices } from "@/features/viewer/hooks/use-listed-agents";
import { CHROME_NEW_TAB_W } from "../lib/tile-chrome-fit";
import { TERMINAL_TAB_ID_ATTR } from "../lib/terminal-tab-keys";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay, isMac } from "@/shared/lib/shortcuts/keystroke";
import { useMachine } from "@/features/machines/stores/machines-store";
import { UNKNOWN_MACHINE_LABEL } from "@/features/machines/lib/machine-offline";
import { useShallow } from "zustand/react/shallow";
import { defaultMachineColor, useMachineColor } from "@/features/machines/lib/machine-color";
import { useMachineAppearanceStore } from "@/features/machines/lib/machine-appearance";
import { TAB_TINT } from "@/features/machines/components/machine-canvas";
import {
  useSessionTimersStore,
  visibleTimers,
} from "@/features/viewer/stores/session-timers-store";
import { wakeName } from "@/features/viewer/lib/wake-marker";
import { countdownWords, timerTargetKey } from "@/features/viewer/lib/session-timers";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";

export interface SelectTabOptions {
  focusTerminal?: boolean;
}

interface TerminalTabBarProps {
  tabs: TerminalTab[];
  activeTabId: string | undefined;
  /** The active tab's fill: the surface directly below the bar (the per-tab
   *  caption), so the selected tab reads as joined to what it controls. */
  activeTabBg?: string;
  /** The strip's own background, behind the inactive tabs. */
  chromeBg?: string;
  /** The workspace's team colour (an oklch hue): the ring on the active tab's agent face. */
  teamHue?: number | null;
  /** `focusTerminal`: move keyboard focus into the tab's terminal — set only for
   *  a single pointer click. Keyboard activation (Enter/Space, `detail` 0) keeps
   *  focus on the tab (the tabs pattern), and a double-click's rename keeps its input. */
  onSelectTab: (tabId: string, opts?: SelectTabOptions) => void;
  /** The default close (`×`, middle-click, Delete): Hide when running, End
   *  when idle. May resolve false when nothing closed (End's ask cancelled). */
  onCloseTab: (tabId: string) => void | Promise<boolean>;
  onNewShell: () => void;
  /** The machine chooser: ⌥-click or right-click on "+". */
  onNewShellOn?: () => void;
  /** Its shortcut, for the chevron's tooltip. */
  newShellOnKeystroke?: string;
  onReorderTab: (fromIdx: number, toIdx: number) => void;
  onRenameTab: (tabId: string, name: string) => void;
  onContextMenu?: (tabId: string, x: number, y: number) => void;
  /** The tab whose context menu is open, if any — its pill reads as expanded. */
  menuOpenTabId?: string;
  /** An imperative rename request from the tab context
   *  menu. When this changes to a tab id, that tab enters inline-edit mode (the
   *  same edit UI as double-click). The parent bumps a `{ tabId, nonce }` token so
   *  repeated "Rename" clicks on the same tab re-trigger. */
  renameRequest?: { tabId: string; nonce: number } | null;
  /** An inline rename was committed with Enter or cancelled with Esc — the caller
   *  returns focus to the terminal. (A rename committed by clicking away leaves
   *  focus where the click put it.) */
  onRenameDone?: () => void;
  /** Minimum pill width. When set, pills never shrink below it (status dot +
   *  a few characters stay legible), the strip reserves room for every pill it
   *  is given plus the "+", and the strip no longer clips — so the caller must
   *  hand it only the pills that fit (see `fitTileChrome`). Unset, the strip
   *  scrolls its overflow. */
  pillMinWidth?: number;
}

/** Right padding a tab keeps free for its `×` (the `×` button, 20px, plus the gap
 *  before it), so the label never runs under the button. */
const CLOSE_BUTTON_ROOM = "pr-[34px]";

/**
 * Shared tab rendering for terminal tabs. Used by the per-tile
 * TerminalTileChrome. Handles drag-to-reorder, rename, close, and new tab.
 *
 * SW-EW-34: the `variant="full"` form (the retired title-bar `ConsoleTabs`) is
 * gone — this is the single, compact in-body form.
 */
export function TerminalTabBar({
  tabs,
  activeTabId,
  activeTabBg = "#161b22",
  chromeBg = "var(--cs-terminal-tabbar-bg)",
  teamHue = null,
  onSelectTab,
  onCloseTab,
  onNewShell,
  onNewShellOn,
  newShellOnKeystroke,
  onReorderTab,
  onRenameTab,
  onContextMenu,
  menuOpenTabId,
  renameRequest,
  onRenameDone,
  pillMinWidth,
}: TerminalTabBarProps) {
  const newTabKeystroke = formatKeystrokeDisplay(useEffectiveShortcut("viewer.new-terminal-tab"));
  // Each remote tab wears its Machine's color: read per tab, compared
  // shallowly, so only a change to one of these tabs' colors re-renders the bar
  // (not every image-strength tick, not another machine's color).
  const tabColors = useMachineAppearanceStore(
    useShallow((s) =>
      tabs.map((t) =>
        t.origin
          ? (s.prefs[t.origin.machineId]?.color ?? defaultMachineColor(t.origin.machineId))
          : null,
      ),
    ),
  );
  const dragState = useRef<{ idx: number; startX: number } | null>(null);
  const [dragOverIdx, setDragOverIdx] = useState<number | null>(null);
  const tabRefs = useRef<(HTMLDivElement | null)[]>([]);
  // Rects snapshotted once at drag start (mirrors viewer-tabs.tsx) so pointermove
  // hit-testing reads cached geometry instead of forcing a synchronous layout
  // reflow on every move event during a drag.
  const cachedRects = useRef<(DOMRect | null)[]>([]);
  const [editingTabId, setEditingTabId] = useState<string | null>(null);

  const cancelDrag = () => {
    dragState.current = null;
    cachedRects.current = [];
    setDragOverIdx(null);
  };

  // Mirrors viewer-tabs.tsx: the drag-rect cache + pointer-state ref are cleared
  // on PointerUp / PointerLeave, but unmounting mid-drag (tile close, zoom swap,
  // tab churn) leaves them alive on the ref objects. Null all three ref slots so
  // in-flight closures don't keep the DOMRect array GC-pinned or hold `tabRefs`
  // (detached per-tab DOM nodes) alive.
  useEffect(
    () => () => {
      cachedRects.current = [];
      dragState.current = null;
      tabRefs.current = [];
    },
    [],
  );
  const [editValue, setEditValue] = useState("");
  // The `×` names the verb a click takes (Hide / End), probed lazily on hover.
  const { verbFor, probe: probeCloseVerb } = useCloseVerb();
  const editInputRef = useRef<HTMLInputElement>(null);
  const originalNameRef = useRef("");
  const editingProgressRef = useRef(false);
  // Set once Enter or Esc has settled the edit, so the blur that follows the
  // input unmounting can't commit a second time (or commit a cancelled edit).
  const editSettledRef = useRef(false);

  const startEdit = (tab: TerminalTab) => {
    const names = faces[tabs.findIndex((t) => t.id === tab.id)]?.names;
    const primary = (names?.source === "progress" ? names.secondary : names?.primary) ?? tab.name;
    originalNameRef.current = primary;
    editingProgressRef.current = names?.source === "progress";
    editSettledRef.current = false;
    setEditValue(primary);
    setEditingTabId(tab.id);
  };
  const finishEdit = (tabId: string, commit: boolean, value = editValue) => {
    if (editSettledRef.current) return;
    editSettledRef.current = true;
    const name = value.trim() || originalNameRef.current;
    if (commit && (!editingProgressRef.current || name !== originalNameRef.current)) {
      onRenameTab(tabId, name);
    }
    setEditingTabId(null);
  };

  useEffect(() => {
    if (editingTabId && editInputRef.current) {
      editInputRef.current.focus();
      editInputRef.current.select();
    }
  }, [editingTabId]);

  // The tab context menu's "Rename" enters the same inline-edit
  // mode as a double-click. Keyed by nonce so re-clicking Rename on the same tab
  // re-arms it. The tab is looked up by id so a stale request for a closed tab noops,
  // and a request for the tab already being edited keeps the in-progress text.
  const renameNonce = renameRequest?.nonce;
  const renameTabId = renameRequest?.tabId;
  useEffect(() => {
    if (renameNonce === undefined || !renameTabId) return;
    const tab = tabs.find((t) => t.id === renameTabId);
    if (!tab || editingTabId === tab.id) return;
    startEdit(tab);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- nonce is the trigger; tabs + editingTabId read at fire time
  }, [renameNonce, renameTabId]);

  const activeIdx = tabs.findIndex((t) => t.id === activeTabId);

  // The strip is a tablist: one Tab stop (the focused pill, else the active
  // one); arrows / Home / End move focus between pills, Enter / Space select,
  // Delete (and Backspace on macOS) closes the focused tab with its close verb.
  // Leaving the strip hands the stop back to the active tab, so Tab or
  // Shift+Tab back in lands there.
  const idBase = useId();
  const [focusedTabId, setFocusedTabId] = useState<string | null>(null);
  const stopId = tabs.some((t) => t.id === focusedTabId)
    ? focusedTabId
    : (tabs[activeIdx]?.id ?? tabs[0]?.id);
  const pillRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // A tab closed from the keyboard: focus moves to its neighbour once it is gone.
  const closedFromKeys = useRef<{ closed: string; next: string | undefined } | null>(null);
  useEffect(() => {
    const pending = closedFromKeys.current;
    if (!pending || tabs.some((t) => t.id === pending.closed)) return;
    closedFromKeys.current = null;
    const at = tabs.findIndex((t) => t.id === pending.next);
    pillRefs.current[at === -1 ? activeIdx : at]?.focus();
  }, [tabs, activeIdx]);

  const onPillKeyDown = (e: KeyboardEvent<HTMLButtonElement>, tab: TerminalTab, idx: number) => {
    // Shift+F10 / the context-menu key open the tab menu from the keyboard,
    // anchored under the pill. Only here, on a focused pill: inside the
    // terminal these keys belong to the program running there.
    const isMenuKey =
      e.key === "ContextMenu" ||
      (e.key === "F10" && e.shiftKey && !e.metaKey && !e.ctrlKey && !e.altKey);
    if (isMenuKey && onContextMenu) {
      e.preventDefault();
      e.stopPropagation();
      const rect = e.currentTarget.getBoundingClientRect();
      onContextMenu(tab.id, rect.left, rect.bottom);
      return;
    }
    // Any chord (⌘⌥← Focus Pane, ⌘⌫…) belongs to the global shortcuts.
    if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
    if (e.key === "Delete" || (e.key === "Backspace" && isMac())) {
      e.preventDefault();
      e.stopPropagation();
      const pending = { closed: tab.id, next: (tabs[idx + 1] ?? tabs[idx - 1])?.id };
      closedFromKeys.current = pending;
      void Promise.resolve(onCloseTab(tab.id)).then((closed) => {
        // End's ask was cancelled: the tab stays, and so does focus.
        if (closed === false && closedFromKeys.current === pending) closedFromKeys.current = null;
      });
      return;
    }
    const last = tabs.length - 1;
    const targets: Record<string, number> = {
      ArrowRight: idx === last ? 0 : idx + 1,
      ArrowLeft: idx === 0 ? last : idx - 1,
      Home: 0,
      End: last,
    };
    const to = targets[e.key];
    if (to === undefined) return;
    e.preventDefault();
    e.stopPropagation();
    pillRefs.current[to]?.focus();
  };

  // SW-EW-34: compact (in-body) size tokens — the only remaining form.
  const faces = useTabFaces(tabs);

  const iconSize = 11;
  const activeH = "h-7";
  const inactiveH = "h-6";
  const tabPx = "px-2.5";
  const tabGap = "gap-1.5";
  const newBtnSize = "h-6 w-6";
  const wrapperFlex = "select-none min-w-0 flex items-end";

  return (
    <div
      data-testid="terminal-tab-strip"
      className={`flex items-end gap-0 flex-1 min-w-0 ${pillMinWidth == null ? "overflow-x-auto overflow-y-hidden" : ""}`}
      style={
        pillMinWidth != null
          ? { minWidth: tabs.length * pillMinWidth + CHROME_NEW_TAB_W }
          : undefined
      }
      // SLW-FX-20: the WHOLE tab strip owns its pointer — reorder is tab-location
      // only, never a tile drag. FX-10 guarded the tab buttons (:146); this guard
      // on the strip root closes the two regions it left open — the `+` button
      // and the empty flex background — because their pointerdowns bubble through
      // here before they can reach the SW-EW-17 tile-drag handle spread onto the
      // ancestor chrome wrapper (workspace-terminal-panels.tsx:588). One guard,
      // whole subtree; the identity chrome is a sibling, so tile-drag still
      // initiates there (AC-3). Tile focus is untouched: it runs via the frame's
      // capture-phase focusTile (workspace-terminal-panels.tsx:578, before this
      // bubble handler) and the chrome's onMouseDown (terminal-tile-chrome.tsx:232)
      // — a distinct event from pointerdown. It's stopPropagation, not
      // preventDefault, so the `+` button's onClick still fires.
      onPointerDown={(e) => e.stopPropagation()}
      onPointerMove={(e) => {
        if (!dragState.current) return;
        const dx = Math.abs(e.clientX - dragState.current.startX);
        if (dx < 5) return;
        for (let i = 0; i < cachedRects.current.length; i++) {
          const rect = cachedRects.current[i];
          if (!rect) continue;
          if (e.clientX >= rect.left && e.clientX <= rect.right) {
            setDragOverIdx(i);
            return;
          }
        }
      }}
      onPointerUp={() => {
        if (dragState.current && dragOverIdx !== null && dragState.current.idx !== dragOverIdx) {
          onReorderTab(dragState.current.idx, dragOverIdx);
        }
        cancelDrag();
      }}
      onPointerLeave={() => {
        cancelDrag();
      }}
    >
      <div role="tablist" aria-label="Terminal tabs" className="flex items-end min-w-0">
        {tabs.map((tab, idx) => {
          const isActive = tab.id === activeTabId;
          const tabColor = tabColors[idx] ?? null;
          const tabTint = tabColor
            ? `color-mix(in srgb, ${tabColor} ${Math.round(TAB_TINT * 100)}%, transparent)`
            : null;
          const isStop = tab.id === stopId;
          const nameId = `${idBase}-${idx}-name`;
          const names = faces[idx]?.names;
          const statusId = `${idBase}-${idx}-status`;
          const machineWordsId = `${idBase}-${idx}-machine`;
          const timersId = `${idBase}-${idx}-timers`;
          const mailId = `${idBase}-${idx}-mail`;
          const hideRightDivider = isActive || idx === activeIdx - 1;
          const showDropIndicator =
            dragOverIdx === idx && dragState.current && dragState.current.idx !== idx;

          return (
            <div
              key={tab.id}
              ref={(el) => {
                tabRefs.current[idx] = el;
              }}
              {...{ [TERMINAL_TAB_ID_ATTR]: tab.id }}
              role="presentation"
              className={`group relative ${wrapperFlex} ${showDropIndicator ? "border-l-2 border-accent" : ""} ${!isActive && !hideRightDivider ? "border-r border-[#30363d]" : ""}`}
              style={
                pillMinWidth != null || faces[idx]
                  ? { minWidth: Math.max(pillMinWidth ?? 0, faces[idx] ? TAB_PILL_MIN_W : 0) }
                  : undefined
              }
            >
              <button
                ref={(el) => {
                  pillRefs.current[idx] = el;
                }}
                type="button"
                role="tab"
                data-testid={`terminal-tab-${tab.id}`}
                data-active={isActive ? "true" : "false"}
                aria-selected={isActive}
                tabIndex={isStop ? 0 : -1}
                // Named by the tab's name, then its status in words and how it ended — without the status's
                // ticking age, which the dot's tooltip carries.
                title={names?.full}
                aria-label={names?.full}
                aria-labelledby={`${nameId} ${statusId} ${mailId} ${machineWordsId} ${timersId}`}
                aria-haspopup={onContextMenu ? "menu" : undefined}
                aria-expanded={onContextMenu ? menuOpenTabId === tab.id : undefined}
                onKeyDown={(e) => onPillKeyDown(e, tab, idx)}
                className={`flex grow items-center ${tabGap} ${tabPx} ${CLOSE_BUTTON_ROOM} text-xs transition-colors min-w-0 ${
                  isActive
                    ? `text-foreground font-medium ${activeH} rounded-t-md border-t border-x border-[#30363d] -mb-[1px] relative z-10`
                    : `text-muted-foreground hover:text-foreground hover:bg-foreground/5 hover:rounded-md ${tabTint ? "rounded-md" : ""} ${inactiveH} mb-0.5 mx-0.5`
                }`}
                data-machine-tint={tabTint ? "" : undefined}
                style={{
                  ...(isActive ? { backgroundColor: activeTabBg } : {}),
                  // What a face's status dot is cut out of, and the active face's ring.
                  // (Over a remote tab's wash, the same wash over that ground.)
                  ["--ag-tab-cut" as string]: tabColor
                    ? `color-mix(in srgb, ${tabColor} ${Math.round(TAB_TINT * 100)}%, ${isActive ? activeTabBg : chromeBg})`
                    : isActive
                      ? activeTabBg
                      : chromeBg,
                  ["--ag-tab-ring" as string]: tabRingColor(teamHue),
                  // A remote tab wears its Machine's color: a faint wash over
                  // whatever the strip, the active fill or the hover paints.
                  ...(tabTint
                    ? { backgroundImage: `linear-gradient(${tabTint}, ${tabTint})` }
                    : {}),
                }}
                onClick={(e) => onSelectTab(tab.id, { focusTerminal: e.detail === 1 })}
                // Keyboard / screen-reader users reach the tab by focus, not hover —
                // probe then too, so its `×` names the verb it will take.
                onFocus={() => {
                  setFocusedTabId(tab.id);
                  probeCloseVerb(tab.id, [tab.sessionId]);
                }}
                onBlur={() => setFocusedTabId(null)}
                onPointerDown={(e) => {
                  // SLW-FX-10: a press on a tab is owned by the tab bar's own
                  // reorder, so it must never bubble to the tile's dnd-kit drag
                  // handle (spread onto the enclosing chrome wrapper in split mode).
                  // Mirrors the rename input / close-X, which already stopPropagation.
                  // Runs for every button so middle-click close below stays intact;
                  // pointermove/pointerup are separate events and still reach the
                  // container's reorder handlers.
                  e.stopPropagation();
                  if (e.button === 0) {
                    dragState.current = { idx, startX: e.clientX };
                    // Snapshot rects at drag start — avoids forced layout per move.
                    // Drive length from `tabs` so stale slots in `tabRefs.current`
                    // (never truncated when a tab closes) can't seed phantom entries.
                    cachedRects.current = tabs.map(
                      (_, i) => tabRefs.current[i]?.getBoundingClientRect() ?? null,
                    );
                  }
                  if (e.button === 1) {
                    e.preventDefault();
                    onCloseTab(tab.id);
                  }
                }}
                onContextMenu={(e) => {
                  if (onContextMenu) {
                    e.preventDefault();
                    onContextMenu(tab.id, e.clientX, e.clientY);
                  }
                }}
              >
                {(() => {
                  const tabFace = faces[idx];
                  const face = tabFace?.face;
                  const statusDot = (
                    <SessionStatusMark
                      sessionId={tab.sessionId}
                      exit={tab.exit}
                      size="sm"
                      wordsId={statusId}
                      className={face ? undefined : "absolute -top-0.5 -right-0.5"}
                      data-testid={`tab-${tab.id}-ai-status`}
                    />
                  );
                  if (face && editingTabId !== tab.id) {
                    return (
                      <AgentTabLabel
                        face={face}
                        tag={tabFace?.tag}
                        name={tabFace.names.primary}
                        nameId={nameId}
                        fullName={tabFace.names.full}
                        progressTitle={tabFace.names.source === "progress"}
                        active={isActive}
                        status={statusDot}
                        onDoubleClick={(e) => {
                          e.stopPropagation();
                          startEdit(tab);
                        }}
                      />
                    );
                  }
                  if (face)
                    return (
                      <TabFace
                        face={face}
                        tag={tabFace?.tag}
                        active={isActive}
                        status={statusDot}
                      />
                    );
                  return (
                    <span className="relative flex-shrink-0">
                      <TerminalSquare size={iconSize} className="opacity-70" />
                      {statusDot}
                    </span>
                  );
                })()}
                {faces[idx] && editingTabId !== tab.id ? null : editingTabId === tab.id ? (
                  <input
                    ref={editInputRef}
                    id={nameId}
                    aria-label="Tab name"
                    type="text"
                    className="bg-transparent outline-none text-xs w-full min-w-[40px] text-inherit"
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === "Escape") {
                        e.preventDefault();
                        finishEdit(tab.id, e.key === "Enter");
                        onRenameDone?.();
                      }
                      e.stopPropagation();
                    }}
                    onBlur={(event) => {
                      const field = event.currentTarget;
                      commitTextEditOnBlur(field, () => finishEdit(tab.id, true, field.value));
                    }}
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={(e) => e.stopPropagation()}
                  />
                ) : (
                  <MiddleTruncate
                    id={nameId}
                    text={tab.name}
                    className="cursor-pointer"
                    // Tooltip the full name only when it is actually cut off.
                    onMouseEnter={(e) => {
                      const el = e.currentTarget;
                      if (isMiddleTruncated(el)) el.title = tab.name;
                      else el.removeAttribute("title");
                    }}
                    onDoubleClick={(e) => {
                      e.stopPropagation();
                      startEdit(tab);
                    }}
                  />
                )}
                <TabMailGlyph tab={tab} id={mailId} />
                {tab.origin && (
                  <RemoteTabBadge
                    id={machineWordsId}
                    machineId={tab.origin.machineId}
                    offline={tab.origin.offlineSince !== undefined}
                    testId={`tab-${tab.id}-machine`}
                  />
                )}
                <TabTimersGlyph tab={tab} id={timersId} active={isActive} />
              </button>
              {/* The `×` is the mouse's close — a sibling of the tab, not nested in
                it, over the room the tab reserves at its right edge, shown on hover
                and while the tab has keyboard focus. The keyboard closes a tab with
                Delete on the tab itself, so the tablist holds only tabs. */}
              <InfoTip label={tabCloseTooltip(verbFor(tab.id))}>
                <button
                  type="button"
                  aria-hidden
                  tabIndex={-1}
                  onPointerEnter={() => probeCloseVerb(tab.id, [tab.sessionId])}
                  onFocus={() => probeCloseVerb(tab.id, [tab.sessionId])}
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={() => onCloseTab(tab.id)}
                  data-testid={`tab-${tab.id}-close`}
                  // A 24px square around the 12px glyph (the pointer-target
                  // floor), inside the room the tab reserves at its right edge.
                  className={`absolute right-1 ${isActive ? "bottom-[2px]" : "bottom-0"} z-20 flex h-6 w-6 items-center justify-center rounded cursor-pointer opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100 focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring text-muted-foreground hover:text-foreground transition-opacity`}
                >
                  <X size={12} />
                </button>
              </InfoTip>
            </div>
          );
        })}
      </div>
      <InfoTip
        label={
          onNewShellOn
            ? `New terminal · ${newTabKeystroke} — ⌥-click for another machine${newShellOnKeystroke ? ` (${newShellOnKeystroke})` : ""}`
            : `New terminal · ${newTabKeystroke}`
        }
        side="top"
      >
        <button
          data-testid="terminal-tab-new"
          onClick={(e) => (e.altKey && onNewShellOn ? onNewShellOn() : onNewShell())}
          onContextMenu={
            onNewShellOn
              ? (e) => {
                  e.preventDefault();
                  onNewShellOn();
                }
              : undefined
          }
          aria-label="New terminal"
          className={`text-muted-foreground hover:text-foreground hover:bg-foreground/5 transition-colors ${newBtnSize} mb-0.5 rounded flex items-center justify-center flex-shrink-0 cursor-pointer`}
        >
          <span className="text-base leading-none">+</span>
        </button>
      </InfoTip>
    </div>
  );
}

/** The active agent face's ring: the team colour, soft enough (low chroma) on the always-dark
 *  terminal chrome that it never reads as a status colour; a quiet grey when the workspace has
 *  no team colour. */
function tabRingColor(hue: number | null): string {
  return hue === null ? "#6e7681" : `oklch(78% 0.06 ${hue})`;
}

/** The face of each tab's agent, in tab order, with its number when another running agent of
 *  its team wears the same face; null for a plain shell. */
function useTabFaces(tabs: readonly TerminalTab[]): ({
  face: AgentFace;
  tag: number | undefined;
  names: ReturnType<typeof agentRowNames>;
} | null)[] {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const choices = useAgentIdentityChoices();
  const { faces } = useAvatarPacks();
  const teamKey = useTabTeamOf(tabs[0]?.sessionId);
  const tags = useFaceTags(teamKey);
  return useMemo(() => {
    const bySession = new Map(
      (sessions ?? []).flatMap((r) => (isAgentSession(r) && r.sessionId ? [[r.sessionId, r]] : [])),
    );
    return tabs.map((t) => {
      const row = bySession.get(t.sessionId);
      return row
        ? {
            face: resolveAgentFace(row, {
              name: agentRowLabel(row),
              choice: choices[row.handle],
              faces,
            }),
            tag: tags.get(row.handle),
            names: agentRowNames(row),
          }
        : null;
    });
  }, [tabs, sessions, choices, faces, tags]);
}

/** The machine a remote tab runs on: a small server glyph in the machine's
 *  color beside the tab name — dimmed, with an amber dot, while offline (so
 *  offline is never told by color alone). Hovering names the machine and its
 *  address. */
function RemoteTabBadge({
  id,
  machineId,
  offline,
  testId,
}: {
  id: string;
  machineId: string;
  offline: boolean;
  testId: string;
}) {
  const machine = useMachine(machineId);
  const label = machine?.label ?? UNKNOWN_MACHINE_LABEL;
  const decl = machine?.declaration;
  const address = decl
    ? `${machine?.binding?.username ? `${machine.binding.username}@` : ""}${decl.host}${decl.port === 22 ? "" : `:${decl.port}`}`
    : null;
  const color = useMachineColor(machineId);
  return (
    <InfoTip
      label={
        <span className="flex flex-col gap-0.5">
          <span className="font-medium">{label}</span>
          {address && <span className="text-[#8b949e]">{address}</span>}
          <span className="text-[#8b949e]">{offline ? "Offline" : "Connected"}</span>
        </span>
      }
    >
      <span
        id={id}
        data-testid={testId}
        data-offline={offline ? "" : undefined}
        role="img"
        aria-label={offline ? `on ${label} · offline` : `on ${label}`}
        className="relative inline-flex flex-shrink-0"
        style={{ color }}
      >
        <Server size={12} strokeWidth={2} aria-hidden style={{ opacity: offline ? 0.5 : 1 }} />
        {offline && (
          <span
            aria-hidden
            className="absolute -right-0.5 -bottom-0.5 size-1.5 rounded-full bg-[#d29922]"
          />
        )}
      </span>
    </InfoTip>
  );
}

/** A tab typed by hand whose CLI armed no inbox waiter wears a mail-off glyph after its name:
 *  mail waits there and nothing wakes it. Its name and tooltip are the same words. Foreground
 *  at 88%, since it is the tab's only cue and muted ink is under 3:1 on light tab bars. */
function TabMailGlyph({ tab, id }: { tab: TerminalTab; id: string }) {
  const typed = useAgentListProjectionStore((s) => {
    const row = s.sessions?.find((r) => r.sessionId === tab.sessionId);
    return !!row && isTypedByHand(row);
  });
  if (!typed || tab.exit) return null;
  return (
    <InfoTip label={TYPED_BY_HAND_TAB}>
      <span
        id={id}
        role="img"
        aria-label={TYPED_BY_HAND_TAB}
        data-testid={`tab-${tab.id}-mail-off`}
        className="inline-flex flex-shrink-0 text-foreground/88"
      >
        <MailX size={12} aria-hidden />
      </span>
    </InfoTip>
  );
}

/**
 * SW-AGR-6 (design §4.11): a tab whose session has timers wears a muted alarm
 * glyph — no text, so the pill keeps its width — and says in words, inside the
 * tab's name, how many and when the next is due.
 */
function TabTimersGlyph({ tab, id, active }: { tab: TerminalTab; id: string; active: boolean }) {
  const key = timerTargetKey({ sessionId: tab.sessionId, machineId: tab.origin?.machineId });
  const allRows = useSessionTimersStore((s) => (tab.exit ? undefined : s.bySession[key]?.rows));
  const cancelling = useSessionTimersStore((s) => s.cancelling[key]);
  const rows = useMemo(() => visibleTimers(allRows, cancelling), [allRows, cancelling]);
  // A background tab has no caption on screen: its pill says who woke it.
  const woken = useSessionTimersStore((s) => (tab.exit ? undefined : s.woken[key]));
  const now = useNowClock(30_000, !!rows?.length);
  const next = rows?.length ? Math.min(...rows.map((r) => Date.parse(r.nextAt))) : null;
  const timerWords =
    next !== null && rows
      ? `${rows.length} ${rows.length === 1 ? "timer" : "timers"}, next ${countdownWords(next, now)}`
      : null;
  const wokenWords = woken ? `woken by ${wakeName(woken.marker)}` : null;
  const words = [wokenWords, timerWords].filter(Boolean).join(", ");
  return (
    <>
      {timerWords && (
        <AlarmClock
          aria-hidden
          size={11}
          data-testid={`tab-${tab.id}-timers`}
          className="flex-shrink-0 opacity-70"
        />
      )}
      <span id={id} hidden>
        {words}
      </span>
      {/* Spoken once per delivery into a tab that is not the one shown (the
          shown tab's caption speaks for itself). */}
      {!active && (
        <span className="sr-only" aria-live="polite">
          {woken && (
            <span key={woken.deliveryId}>{`${tab.name} woken by ${wakeName(woken.marker)}`}</span>
          )}
        </span>
      )}
    </>
  );
}

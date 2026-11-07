// SPDX-License-Identifier: MIT
import { useTheme } from "next-themes";
import { DEFAULT_APP_THEME, themeTokens } from "@/themes";
import { contrastRatio } from "@/shared/lib/color-contrast";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { CSSProperties } from "react";
import { SquareArrowOutUpRight } from "lucide-react";
import type { AgentFace } from "../lib/agent-list-view";
import type { StatusMarkKind } from "@/shared/lib/status-mark";
import { useTerminalThemeConfig } from "@/themes/use-terminal-theme";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { PopupHead } from "@/features/viewer/components/agents-popup-head";
import { useRowPopover, type PopoverBounds } from "@/features/viewer/components/agents-popover";
import { TerminalSlot } from "@/shared/terminal/terminal-slot";
import {
  terminalManager,
  type TerminalDisplayReference,
  type TerminalManager,
} from "@/shared/terminal/terminal-manager";
import { TERMINAL_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { useTerminalDragDrop } from "@/shared/hooks/use-terminal-drag-drop";
import { getEffectiveTerminalFontPx } from "@/shared/lib/font-sizes";
import {
  useTerminalFontSize,
  useTerminalFontSizePx,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";

export interface QuickSendProps {
  /** The agent's name, for the header and the terminal's label. */
  name: string;
  face?: AgentFace;
  mark?: StatusMarkKind;
  bounds?: () => PopoverBounds;
  /** The status word the row shows. */
  statusWord: string;
  nativeSubagents?: number;
  /** The workspace the agent works in, when it has one. */
  workspace: string | null;
  /** The question the agent is waiting on, shown as status text (no answer buttons). */
  ask?: string | null;
  /** The agent's live terminal, or null when it has none (it is not running in a tab). */
  sessionId: string | null;
  /** The machine it runs on, when that machine is offline: the reason it has no live terminal. */
  offlineMachine?: string | null;
  /** The row's button, which the popup is anchored to and returns focus to. */
  anchor: HTMLElement;
  /** Close the popup. `restoreFocus` is true for Escape, ⌘W and the close button, false for an
   *  outside click, a scroll away, or the tab opening in the CLI. */
  onClose: (restoreFocus: boolean) => void;
  /** Show the agent in its CLI tab (the tab-visible state's link). */
  onShowInCli: () => void;
  /** Tests pass their own manager. */
  manager?: TerminalManager;
}

/** The popup's size before the user drags its corner: wide and tall enough to read the CLI's
 *  recent output, not only its prompt. `height` is the terminal's. */
const DEFAULT_SIZE = { width: 680, height: 520 };
const MIN_SIZE = { width: 380, height: 160 };
/** Where the dragged size is remembered, on this Mac. */
const SIZE_KEY = "codesense.agents.quickView.size";

/** Typing into the agent's terminal: Escape belongs to the CLI, not the popup. */
const inTerminal = (dialog: HTMLElement) =>
  !!dialog.querySelector(".ag-quick-send-terminal")?.contains(document.activeElement);

/**
 * The quick view: the agent's live CLI, typed into from its row, without switching the CLI
 * tab. When the agent's terminal is not on screen (no shown tile or grid cell holds it, or the
 * CLI column is hidden) it is borrowed here (the same display the tab uses, given back on
 * close); when it is on screen the popup links to it instead. Escape on the popup's own
 * controls or ⌘W anywhere in it closes it. Its bottom-right corner resizes it, and the size is
 * remembered.
 */
export function QuickSendPopover({
  name,
  face,
  mark,
  bounds,
  statusWord,
  nativeSubagents = 0,
  workspace,
  ask,
  sessionId,
  offlineMachine = null,
  anchor,
  onClose,
  onShowInCli,
  manager = terminalManager,
}: QuickSendProps): React.JSX.Element {
  // Dragging the bottom-right corner resizes the popup; the size is remembered for next time.
  const {
    dialogRef,
    bodyRef,
    place,
    shown,
    gripProps,
    gripHint,
    arrowProps,
    onMouseDown,
    closeProps,
  } = useRowPopover({
    anchor,
    onClose,
    storageKey: SIZE_KEY,
    defaultSize: DEFAULT_SIZE,
    minSize: MIN_SIZE,
    bounds,
    keepEscape: inTerminal,
  });
  const [terminalFocused, setTerminalFocused] = useState(false);
  const fontSize = useTerminalFontSize();
  const fontSizePx = useTerminalFontSizePx();
  const themeName = useTerminalThemeConfig();
  const appTheme = useViewerStore((s) => s.settings.appTheme) ?? DEFAULT_APP_THEME;
  const { resolvedTheme } = useTheme();
  const ring = themeTokens(appTheme, resolvedTheme === "dark" ? "dark" : "light")[
    "--theme-focus-ring"
  ];
  // A separately chosen terminal palette can need a different ring from the overlay chrome.
  const terminalRing =
    contrastRatio(ring, themeName.theme.background!) >= 3 ? ring : themeName.theme.foreground;
  const effectiveFontPx = getEffectiveTerminalFontPx(fontSize, fontSizePx);
  const display = useMemo(
    () => ({ fontSizePx: effectiveFontPx, themeConfig: themeName }),
    [effectiveFontPx, themeName],
  );

  // A terminal on screen in the CLI stays there: borrowing it would blank that tile.
  const watchShown = useCallback(
    (listener: () => void) =>
      sessionId === null
        ? () => {}
        : manager.watchShown(sessionId as TerminalDisplayReference, listener),
    [manager, sessionId],
  );
  const onScreen = useSyncExternalStore(watchShown, () =>
    sessionId === null ? false : manager.holdsVisible(sessionId as TerminalDisplayReference),
  );
  const live = sessionId !== null && !onScreen;
  // Set when the terminal leaves the open popup for a tile in the CLI: a screen reader hears it.
  // Not when it leaves because the agent ended or went offline (no tile shows it then).
  const [shownLive, setShownLive] = useState(live);
  const [movedAway, setMovedAway] = useState(false);
  if (shownLive !== live) {
    setShownLive(live);
    setMovedAway(!live && onScreen);
  }
  // A Finder file dropped on the popup's live terminal pastes its path into this agent's terminal,
  // through the same `term.paste()` seam a CLI tile's drop uses (bracketed paste per the live
  // mode, then the session's input route). The popup covers any tile under it, so only it lights.
  const liveRef = useRef(live);
  useEffect(() => {
    liveRef.current = live;
  }, [live]);
  const disposedRef = useRef(false);
  useEffect(() => {
    disposedRef.current = false;
    return () => {
      disposedRef.current = true;
    };
  }, []);
  const pasteIntoTerminal = useCallback(
    (text: string) => {
      if (sessionId === null) return;
      manager.terminal(sessionId as TerminalDisplayReference)?.paste(text);
    },
    [manager, sessionId],
  );
  const isDragOver = useTerminalDragDrop(pasteIntoTerminal, liveRef, disposedRef, bodyRef);
  // Without a live terminal to take focus, the popup's primary action does (else its Close): as
  // it opens, and when its terminal goes on screen in the CLI while focus was in it here. Focus
  // that moved anywhere else (into the CLI tile that took the terminal) stays there. The move
  // waits a frame, so the key that was being typed into the terminal never presses the button.
  const wasLive = useRef<boolean | null>(null);
  useEffect(() => {
    const opening = wasLive.current === null;
    wasLive.current = live;
    if (live) return;
    const dialog = dialogRef.current;
    const focusPrimary = () =>
      (
        dialog?.querySelector<HTMLElement>("[data-primary]") ?? dialog?.querySelector("button")
      )?.focus();
    if (opening) {
      focusPrimary();
      return;
    }
    const active = document.activeElement;
    if (active !== null && active !== document.body && !dialog?.contains(active)) return;
    const frame = requestAnimationFrame(() => {
      const now = document.activeElement;
      if (now === null || now === document.body || dialog?.contains(now)) focusPrimary();
    });
    return () => cancelAnimationFrame(frame);
  }, [live, dialogRef]);
  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-label={`Quick view of ${name}`}
      // A click on its header or notes keeps focus inside it, where ⌘W and Escape still reach it.
      tabIndex={-1}
      data-testid="agent-quick-send"
      {...{ [TERMINAL_POPUP_ATTR]: "" }}
      // ⌘W anywhere in it closes it (Escape in its terminal is the CLI's).
      {...closeProps}
      data-surface="overlay"
      className="ag-quick-send ag-quick-view"
      onMouseDown={onMouseDown}
      style={{
        position: "fixed",
        top: place?.top ?? 0,
        left: place?.left ?? 0,
        width: shown.width,
      }}
    >
      <span {...arrowProps} />
      <PopupHead
        name={name}
        face={face}
        mark={mark}
        subagents={nativeSubagents}
        statusWord={statusWord}
        workspace={workspace}
        ask={ask}
        onClose={() => onClose(true)}
      >
        {sessionId !== null && (
          <button type="button" className="ag-quick-send-btn" onClick={onShowInCli}>
            <SquareArrowOutUpRight className="size-3.5" aria-hidden />
            Open tab
          </button>
        )}
      </PopupHead>
      {live && (
        <div
          ref={bodyRef}
          className={`ag-quick-send-terminal${isDragOver ? " terminal-drag-over" : ""}`}
          style={
            {
              height: shown.height,
              background: themeName.theme.background,
              "--ag-terminal-focus-ring": terminalRing,
            } as CSSProperties
          }
          onFocusCapture={() => setTerminalFocused(true)}
          onBlurCapture={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget)) setTerminalFocused(false);
          }}
        >
          <TerminalSlot
            sessionId={sessionId as TerminalDisplayReference}
            display={display}
            manager={manager}
            active
            borrow
            focusOnReveal
            focusOnMount
          />
        </div>
      )}
      {!live && sessionId !== null && (
        <div className="ag-quick-send-note">
          <span>{name}'s terminal is on screen in the CLI.</span>
          <button type="button" className="ag-quick-send-btn" data-primary onClick={onShowInCli}>
            Show in CLI
          </button>
        </div>
      )}
      {sessionId === null && (
        <div className="ag-quick-send-note">
          <span>
            {offlineMachine
              ? `${offlineMachine} is offline, so ${name}'s terminal can't be shown until it reconnects.`
              : `${name} has no live terminal to message right now.`}
          </span>
        </div>
      )}
      <span role="status" className="sr-only">
        {movedAway ? `${name}'s terminal moved to its tile in the CLI.` : ""}
      </span>
      {live && (
        <div className="ag-quick-send-foot">
          <span className="ag-quick-send-hint">
            {terminalFocused
              ? `Typing to ${name} · Esc goes to ${name} · ⌘W closes`
              : `Click the terminal to type to ${name} · ⌘W closes`}
          </span>
        </div>
      )}
      <div className="ag-quick-send-grip" data-testid="agent-quick-send-grip" {...gripProps}>
        <span id={gripHint.id} hidden>
          {gripHint.text}
        </span>
      </div>
    </div>
  );
}

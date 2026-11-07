// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { Terminal } from "@xterm/xterm";
import { logger } from "@/shared/lib/logger";
import { TerminalBodyContextMenu, useTerminalBodyContextMenu } from "./terminal-body-context-menu";
import { useTerminalDragDrop } from "@/shared/hooks/use-terminal-drag-drop";
import {
  getViewerState,
  type TerminalTab,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { writePty } from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";
import { TerminalGhostHint } from "./terminal-ghost-hint";
import { useTerminalGhostHintStore } from "@/features/viewer/stores/terminal-ghost-hint-store";
import { mayTakeUnrequestedFocus, shouldFocusTerminalOnReveal } from "../lib/focus-cascade";
import { acquireSessionInput, type SessionInput } from "../lib/session-input";
import { routeTerminalInput } from "../lib/terminal-input-gate";
import { endEndedTab, endTabs } from "../lib/terminal-close";
import { useTerminalExit } from "../hooks/use-terminal-exit";
import { paneEndShown, TerminalEndedAnnouncer, TerminalEndedBar } from "./terminal-ended-bar";
import type { RespawnTab } from "@/shared/lib/agent-session-launcher";
import { readXtermVisualState, readGridText } from "@/shared/lib/xterm-visual-state";
import { TerminalFindBar, type TerminalSearch } from "./terminal-find-bar";
import { TERMINAL_SESSION_ID_ATTR } from "../lib/terminal-tab-keys";
import type { TerminalThemeConfig } from "./terminal-constants";
import { usePaneStartup } from "../hooks/use-pane-startup";
import { useEvictedRestore } from "../hooks/use-evicted-restore";
import { PaneStartupOverlay, RetryRemount } from "./pane-startup-overlay";
import {
  installWebglContextEventCounters,
  instrumentClearTextureAtlas,
  readWebglDiag,
} from "@/shared/lib/xterm-webgl-diag";
import { terminalManager, type DisplayHandle } from "@/shared/terminal/terminal-manager";
import { holdForTerminalServer } from "@/features/shell/lib/terminal-server";
import { isFreshOpen } from "@/shared/lib/terminal-open";
import { TerminalSlot } from "@/shared/terminal/terminal-slot";

/** Under "Couldn't restore this terminal.": the session is not lost. */
const RESTORE_FAILED_DETAIL = "The session is still running.";
const INCOMPATIBLE_TITLE = "Terminal server update needed";
const INCOMPATIBLE_DETAIL =
  "This session was started by the previous version. Restart the terminal server to continue.";

/** SLW-TL-7: clear a fresh terminal's ghost placeholder on the first keystroke. Reads the
 *  store imperatively so it never subscribes the panel; a no-op when no hint is registered. */
function clearGhostHint(sessionId: string): void {
  useTerminalGhostHintStore.getState().clear(sessionId);
}

interface ShellTerminalPanelProps {
  sessionId: string;
  /** The tab's process generation: a restart in place bumps it, and the
   *  display is rebuilt for the new process. */
  life?: number;
  /** Set once the tab's process exited: the pane stays, read-only, with its
   *  final output above the ended bar, until the tab is closed or a new process
   *  starts in it. */
  exit?: TerminalTab["exit"];
  /** OC-9c: the workspace whose activation authority this reveal consults. */
  workspaceId?: string;
  /** Webdriver builds only: the key this terminal's harness seams register under.
   *  Defaults to `workspaceId`; a workspace with several tabs passes a per-tab key
   *  (see `harnessPaneKey`) so tab-grid cells don't share one registry slot. */
  harnessPaneKey?: string;
  /** OC-9c: the team whose activation authority this reveal consults. */
  teamKey?: string;
  visible: boolean;
  /** OC-9c: the coordinator generation captured when this attach began. The
   *  reveal-focus is vetoed if a newer activation with a non-`focus-terminal`
   *  intent (or a different target) has landed since — so a superseded restore
   *  never steals focus. Consulted at focus-execution. */
  attachGeneration?: number;
  /** SW-EW-3 R-1: only the focused tile of a split takes keyboard focus, so N
   *  visible tiles don't fight for it. Default true. */
  autoFocus?: boolean;
  fontSizePx: number;
  themeConfig?: TerminalThemeConfig;
  /**
   * SW-GS-1 AC-11: when true, the terminal is input-locked — it renders +
   * scrolls but NO user input reaches `write_pty` (keydown, paste, IME,
   * bracketed-paste, drag-drop). The agent is driving; the user steers via the
   * Workbench. Additive + defaults to unlocked, so existing callers are
   * unaffected.
   */
  inputLocked?: boolean;
  /**
   * SW-LN-29: team context is still landing in this terminal's worktree. While
   * true, a command the user presses Enter on is held and runs once it turns
   * false (or after the hold timeout — see `onContextHoldTimeout`).
   */
  contextLanding?: boolean;
  /** SW-LN-29: a typed command is now held, waiting for context. */
  onContextHold?: () => void;
  /** A held command was dropped (Ctrl-C) and will never run. */
  onContextHoldDropped?: () => void;
  /** SW-LN-29: a held command ran without context because the hold timed out. */
  onContextHoldTimeout?: () => void;
}

/**
 * The reveal-focus gate: while the shell is gone, focus headed for the terminal
 * goes to the ended bar instead (and the terminal is not focused); otherwise the
 * coordinator decides.
 */
export function gateRevealFocus(
  exited: boolean,
  claimEndedFocus: () => void,
  coordinatorAllows: () => boolean,
): boolean {
  if (exited) {
    claimEndedFocus();
    return false;
  }
  return coordinatorAllows();
}

/**
 * OC-9c: the coordinator's verdict on a reveal-focus, with the panel's fail-open
 * guard. When the panel has no team/workspace identity to consult (both are
 * optional props), it never interferes and returns `true` — the historical
 * autofocus. Otherwise it delegates to the suppress-only {@link
 * shouldFocusTerminalOnReveal}.
 */
export function coordinatorAllowsRevealFocus(
  teamKey: string | undefined,
  workspaceId: string | undefined,
  sinceGeneration: number,
): boolean {
  if (!teamKey || !workspaceId) return true;
  return shouldFocusTerminalOnReveal(teamKey, workspaceId, sinceGeneration);
}

/**
 * A terminal pane: the chrome (find bar, startup overlay, ended bar, context
 * hold) around the slot that shows the session's one display. Retry on a pane
 * that failed to start attaches the same display to its session again.
 */
export function ShellTerminalPanel(props: ShellTerminalPanelProps) {
  return (
    <RetryRemount>{(retry) => <ShellTerminalPanelBody {...props} onRetry={retry} />}</RetryRemount>
  );
}

function ShellTerminalPanelBody({
  onRetry,
  sessionId,
  life = 0,
  exit,
  workspaceId,
  harnessPaneKey,
  teamKey,
  visible,
  attachGeneration = 0,
  autoFocus = true,
  fontSizePx,
  themeConfig,
  inputLocked = false,
  contextLanding = false,
  onContextHold,
  onContextHoldDropped,
  onContextHoldTimeout,
}: ShellTerminalPanelProps & { onRetry: () => void }) {
  // Webdriver-only harness seams key off this (see the registry effect below).
  const seamKey = harnessPaneKey ?? workspaceId;
  // The session's display, read live: a reconnect replaces it under the pane.
  const termRef = useMemo<RefObject<Terminal | null>>(
    () => ({
      get current() {
        return terminalManager.terminal(sessionId);
      },
    }),
    [sessionId],
  );
  // The pane is unmounted: nothing it scheduled may act any more.
  const disposedRef = useRef(false);
  useEffect(() => {
    disposedRef.current = false;
    return () => {
      disposedRef.current = true;
    };
  }, []);
  // SW-GS-1 AC-11: read live by every input funnel, so a lock toggle applies
  // without re-creating them.
  const inputLockedRef = useRef(inputLocked);
  inputLockedRef.current = inputLocked;
  // SW-EW-26: the tile's on-screen element, hit-tested against the webview-global
  // drag-drop position so only the terminal under the cursor receives a drop.
  const tileRef = useRef<HTMLDivElement>(null);
  const autoFocusRef = useRef(autoFocus);
  autoFocusRef.current = autoFocus;
  const attachGenerationRef = useRef(attachGeneration);
  attachGenerationRef.current = attachGeneration;
  const teamKeyRef = useRef(teamKey);
  teamKeyRef.current = teamKey;
  // SW-EW-39 / SW-LN-29: the session's input path — keystrokes typed before
  // the attach settled are held in order and flushed once the session provably
  // exists, behind a gate holding a command typed before the worktree's context
  // landed. Owned per session (and life), so a pane that moves hands it on.
  const sessionInputRef = useRef<SessionInput | null>(null);
  const contextLandingRef = useRef(contextLanding);
  contextLandingRef.current = contextLanding;
  const onContextHoldTimeoutRef = useRef(onContextHoldTimeout);
  onContextHoldTimeoutRef.current = onContextHoldTimeout;
  const onContextHoldRef = useRef(onContextHold);
  onContextHoldRef.current = onContextHold;
  const onContextHoldDroppedRef = useRef(onContextHoldDropped);
  onContextHoldDroppedRef.current = onContextHoldDropped;
  useEffect(() => {
    sessionInputRef.current?.setLanding(contextLanding);
  }, [contextLanding]);
  const [started, setStarted] = useState(false);
  // Until the first paint the pane says what it is doing: nothing for a fast
  // open, then the spinner, then "Still starting…", then "Failed · Retry".
  const startup = usePaneStartup(visible);
  const { markReady, markRendered, markFailed } = startup;

  // The process exited: the pane is read-only (no input, no resize).
  const exitedRef = useRef(false);
  const claimEndedFocusRef = useRef<() => void>(() => {});
  const markExitedRef = useRef<() => void>(() => {});
  // Both input funnels (keydown and xterm's onData) route through here: to the
  // shell while it runs; once it is gone, scroll keys scroll and any other key
  // hands focus back to the ended bar.
  const routeInputRef = useRef<(data: string) => void>(() => {});

  // The attach settled. Ask the renderer for a frame: the first paint is its
  // first frame from here on, and an attach with nothing to write would
  // otherwise draw no new frame.
  const readyRef = useRef(false);
  const markPaneReady = useCallback(() => {
    if (readyRef.current) return;
    readyRef.current = true;
    markReady();
    const term = termRef.current;
    if (term && !disposedRef.current) term.refresh(0, term.rows - 1);
  }, [markReady, termRef]);

  // Whether a reveal of this pane may take keyboard focus. Split tiles other
  // than the focused one never take it, focus held by another surface is never
  // taken, and the coordinator vetoes a superseded restore. While the shell is
  // gone, the focus goes to the ended bar.
  const mayFocusOnReveal = useCallback(() => {
    if (!autoFocusRef.current) return false;
    if (workspaceId && !mayTakeUnrequestedFocus(workspaceId)) return false;
    return gateRevealFocus(exitedRef.current, claimEndedFocusRef.current, () =>
      coordinatorAllowsRevealFocus(teamKeyRef.current, workspaceId, attachGenerationRef.current),
    );
  }, [workspaceId]);

  // Input: the session's input path, as the display's input route while this
  // pane is mounted.
  useEffect(() => {
    const input = acquireSessionInput(
      `${sessionId}#${life}`,
      (data) => {
        writePty(sessionId, data).catch(() => {});
      },
      {
        onHold: () => onContextHoldRef.current?.(),
        onTimeout: () => onContextHoldTimeoutRef.current?.(),
        onDiscard: () => onContextHoldDroppedRef.current?.(),
      },
    );
    input.setLanding(contextLandingRef.current);
    sessionInputRef.current = input;
    const unroute = terminalManager.routeInput(sessionId, (data) => routeInputRef.current(data));
    return () => {
      unroute();
      input.release();
      sessionInputRef.current = null;
    };
  }, [sessionId, life]);

  // A pane refused by an incompatible terminal server waits for its restart.
  const releaseHoldRef = useRef<() => void>(() => {});
  useEffect(() => () => releaseHoldRef.current(), []);
  const onEnsured = useCallback(
    (handle: DisplayHandle) => {
      if (disposedRef.current) return;
      const { outcome } = handle;
      if (outcome.kind === "failed") {
        markFailed(RESTORE_FAILED_DETAIL, outcome.reason);
        sessionInputRef.current?.markReady(false);
        return;
      }
      if (outcome.kind === "incompatible") {
        // The terminal server runs another version: offer its restart, and
        // show this pane again once it has restarted.
        markFailed(INCOMPATIBLE_DETAIL, INCOMPATIBLE_TITLE);
        sessionInputRef.current?.markReady(false);
        releaseHoldRef.current();
        releaseHoldRef.current = holdForTerminalServer(onRetry);
        return;
      }
      if (outcome.kind === "ended") {
        // The host no longer holds the session: a read-only "session ended"
        // pane — never a fresh shell.
        markExitedRef.current();
        sessionInputRef.current?.markReady(false);
      } else {
        sessionInputRef.current?.markReady(true);
      }
      markPaneReady();
      setStarted(true);
    },
    [markFailed, markPaneReady, onRetry],
  );
  const restoring = useMemo(() => !isFreshOpen(sessionId), [sessionId]);
  const evictedRestore = useEvictedRestore(sessionId, visible, onEnsured);

  // The first frame counts as painted only once the slot measured the display
  // at its real size, so a new terminal never shows a wrong-width first frame.
  const measuredRef = useRef(false);
  const renderedRef = useRef(false);
  const onMeasured = useCallback(() => {
    measuredRef.current = true;
    if (renderedRef.current) markRendered();
  }, [markRendered]);

  // What the pane hears from its display: first output, first frame, exit.
  const onPtyExitRef = useRef<() => void>(() => {});
  useEffect(() => {
    return terminalManager.listen(sessionId, {
      output: () => markPaneReady(),
      retired: () => {
        sessionInputRef.current?.markReady(false);
        markFailed("Private terminal authorization changed");
      },
      render: () => {
        renderedRef.current = true;
        if (measuredRef.current) markRendered();
      },
      exit: () => onPtyExitRef.current(),
    });
  }, [sessionId, markPaneReady, markRendered, markFailed]);

  // SW-FX-93: right-click Copy/Paste menu on the terminal body.
  const {
    position: terminalMenuPos,
    onContextMenu: handleTerminalContextMenu,
    onClose: closeTerminalMenu,
    target: terminalMenuTarget,
  } = useTerminalBodyContextMenu(termRef, visible);

  // WebDriver-only: an exactly-once printable-input hook, a discrete input
  // (submit) hook, and live visual-state readers, all keyed by `seamKey` and
  // read from the session's display (so they follow it across a rebuild).
  // Synthetic key/`input` events into xterm under WKWebView arrive duplicated;
  // `term.paste` routes bulk data straight through `onData` -> `write_pty` once.
  // `term.input(data)` routes through the same seam WITHOUT bracketed-paste
  // framing, so a submit (`\r`) reads as a real Enter.
  useEffect(() => {
    if (import.meta.env.MODE !== "webdriver" || !seamKey) return;
    const registry = ((
      window as unknown as { __CODESENSE_CLI_TERMS__?: Record<string, (data: string) => void> }
    ).__CODESENSE_CLI_TERMS__ ??= {});
    const paste = (data: string) => termRef.current?.paste(data);
    registry[seamKey] = paste;
    const submitRegistry = ((
      window as unknown as {
        __CODESENSE_CLI_TERM_SUBMIT__?: Record<string, (data: string) => void>;
      }
    ).__CODESENSE_CLI_TERM_SUBMIT__ ??= {});
    // `wasUserInput: false`: a scripted submit — no focus/selection side effects.
    const submit = (data: string) => termRef.current?.input(data, false);
    submitRegistry[seamKey] = submit;
    const stateRegistry = (window.__CODESENSE_TERMINAL_STATE__ ??= {});
    const readState = () => {
      const term = termRef.current;
      return term ? readXtermVisualState(term) : null;
    };
    stateRegistry[seamKey] = readState;
    // SW-CL-12: the buffer-grid truth D1 compares the rendered crop against.
    const gridRegistry = (window.__CODESENSE_TERMINAL_GRID__ ??= {});
    const readGrid = () => {
      const term = termRef.current;
      return term ? readGridText(term) : null;
    };
    gridRegistry[seamKey] = readGrid;
    // Per-pane WebGL renderer diagnostics the harness records with every capture.
    installWebglContextEventCounters();
    const diagRegistry = (window.__CODESENSE_TERMINAL_WEBGL_DIAG__ ??= {});
    const readDiag = () => {
      const term = termRef.current;
      if (!term) return null;
      instrumentClearTextureAtlas(term);
      return readWebglDiag(term, terminalManager.webglLease(sessionId));
    };
    diagRegistry[seamKey] = readDiag;
    // SW-CL-12: drive the scrollback viewport; returns the distance from the
    // bottom after the scroll so a no-op scroll is visible to the caller.
    const scrollRegistry = (window.__CODESENSE_TERMINAL_SCROLL__ ??= {});
    const scrollBy = (lines: number) => {
      const term = termRef.current;
      if (!term) return null;
      term.scrollLines(lines);
      const buffer = term.buffer.active;
      return buffer.baseY - buffer.viewportY;
    };
    scrollRegistry[seamKey] = scrollBy;
    // SW-CL-10: the keypress→echoed-glyph latency, from just before the input
    // to the first render after it (`null` when no render arrives in time).
    const latencyRegistry = ((
      window as unknown as {
        __CODESENSE_CLI_TERM_LATENCY__?: Record<
          string,
          (ch: string, timeoutMs: number) => Promise<number | null>
        >;
      }
    ).__CODESENSE_CLI_TERM_LATENCY__ ??= {});
    const measureLatency = (ch: string, timeoutMs: number): Promise<number | null> => {
      const term = termRef.current;
      if (!term) return Promise.resolve(null);
      const printable = ch.length ? ch[0] : "x";
      return new Promise<number | null>((resolvePromise) => {
        let settled = false;
        const finish = (value: number | null) => {
          if (settled) return;
          settled = true;
          disposable.dispose();
          clearTimeout(timer);
          resolvePromise(value);
        };
        const startedAt = performance.now();
        const disposable = term.onRender(() => finish(performance.now() - startedAt));
        const timer = setTimeout(() => finish(null), timeoutMs);
        term.input(printable, false);
      });
    };
    latencyRegistry[seamKey] = measureLatency;
    return () => {
      // Only retract our own entries — a remount for the same key may already
      // have registered its own.
      if (registry[seamKey] === paste) delete registry[seamKey];
      if (submitRegistry[seamKey] === submit) delete submitRegistry[seamKey];
      if (stateRegistry[seamKey] === readState) delete stateRegistry[seamKey];
      if (gridRegistry[seamKey] === readGrid) delete gridRegistry[seamKey];
      if (diagRegistry[seamKey] === readDiag) delete diagRegistry[seamKey];
      if (scrollRegistry[seamKey] === scrollBy) delete scrollRegistry[seamKey];
      if (latencyRegistry[seamKey] === measureLatency) delete latencyRegistry[seamKey];
    };
  }, [seamKey, sessionId, termRef]);

  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  // Single paste seam shared with the right-click menu: `term.paste()` frames
  // bracketed paste per the live mode and routes through the gated input.
  const pasteIntoTerminal = useCallback(
    (text: string) => {
      termRef.current?.paste(text);
    },
    [termRef],
  );
  const isDragOver = useTerminalDragDrop(
    pasteIntoTerminal,
    visibleRef,
    disposedRef,
    tileRef,
    inputLockedRef,
  );

  // The process ended: the ended bar says how and what next. The pane turns
  // read-only at once and stays until the tab is closed or a new process starts
  // in it — only from the bar (Restart / Relaunch), never on a keystroke.
  const focusTerminal = useCallback(() => terminalManager.focus(sessionId), [sessionId]);
  const respawn = useCallback<RespawnTab>(
    (request) => {
      if (!teamKey || !workspaceId) {
        return Promise.reject(new Error("this terminal has no workspace to restart in"));
      }
      return getViewerState().respawnTerminalTab(teamKey, sessionId, workspaceId, {
        ...request.shell,
        scope: request.scope,
      });
    },
    [teamKey, workspaceId, sessionId],
  );
  const endTab = useCallback(
    ({ dead }: { dead: boolean }) => {
      if (!teamKey || !workspaceId) return;
      if (dead) endEndedTab(teamKey, workspaceId, sessionId);
      else void endTabs(teamKey, workspaceId, { tabId: sessionId });
    },
    [teamKey, workspaceId, sessionId],
  );
  const {
    markExited,
    claimFocus: claimEndedFocus,
    bar: endedBar,
  } = useTerminalExit({
    sessionId,
    exit,
    exitedRef,
    containerRef: tileRef,
    focusTerminal,
    respawn,
    endTab,
  });
  claimEndedFocusRef.current = claimEndedFocus;
  markExitedRef.current = markExited;
  // The ended bar or the startup overlay: one "not running" state at a time.
  const shown = paneEndShown(endedBar, startup.phase);
  routeInputRef.current = (data: string) =>
    routeTerminalInput(
      data,
      { exited: exitedRef.current, locked: inputLockedRef.current },
      {
        write: (d) => sessionInputRef.current?.write(d),
        term: termRef.current,
        toBar: claimEndedFocus,
      },
    );

  onPtyExitRef.current = () => {
    if (disposedRef.current || exitedRef.current) return;
    logger.info("pty", `PTY exit: session_id=${sessionId}`);
    markExited();
  };

  // Find-in-terminal: the search addon and the bar's state belong to the
  // display, so an open bar and its highlights survive this pane unmounting.
  const getSearch = useCallback(
    (): TerminalSearch | null => terminalManager.search(sessionId),
    [sessionId],
  );
  const onFindStateChange = useCallback(
    (state: Parameters<typeof terminalManager.setFindState>[1]) =>
      terminalManager.setFindState(sessionId, state),
    [sessionId],
  );
  // Not on the alternate screen: a full-screen program (vim, htop, an agent's TUI)
  // owns it, and clearing would blank it under the program.
  const clearTerminal = useCallback((): boolean => {
    const term = termRef.current;
    if (!term || term.buffer.active.type === "alternate") return false;
    term.clear();
    return true;
  }, [termRef]);

  const displayOpts = useMemo(
    () => ({ fontSizePx, themeConfig, life }),
    [fontSizePx, themeConfig, life],
  );

  return (
    <div
      ref={tileRef}
      {...{ [TERMINAL_SESSION_ID_ATTR]: sessionId }}
      // SLW-TL-7: the first keystroke clears this tab's ghost placeholder hint (if any).
      // A capture-phase handler runs before xterm's own key handling and never
      // preventDefaults, so it is a clean, additive no-op for a tab with no hint.
      onKeyDownCapture={() => clearGhostHint(sessionId)}
      className={`h-full relative flex flex-col${isDragOver ? " terminal-drag-over" : ""}`}
      // SW-CL-8 (N8): paint the theme bg on the ROOT tile from frame 0 so the
      // first frame is never a default light/white frame.
      style={{ backgroundColor: themeConfig?.theme.background ?? "#0d1117" }}
    >
      {/* SLW-TL-7: a fresh agent terminal's ghosted placeholder — a clean no-op for an
          ordinary tab (no registered hint), cleared on the first keystroke. */}
      <TerminalGhostHint sessionId={sessionId} />
      <TerminalFindBar
        sessionId={sessionId}
        getSearch={getSearch}
        onClear={clearTerminal}
        onClose={focusTerminal}
        initialState={terminalManager.findState(sessionId)}
        onStateChange={onFindStateChange}
        displayGeneration={evictedRestore.generation}
      />
      {evictedRestore.label && (
        <PaneStartupOverlay
          phase="starting"
          failure={null}
          started
          restoring
          background={themeConfig?.theme.background ?? "#0d1117"}
          onRetry={onRetry}
        />
      )}
      {shown.overlay && (
        <PaneStartupOverlay
          phase={startup.phase}
          failure={startup.failure}
          started={started}
          restoring={restoring}
          background={themeConfig?.theme.background ?? "#0d1117"}
          onRetry={onRetry}
        />
      )}
      <TerminalSlot
        sessionId={sessionId}
        display={displayOpts}
        active={visible}
        focusOnReveal={mayFocusOnReveal}
        onEnsured={onEnsured}
        onMeasured={onMeasured}
        onContextMenu={handleTerminalContextMenu}
        {...(import.meta.env.MODE === "webdriver"
          ? { "data-qh1-workspace-id": seamKey, "data-qh1-session-id": sessionId }
          : {})}
        // The ended bar below takes its row from the terminal, never covering it.
        className="flex-1 min-h-0"
        style={{
          // SW-CL-8 (N7): no padding — a native terminal runs content flush to
          // its chrome.
          width: "100%",
          overflow: "hidden",
          backgroundColor: themeConfig?.theme.background ?? "#0d1117",
        }}
      />
      {shown.bar && <TerminalEndedBar {...shown.bar} />}
      <TerminalEndedAnnouncer bar={shown.bar} />
      <TerminalBodyContextMenu
        position={terminalMenuPos}
        onClose={closeTerminalMenu}
        target={terminalMenuTarget}
      />
    </div>
  );
}

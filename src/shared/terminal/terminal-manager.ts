// SPDX-License-Identifier: MIT
/**
 * The one owner of every terminal display: exactly one xterm per session,
 * created once and kept alive — visible or parked — until its tab closes, the
 * process restarts in place, or a reconnect loses its stream. React renders
 * slots (`terminal-slot.tsx`); showing a terminal moves its existing element
 * into a slot, hiding moves it to a parking container. Nothing is rebuilt,
 * replayed, reset or refitted on a show.
 *
 * Every display is built one way: from the host emulator's snapshot of its
 * session, at the size the snapshot was taken, then the stream from there
 * (listen, subscribe, construct at `cols × rows`, write `vt`, apply the held
 * events past the snapshot). A first open, an app relaunch, a reconnect, a
 * restart in place and an eviction all take this path.
 *
 * A reconnect swaps rather than blanks: the current display is frozen (no
 * stream, no input), stays painted under a "Reconnecting…" label, and the
 * rebuilt one replaces it in one frame once its snapshot is parsed.
 *
 * Memory pressure: past {@link EVICT_MAX_DISPLAYS} live displays or
 * {@link EVICT_MAX_BYTES} of estimated buffer, the least recently seen hidden
 * displays are released — lease returned, stream closed, xterm disposed — while
 * their sessions keep running in the host. Showing one again rebuilds it from
 * the snapshot. Shown, focused and still-restoring displays are never evicted.
 *
 * Output and resizes are applied, in stream order, through one apply queue per
 * display: an xterm `write` is asynchronous, so the next item for a session
 * starts only in the previous write's callback, and `appliedSeq` advances in
 * that callback (it always means "parsed"). The display is resized only by the
 * stream's `Resized` events, never by the slot directly, so it changes size at
 * the same point in the output as the host's emulator.
 */
import { Terminal, type IDisposable, type ITerminalOptions } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import {
  activateTerminalParser,
  TERMINAL_PARSER_OPTIONS,
} from "@/shared/terminal/terminal-parser-options";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import "@xterm/xterm/css/xterm.css";

import {
  TERMINAL_THEME,
  type TerminalThemeConfig,
} from "@/features/viewer/components/terminal-constants";
import {
  TERMINAL_STREAMS_LOST_EVENT,
  reserveTerminalStream,
  subscribeTerminal,
  unsubscribeTerminal,
  resizePty,
  writePty,
  type TerminalSnapshot,
  type TerminalStreamEvent,
  type TerminalStreamsLost,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-session-source";
import { attachTerminal } from "@/features/viewer/lib/terminal-attach";
import { acquireInstance } from "@/shared/lib/fe-instance-registry";
import { terminalFrameScheduler } from "@/shared/lib/frame-budget-scheduler";
import { logger } from "@/shared/lib/logger";
import { beginOpenSpan } from "@/shared/lib/open-path-timing";
import {
  createTerminalKeyHandler,
  type TerminalKeyHandler,
} from "@/shared/lib/terminal-key-handler";
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { referenceBuffer, type ReferenceBuffer } from "./buffer-dump";
import { onPrivateAuthorityRetired } from "@/shared/lib/private-terminal-authority";
import { isLiveStreamCurrent, openLiveStream, type LiveItem, type LiveStream } from "./live-stream";
import { createPtyWriteBuffer, type PtyWriteBuffer } from "./pty-write-buffer";
import {
  BUFFER_BYTES_PER_CELL,
  DEFAULT_MAC_OPTION_IS_META,
  EVICT_MAX_BYTES,
  EVICT_MAX_DISPLAYS,
  TERMINAL_FONT_FAMILY,
  TERMINAL_SCROLL_SENSITIVITY,
  TERMINAL_SMOOTH_SCROLL_DURATION_MS,
} from "./terminal-constants";
import {
  createWebglLeasePool,
  type LeaseRenderer,
  type WebglLeaseReading,
} from "./webgl-lease-pool";

import {
  currentTerminalDisplay,
  terminalDisplayKey,
  terminalSessionReference,
  type TerminalDisplayReference,
} from "./terminal-display-reference";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
export type {
  TerminalDisplayReference,
  RemoteTerminalDisplayReference,
} from "./terminal-display-reference";

// ---------------------------------------------------------------------------
// Options and renderer helpers
// ---------------------------------------------------------------------------

export interface BuildTerminalOptionsArgs {
  fontSizePx: number;
  scrollback: number;
  themeConfig?: TerminalThemeConfig;
  macOptionIsMeta?: boolean;
}

/**
 * The `new Terminal(...)` options: a TTY block cursor that hollows on blur, a
 * native scroll feel, bold as weight (not a bright-colour swap, so the tuned
 * palette renders as designed), crisp MesloLGS weights, and ⌥ as Meta.
 */
export function buildTerminalOptions({
  fontSizePx,
  scrollback,
  themeConfig,
  macOptionIsMeta = DEFAULT_MAC_OPTION_IS_META,
}: BuildTerminalOptionsArgs): ITerminalOptions {
  return {
    fontSize: fontSizePx,
    fontFamily: `'${TERMINAL_FONT_FAMILY}', Menlo, monospace`,
    fontWeight: "normal",
    fontWeightBold: "bold",
    theme: themeConfig?.theme ?? TERMINAL_THEME,
    cursorBlink: true,
    cursorStyle: "block",
    cursorInactiveStyle: "outline",
    drawBoldTextInBrightColors: false,
    scrollSensitivity: TERMINAL_SCROLL_SENSITIVITY,
    smoothScrollDuration: TERMINAL_SMOOTH_SCROLL_DURATION_MS,
    macOptionIsMeta,
    scrollback,
    ...TERMINAL_PARSER_OPTIONS,
  };
}

/** Handle over a display's WebGL renderer. `active` is false once disposed or
 *  after a failed load (the display is on the DOM renderer). */
export interface WebglRendererHandle {
  dispose(): void;
  readonly active: boolean;
}

/**
 * Load xterm's WebGL renderer with a DOM fallback. Must run after `term.open()`.
 * No GPU, no context left, or a throw during activation: the partial addon is
 * disposed and the display keeps rendering on the DOM renderer. A context lost
 * mid-session disposes the addon (DOM fallback) and then calls `onLost`; a
 * dispose of our own never does. `dispose` is idempotent and re-entrancy-safe
 * (the addon's own dispose can fire the loss event).
 */
export function loadWebglRenderer(
  createAddon: () => WebglAddon,
  loadAddon: (addon: WebglAddon) => void,
  onLost: () => void = () => {},
): WebglRendererHandle {
  let addon: WebglAddon | null = null;
  const disposeOnce = () => {
    const current = addon;
    if (!current) return;
    addon = null;
    try {
      current.dispose();
    } catch {
      // Already torn down by xterm's addon manager.
    }
  };
  try {
    addon = createAddon();
    // Before activation, so a context lost during activate() still falls back.
    addon.onContextLoss(() => {
      if (!addon) return;
      disposeOnce();
      onLost();
    });
    loadAddon(addon);
  } catch {
    disposeOnce();
  }
  return {
    dispose: disposeOnce,
    get active() {
      return addon !== null;
    },
  };
}

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

export type DisplayCreateReason = "first" | "respawn" | "evicted" | "reconnect";

export interface DisplayOpts {
  fontSizePx: number;
  themeConfig?: TerminalThemeConfig;
  /** The tab's process generation. A different value than the display was
   *  built for means the process restarted in place: the display is rebuilt. */
  life?: number;
}

/** What a pane says above Retry when its terminal could not be restored. */
export const RESTORE_FAILED = "Couldn't restore this terminal.";

/** How long a pane waits for its display to reach the slot's size (the
 *  `Resized` round trip) before it shows the display anyway. */
export const SIZE_SETTLE_BOUND_MS = 1_000;

/** How the session's last attach went. */
export type AttachOutcome =
  | { kind: "attached" }
  /** The host no longer holds the session: the pane ends, read-only. */
  | { kind: "ended" }
  /** No snapshot arrived (no reply in time, or a host error): the pane says
   *  so and offers Retry. The session itself keeps running. */
  | { kind: "failed"; reason: string }
  /** The terminal server runs another version and refused the session: no
   *  display is built; only restarting the server helps. */
  | { kind: "incompatible"; reason: string };

export interface DisplayHandle {
  readonly sessionId: string;
  /** The display's id, or `null` when none was built. */
  readonly instanceId: string | null;
  readonly outcome: AttachOutcome;
}

/** The harness oracle's view of a display. The buffer fields are exactly
 *  `referenceBuffer()`'s, the function the backend emulator's dump runs. */
export interface DisplayDump extends ReferenceBuffer {
  instanceId: string;
  created: number;
  rebuilds: number;
  restoredFromSnapshot: boolean;
  appliedSeq: number;
  appliedResizeGen: number;
}

/** The find bar of a session: kept here so it survives the panel unmounting. */
export interface TerminalFindState {
  query: string;
  caseSensitive: boolean;
  regex: boolean;
  wholeWord: boolean;
  /** The current match (`-1` for none) and the match count, as last reported. */
  resultIndex: number;
  resultCount: number;
}

/** What a mounted pane hears from its session's display — across rebuilds. */
export interface SessionListener {
  output?: (data: string) => void;
  /** The session's private-account authority changed: its display was torn
   *  down (nothing of it may be shown or typed into). A later `ensure` builds
   *  a fresh one. */
  retired?: () => void;
  exit?: () => void;
  render?: () => void;
  /** A reconnect's rebuild settled: the pane shows this outcome as it would
   *  its own `ensure`'s. */
  rebuilt?: (handle: DisplayHandle) => void;
  /** The hidden display was released under memory pressure. */
  evicted?: () => void;
  /** An evicted display's rebuild settled with `handle`, or `null` when it
   *  was superseded or its session retired. */
  restored?: (handle: DisplayHandle | null) => void;
  /** The manager itself put a display into the session's slot: a restore,
   *  a revived eviction or a reconnect's swap. The slot may have changed size
   *  while it had no display to measure. */
  placed?: () => void;
}

/** The live displays and their estimated buffer bytes. */
export interface TerminalMemory {
  displays: number;
  estimatedBytes: number;
}

/** Rechecked when an asynchronously restored display is actually placed. */
type FocusPolicy = boolean | (() => boolean);

function permitsFocus(policy: FocusPolicy): boolean {
  return typeof policy === "function" ? policy() : policy;
}

export interface TerminalManager {
  /** Build the session's display from its host snapshot if it has none (or
   *  its process restarted), and keep it live. Resolves once the snapshot is
   *  written, with how the attach went; `null` if it was superseded or the
   *  session's private authority retired meanwhile. */
  ensure(sessionId: TerminalDisplayReference, opts: DisplayOpts): Promise<DisplayHandle | null>;
  /** Move the display into `slot` (out of any other slot) and repaint it. A
   *  display still being restored moves in once it is built. The slot it leaves,
   *  when that one is shown, lends it: when `slot` detaches it goes back there
   *  (a grid cell or an expanded view over a tile). `borrow`: `slot` only borrows
   *  it (the Agents pane's quick view) from whichever slot held it, shown or
   *  hidden. While it is borrowed, a hidden slot that attaches lends it too; a
   *  shown one takes it, and the borrower gets nothing back. */
  attach(
    sessionId: TerminalDisplayReference,
    slot: HTMLElement,
    opts: { focus: FocusPolicy; visible?: boolean; borrow?: boolean },
  ): void;
  /** Park the display, if `slot` still holds it, unless a slot that lent it is still in the
   *  page: then it goes back to the last of those. */
  detach(sessionId: TerminalDisplayReference, slot: HTMLElement): void;
  /** The slot holding the display, or one that lent it, is shown or hidden (an
   *  ancestor's display). */
  setVisible(sessionId: TerminalDisplayReference, slot: HTMLElement, visible: boolean): void;
  /** Whether the display is on screen in a slot of its own (a tile or a grid cell
   *  that is shown), not counting a borrower: while borrowed, whether a slot that
   *  lent it is shown. */
  holdsVisible(sessionId: TerminalDisplayReference): boolean;
  /** Called whenever `sessionId`'s {@link holdsVisible} changes. */
  watchShown(sessionId: TerminalDisplayReference, listener: () => void): () => void;
  /** Redraw the display in its slot (a shown canvas can come back empty). */
  repaint(sessionId: TerminalDisplayReference, slot: HTMLElement): void;
  /** The one size writer: resize the session's PTY to `cols`×`rows`. The
   *  display follows when the stream's `Resized` for it is applied. A request
   *  for the size it already has does nothing. */
  requestResize(sessionId: TerminalDisplayReference, cols: number, rows: number): void;
  /** The size last asked for, else the display's, or `null`. */
  size(sessionId: TerminalDisplayReference): { cols: number; rows: number } | null;
  /** Resolves once the display is at the size last asked for, or after
   *  {@link SIZE_SETTLE_BOUND_MS}. */
  sized(sessionId: TerminalDisplayReference): Promise<void>;
  /** Whether `slot` is the one slot holding the session's display. */
  holds(sessionId: TerminalDisplayReference, slot: HTMLElement): boolean;
  /** End the display for good: its tab closed. */
  dispose(sessionId: TerminalDisplayReference): void;
  /** Release the session's display if it is hidden and idle; its session
   *  keeps running and the next show rebuilds it. Whether it was released. */
  evict(sessionId: TerminalDisplayReference): boolean;
  memory(): TerminalMemory;
  /** The display's buffer and counters, after every item queued before it. */
  debugDump(sessionId: TerminalDisplayReference): Promise<DisplayDump>;

  /** The session's live xterm, or `null` (none, or disposed). */
  terminal(sessionId: TerminalDisplayReference): Terminal | null;
  /** SW-AGR-6: the live xterm of `sessionId` on `machineId` (`local` for this
   *  Mac), shown or parked, or `null` (none, evicted, or disposed). */
  terminalFor(machineId: string, sessionId: string): Terminal | null;
  /** The display's fit addon: what its slot measures a fit target with. */
  fitAddon(sessionId: TerminalDisplayReference): FitAddon | null;
  /** The last parsed stream offset of the session's display. */
  appliedSeq(sessionId: TerminalDisplayReference): number | null;
  focus(sessionId: TerminalDisplayReference, policy?: FocusPolicy): void;
  /** The display's WebGL lease and the pool's fill, or `null` (no display). */
  webglLease(sessionId: TerminalDisplayReference): WebglLeaseReading | null;
  setAppearance(
    sessionId: TerminalDisplayReference,
    appearance: { fontSizePx: number; themeConfig?: TerminalThemeConfig },
  ): void;
  /** Rebuild the renderer's glyph atlas (after the web font loaded). */
  clearTextureAtlas(sessionId: TerminalDisplayReference): void;
  /** Whether the session's live display already has its glyph atlas built for the loaded face at
   *  `fontSizePx`: a slot mounting for it again (a team coming back) has nothing to redo. */
  atlasBuilt(sessionId: TerminalDisplayReference, fontSizePx: number): boolean;
  /** Record that the display's atlas was built for the loaded face at `fontSizePx`. */
  markAtlasBuilt(sessionId: TerminalDisplayReference, fontSizePx: number): void;
  /** Route the session's input (keys, paste, IME, query answers) through
   *  `route` while it is set; otherwise input goes straight to the PTY. */
  routeInput(sessionId: TerminalDisplayReference, route: (data: string) => void): () => void;
  listen(sessionId: TerminalDisplayReference, listener: SessionListener): () => void;
  findState(sessionId: TerminalDisplayReference): TerminalFindState | null;
  setFindState(sessionId: TerminalDisplayReference, state: TerminalFindState | null): void;
  search(sessionId: TerminalDisplayReference): SearchAddon | null;
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

type QueueItem =
  | { kind: "output"; data: string; seqEnd: number; replay: boolean }
  | { kind: "resize"; cols: number; rows: number; resizeGen: number }
  | { kind: "task"; run: () => void; abort?: () => void };

interface Display {
  isCurrent: () => boolean;
  readonly instanceId: string;
  readonly entry: SessionEntry;
  readonly term: Terminal;
  readonly fit: FitAddon;
  readonly search: SearchAddon;
  /** The element slots adopt; xterm is opened inside it. */
  readonly host: HTMLElement;
  readonly keys: TerminalKeyHandler;
  readonly coalescer: PtyWriteBuffer;
  readonly queue: QueueItem[];
  readonly disposables: IDisposable[];
  readonly releaseInstance: () => void;
  readonly stream: LiveStream;
  busy: boolean;
  appliedSeq: number;
  appliedResizeGen: number;
  /** The seq the coalesced (not yet queued) output ends at. */
  pendingSeqEnd: number;
  /** Writes of recorded output in flight: xterm's answers to the queries they
   *  hold are stale and never reach the PTY. */
  replayDepth: number;
  /** Deregistered by a reconnect: still painted, takes no output and no input. */
  frozen: boolean;
  disposed: boolean;
}

/** Where the reader was in a display a reconnect replaces. */
interface ReadingPosition {
  atBottom: boolean;
  /** The text of the logical line at the top of the viewport. */
  topLine: string;
  /** Rows between that line's first row and the buffer's last. */
  fromEnd: number;
}

/** The display a reconnect is replacing, until its successor swaps in. */
interface Outgoing {
  readonly display: Display;
  readonly position: ReadingPosition;
}

interface Lender {
  slot: HTMLElement;
  visible: boolean;
  focusOnRestore: FocusPolicy;
}

interface SessionEntry {
  reference: TerminalDisplayReference;
  readonly sessionId: string;
  life: number;
  created: number;
  rebuilds: number;
  /** Last size the slot asked for; a rebuild asks the PTY for it again. */
  size: { cols: number; rows: number } | null;
  /** The last size the host refused or never received. The slot measuring the
   *  same size again does not re-ask it (a protected remote session refuses a
   *  non-owner every time); a new size does, and so does the first measurement
   *  after the display is placed or revealed (`retryRefused`), which recovers a
   *  send that failed only transiently. */
  refusedSize: { cols: number; rows: number } | null;
  retryRefused: boolean;
  /** The process exited: its PTY is gone and takes no resize. */
  exited: boolean;
  display: Display | null;
  /** The frozen display a reconnect shows until the rebuilt one swaps in. */
  outgoing: Outgoing | null;
  /** The restore in flight, if any. */
  opening: Promise<DisplayHandle | null> | null;
  /** Bumped by every restore: an older one still in flight gives up. */
  attempt: number;
  outcome: AttachOutcome | null;
  /** Recheck focus permission once the display is built. */
  focusOnRestore: FocusPolicy;
  slot: HTMLElement | null;
  visible: boolean;
  /** The slots the display came from and that still want it back, oldest first, each with
   *  whether it is shown and the focus it was owed: a shown slot another slot took it from, the
   *  slot a borrower took it from, and a hidden slot that attached while it was borrowed. When
   *  the slot holding it lets go, it goes back to the last one still in the page, instead of
   *  parking while that slot thinks it holds it. */
  lenders: Lender[];
  /** `slot` only borrows the display (the quick view over a CLI tab). */
  borrowed: boolean;
  /** {@link TerminalManager.holdsVisible} as its watchers last heard it. */
  heardShown: boolean;
  find: TerminalFindState | null;
  route: ((data: string) => void) | null;
  readonly listeners: Set<SessionListener>;
  /** Waiting for the display to reach `size`. */
  readonly sizeWaiters: Set<() => void>;
  appearance: { fontSizePx: number; themeConfig?: TerminalThemeConfig };
  /** When the display was last attached, shown or focused (a clock tick). */
  lastSeen: number;
  /** Released under memory pressure; the next show rebuilds it. */
  evicted: boolean;
}

/** What a display frozen by a reconnect says over its dimmed content. */
export const RECONNECTING_LABEL = "Reconnecting…";

/** Re-subscribe backoff after a reconnect lost a stream and the fresh
 *  subscribe failed on the transport. */
export const RECOVERY_RETRY_DELAYS_MS = [250, 1000, 4000] as const;

export interface TerminalManagerDeps {
  /** Apply one output item to xterm and call `parsed` once xterm parsed it. */
  applyOutput?: (term: Terminal, data: string, parsed: () => void) => void;
  createWebgl?: () => WebglAddon;
  maxDisplays?: number;
  maxBytes?: number;
  /** Leak faults the collectability test must catch: keep each evicted
   *  display in a module-level array, or leave it booked in the lease pool. */
  leakMutation?: "leak_evicted" | "keep_lease";
}

const leakedDisplays: unknown[] = [];

/** A display's buffer estimate: `cols × lines × 12` of its normal buffer. */
function estimatedBytes(term: Terminal): number {
  return term.cols * term.buffer.normal.length * BUFFER_BYTES_PER_CELL;
}

/** The whole logical line starting at buffer row `start`. */
function logicalLineAt(term: Terminal, start: number): string {
  const buffer = term.buffer.active;
  let text = buffer.getLine(start)?.translateToString(true) ?? "";
  for (let row = start + 1; row < buffer.length && buffer.getLine(row)?.isWrapped; row++) {
    text += buffer.getLine(row)?.translateToString(true) ?? "";
  }
  return text;
}

function readingPosition(term: Terminal): ReadingPosition {
  const buffer = term.buffer.active;
  let start = buffer.viewportY;
  while (start > 0 && buffer.getLine(start)?.isWrapped) start--;
  return {
    atBottom: buffer.viewportY >= buffer.baseY,
    topLine: logicalLineAt(term, start),
    fromEnd: buffer.length - 1 - start,
  };
}

/** An at-bottom reader stays at the bottom; a scrolled-up one keeps the same
 *  logical line at the top (the occurrence nearest where it was) when the
 *  restored buffer still holds it, else goes to the bottom. A jump, never an
 *  animated scroll. */
function restoreReadingPosition(term: Terminal, position: ReadingPosition): void {
  const buffer = term.buffer.active;
  let best = -1;
  if (!position.atBottom && buffer.type === "normal") {
    const hint = buffer.length - 1 - position.fromEnd;
    for (let row = 0; row < buffer.length; row++) {
      if (buffer.getLine(row)?.isWrapped) continue;
      if (logicalLineAt(term, row) !== position.topLine) continue;
      if (best < 0 || Math.abs(row - hint) < Math.abs(best - hint)) best = row;
    }
  }
  const smooth = term.options.smoothScrollDuration;
  term.options.smoothScrollDuration = 0;
  if (best < 0) term.scrollToBottom();
  else term.scrollToLine(best);
  term.options.smoothScrollDuration = smooth;
}

function showReconnecting(host: HTMLElement): void {
  host.style.position = "relative";
  const screen = host.querySelector<HTMLElement>(".xterm");
  if (screen) screen.style.opacity = "0.45";
  const label = document.createElement("div");
  label.setAttribute("role", "status");
  label.dataset.testid = "terminal-reconnecting";
  label.textContent = RECONNECTING_LABEL;
  Object.assign(label.style, {
    position: "absolute",
    inset: "0",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    color: "#c9d1d9",
    fontSize: "13px",
    pointerEvents: "none",
  });
  host.appendChild(label);
}

const PARKING_ID = "cs-terminal-parking";
/** How often streaming output re-checks the memory budget. */
const OUTPUT_BUDGET_CHECK_MS = 1_000;
let instanceCounter = 0;

function openExternal(uri: string): void {
  if (uri.startsWith("https://") || uri.startsWith("http://")) openUrl(uri).catch(() => {});
}

export function createTerminalManager(deps: TerminalManagerDeps = {}): TerminalManager {
  const applyOutput = deps.applyOutput ?? ((term, data, parsed) => term.write(data, parsed));
  const createWebgl = deps.createWebgl ?? (() => new WebglAddon());
  const maxDisplays = deps.maxDisplays ?? EVICT_MAX_DISPLAYS;
  const maxBytes = deps.maxBytes ?? EVICT_MAX_BYTES;
  let seenClock = 0;
  let budgetPending = false;
  let lastOutputBudgetCheck = 0;
  const sessions = new Map<string, SessionEntry>();
  const leases = createWebglLeasePool({
    schedule: (task) => terminalFrameScheduler.schedule(task),
  });
  let globalsInstalled = false;
  const shownListeners = new Map<string, Set<() => void>>();
  /** The font size each live display's atlas was built at. A rebuilt display is a new terminal. */
  const atlasFontPx = new WeakMap<Terminal, number>();
  /** Scroll positions saved while the window is blurred. */
  const saved = new Map<Display, { viewportY: number; pinned: boolean }>();

  function parking(): HTMLElement {
    let el = document.getElementById(PARKING_ID);
    if (!el) {
      el = document.createElement("div");
      el.id = PARKING_ID;
      el.setAttribute("aria-hidden", "true");
      el.style.display = "none";
      document.body.appendChild(el);
    }
    return el;
  }

  function notify(entry: SessionEntry, pick: (l: SessionListener) => void): void {
    for (const listener of [...entry.listeners]) pick(listener);
  }

  // One listener each for exits and lost streams, for every session.
  function installGlobals(): void {
    if (globalsInstalled) return;
    globalsInstalled = true;
    installEvictHook();
    listen<{ session_id: string }>("pty-exit", (event) => {
      const entry = sessions.get(event.payload.session_id);
      if (!entry) return;
      entry.exited = true;
      notify(entry, (l) => l.exit?.());
    }).catch(() => {});
    const purgeRemote = () => {
      for (const entry of [...sessions.values()]) {
        if (typeof entry.reference !== "string" && !currentTerminalDisplay(entry.reference))
          manager.dispose(entry.reference);
      }
    };
    useViewerStore.subscribe(purgeRemote);
    useMachinesStore.subscribe(purgeRemote);
    onPrivateAuthorityRetired(() => {
      purgeRemote();
      for (const entry of [...sessions.values()]) {
        const d = entry.display;
        if (d && !isLiveStreamCurrent(d.stream)) retire(entry, d);
      }
    });
    listen<TerminalStreamsLost>(TERMINAL_STREAMS_LOST_EVENT, (event) => {
      const lost = new Set(event.payload.streams);
      for (const entry of [...sessions.values()]) {
        const stream = entry.display?.stream;
        if (stream && lost.has(stream.streamId)) reconnect(entry);
      }
    }).catch(() => {});
    // A window hidden by the OS can reset the viewport's scroll position; the
    // buffer's own line is the truth, so put it back when the window returns.
    window.addEventListener("blur", () => {
      saved.clear();
      for (const entry of sessions.values()) {
        const d = entry.display;
        d?.keys.clearRepeat();
        if (!d || !entry.slot || !entry.visible) continue;
        const buffer = d.term.buffer.active;
        saved.set(d, { viewportY: buffer.viewportY, pinned: buffer.viewportY >= buffer.baseY });
      }
    });
    window.addEventListener("focus", () => {
      const restore = [...saved];
      saved.clear();
      requestAnimationFrame(() => {
        for (const [d, snap] of restore) {
          if (d.disposed) continue;
          if (snap.pinned) d.term.scrollToBottom();
          else d.term.scrollToLine(snap.viewportY);
        }
      });
    });
  }

  // --- the apply queue ------------------------------------------------------

  function pump(d: Display): void {
    while (!d.busy && !d.disposed && !d.frozen && d.isCurrent() && d.queue.length > 0) {
      const item = d.queue.shift()!;
      if (item.kind === "task") {
        item.run();
        continue;
      }
      if (item.kind === "resize") {
        applyResize(d, item.cols, item.rows, item.resizeGen);
        continue;
      }
      d.busy = true;
      if (item.replay) d.replayDepth++;
      applyOutput(d.term, item.data, () => {
        if (item.replay) d.replayDepth--;
        d.appliedSeq = item.seqEnd;
        d.busy = false;
        pump(d);
      });
    }
  }

  function enqueue(d: Display, item: QueueItem): void {
    // Coalesced output is older than anything queued after it.
    if (item.kind !== "output") d.coalescer.flush();
    d.queue.push(item);
    pump(d);
  }

  /** A `Resized` in its place in the stream: everything before it is parsed at
   *  the old size. A selection is cleared (a reflow can move its text off the
   *  cells it covers); the search addon re-runs an open find on resize. */
  function applyResize(d: Display, cols: number, rows: number, resizeGen: number): void {
    if (d.term.cols !== cols || d.term.rows !== rows) {
      if (d.term.hasSelection()) d.term.clearSelection();
      d.term.resize(cols, rows);
    }
    d.appliedResizeGen = resizeGen;
    settleSize(d.entry);
  }

  function atSize(entry: SessionEntry): boolean {
    const d = entry.display;
    const want = entry.size;
    return !d || !want || entry.exited || (d.term.cols === want.cols && d.term.rows === want.rows);
  }

  function settleSize(entry: SessionEntry): void {
    if (!atSize(entry)) return;
    for (const waiter of [...entry.sizeWaiters]) waiter();
  }

  /** A resize the host refused (or never received) is not the session's size:
   *  forget it, so `size` reads the display's real size again, and remember it
   *  as refused, so measuring the same size again does not re-send it. */
  function forgetRefusedSize(entry: SessionEntry, cols: number, rows: number): void {
    if (entry.size?.cols !== cols || entry.size?.rows !== rows) return;
    entry.size = null;
    entry.refusedSize = { cols, rows };
    entry.retryRefused = false;
    settleSize(entry);
  }

  /** The display was just placed or revealed: its next measurement may ask a
   *  refused size once more. */
  function armRefusedRetry(entry: SessionEntry): void {
    if (entry.refusedSize) entry.retryRefused = true;
  }

  /** The manager itself put a display into the session's slot. */
  function notifyPlaced(entry: SessionEntry): void {
    armRefusedRetry(entry);
    notify(entry, (l) => l.placed?.());
  }

  function onLive(entry: SessionEntry, d: Display, item: LiveItem): void {
    if (
      d.disposed ||
      d.frozen ||
      !currentTerminalDisplay(entry.reference) ||
      !isLiveStreamCurrent(d.stream)
    )
      return;
    if (item.kind === "resized") {
      enqueue(d, { kind: "resize", cols: item.cols, rows: item.rows, resizeGen: item.resizeGen });
      return;
    }
    d.pendingSeqEnd = item.seq;
    d.coalescer.append(item.data);
    notify(entry, (l) => l.output?.(item.data));
    const now = Date.now();
    if (item.data.length > 0) useAgentStateStore.getState().recordOutput(entry.sessionId, now);
    if (now - lastOutputBudgetCheck >= OUTPUT_BUDGET_CHECK_MS) {
      lastOutputBudgetCheck = now;
      scheduleBudget();
    }
  }

  // --- memory pressure ------------------------------------------------------

  function touch(entry: SessionEntry): void {
    entry.lastSeen = ++seenClock;
  }

  function evictable(entry: SessionEntry): boolean {
    const d = entry.display;
    return (
      !!d &&
      !d.disposed &&
      !shown(entry) &&
      !entry.opening &&
      !entry.outgoing &&
      !d.host.contains(document.activeElement)
    );
  }

  function evict(entry: SessionEntry): void {
    const d = entry.display;
    if (!d) return;
    // The host holds everything not yet written; the rebuild starts from it.
    d.coalescer.discard();
    if (deps.leakMutation === "leak_evicted") leakedDisplays.push(d);
    teardown(entry, d);
    if (deps.leakMutation === "keep_lease") leases.add(d.instanceId, leaseRenderer(d));
    entry.evicted = true;
    entry.outcome = null;
    logger.info("pty", `terminal display evicted: session_id=${entry.sessionId}`);
    notify(entry, (l) => l.evicted?.());
  }

  function memory(): TerminalMemory {
    let displays = 0;
    let bytes = 0;
    for (const entry of sessions.values()) {
      // A reconnect's frozen display is still alive until its swap.
      for (const d of [entry.display, entry.outgoing?.display]) {
        if (!d) continue;
        displays++;
        bytes += estimatedBytes(d.term);
      }
    }
    return { displays, estimatedBytes: bytes };
  }

  /** Release the least recently seen hidden displays until both limits hold. */
  function enforceBudget(): void {
    let { displays, estimatedBytes: bytes } = memory();
    if (displays <= maxDisplays && bytes <= maxBytes) return;
    const oldest = [...sessions.values()].filter(evictable).sort((a, b) => a.lastSeen - b.lastSeen);
    for (const entry of oldest) {
      if (displays <= maxDisplays && bytes <= maxBytes) break;
      bytes -= estimatedBytes(entry.display!.term);
      displays--;
      evict(entry);
    }
  }

  /** After the current task, so a slot move (detach A, attach B in one
   *  commit) never sees its display hidden. */
  function scheduleBudget(): void {
    if (budgetPending) return;
    budgetPending = true;
    queueMicrotask(() => {
      budgetPending = false;
      enforceBudget();
    });
  }

  /** Show an evicted display again: rebuild it from the host snapshot. */
  function revive(entry: SessionEntry): Promise<DisplayHandle | null> {
    entry.evicted = false;
    entry.rebuilds++;
    const pending = connect(entry, "evicted");
    void pending.then((handle) => notify(entry, (l) => l.restored?.(handle)));
    return pending;
  }

  // --- WebGL: a lease while shown ------------------------------------------

  /** Swap a display between WebGL and DOM. Never resizes, fits, resets or
   *  writes: the buffer, scroll position, selection and focus stay put. */
  function leaseRenderer(d: Display): LeaseRenderer {
    let handle: WebglRendererHandle | null = null;
    const repaint = () => {
      if (!d.disposed) d.term.refresh(0, d.term.rows - 1);
    };
    return {
      acquire(onLost) {
        if (d.disposed) return false;
        const next = loadWebglRenderer(
          createWebgl,
          (addon) => d.term.loadAddon(addon),
          () => {
            if (handle === next) handle = null;
            repaint();
            onLost();
          },
        );
        if (!next.active) return false;
        handle = next;
        return true;
      },
      release() {
        const current = handle;
        handle = null;
        current?.dispose();
        repaint();
      },
    };
  }

  function shown(entry: SessionEntry): boolean {
    return !!entry.slot && entry.visible;
  }

  /** On screen in a slot of its own: while borrowed, in a shown slot that lent it. */
  function holdsVisible(entry: SessionEntry): boolean {
    if (entry.borrowed) return entry.lenders.some((l) => l.visible && l.slot.isConnected);
    return shown(entry);
  }

  /** Tell the session's watchers when whether it is on screen changed. */
  function notifyShown(entry: SessionEntry): void {
    const now = sessions.get(entry.sessionId) === entry && holdsVisible(entry);
    watchLenders();
    if (now === entry.heardShown) return;
    entry.heardShown = now;
    for (const listener of [...(shownListeners.get(entry.sessionId) ?? [])]) listener();
  }

  // While a display is borrowed, whether it is on screen is whether a shown slot that lent it is
  // still in the page. A lender can leave the page without letting go (its tile's subtree removed
  // around it), so the page is watched while any display is borrowed: a lender that left is
  // dropped, and the session's watchers hear it. The page changes often (every terminal's output
  // redraws rows), so the lenders are checked once a frame at most, and only after a change that
  // removed something.
  let lenderWatch: MutationObserver | null = null;
  let pruneFrame: number | null = null;
  function schedulePrune(records: MutationRecord[]): void {
    if (pruneFrame !== null || !records.some((r) => r.removedNodes.length > 0)) return;
    pruneFrame = requestAnimationFrame(() => {
      pruneFrame = null;
      pruneLenders();
    });
  }
  function pruneLenders(): void {
    for (const entry of sessions.values()) {
      if (!entry.borrowed) continue;
      const before = entry.lenders.length;
      entry.lenders = entry.lenders.filter((l) => l.slot.isConnected);
      if (entry.lenders.length !== before) notifyShown(entry);
    }
  }
  function watchLenders(): void {
    const any = [...sessions.values()].some((e) => e.borrowed && e.lenders.length > 0);
    if (any && !lenderWatch && typeof MutationObserver !== "undefined") {
      lenderWatch = new MutationObserver(schedulePrune);
      lenderWatch.observe(document.body, { childList: true, subtree: true });
    } else if (!any && lenderWatch) {
      lenderWatch.disconnect();
      lenderWatch = null;
      if (pruneFrame !== null) cancelAnimationFrame(pruneFrame);
      pruneFrame = null;
    }
  }

  /** A slot the display leaves for `slot`, kept to give it back to: a borrower's lender, or a
   *  shown slot a non-borrowing slot takes it from. A borrower that loses it gets nothing back. */
  function lendFrom(entry: SessionEntry, slot: HTMLElement, borrow: boolean): void {
    const holder = entry.slot;
    entry.lenders = entry.lenders.filter(
      (l) => l.slot !== slot && l.slot !== holder && l.slot.isConnected,
    );
    if (!holder || holder === slot || !holder.isConnected || entry.borrowed) return;
    if (!borrow && !entry.visible) return;
    entry.lenders.push({
      slot: holder,
      visible: entry.visible,
      focusOnRestore: entry.focusOnRestore,
    });
  }

  // --- construction and teardown -------------------------------------------

  function deliverInput(entry: SessionEntry, data: string): void {
    if (!currentTerminalDisplay(entry.reference)) return;
    if (entry.route && typeof entry.reference === "string") entry.route(data);
    else writePty(terminalSessionReference(entry.reference), data).catch(() => {});
  }

  /** The one place a display's xterm is constructed: at the snapshot's size
   *  and history length, the session's spawn-time values. */
  function createDisplay(
    entry: SessionEntry,
    reason: DisplayCreateReason,
    snapshot: TerminalSnapshot,
    stream: LiveStream,
  ): Display {
    const doneTiming = beginOpenSpan("xterm construction");
    const term = new Terminal({
      ...buildTerminalOptions({ ...entry.appearance, scrollback: snapshot.scrollback }),
      cols: snapshot.cols,
      rows: snapshot.rows,
    });
    const fit = new FitAddon();
    const search = new SearchAddon();
    term.loadAddon(fit);
    // Same parser setup as the backend emulator, so both parse the stream into the same cells.
    activateTerminalParser(term);
    term.loadAddon(search);
    term.options.linkHandler = { activate: (_event, uri) => openExternal(uri) };

    const host = document.createElement("div");
    host.className = "cs-terminal-display";
    host.style.width = "100%";
    host.style.height = "100%";
    parking().appendChild(host);
    term.open(host);

    // A frozen display's key shortcuts (Shift+Enter, line jumps, repeats) write
    // around `onData`: they are dropped with it.
    const keys = createTerminalKeyHandler(
      (data) => {
        if (!display.frozen) deliverInput(entry, data);
      },
      {
        isApplicationCursorMode: () => term.modes.applicationCursorKeysMode,
        isNormalScreen: () => term.buffer.active.type === "normal",
        scrollToTop: () => term.scrollToTop(),
        scrollToBottom: () => term.scrollToBottom(),
        scrollLines: (n) => term.scrollLines(n),
        scrollPages: (n) => term.scrollPages(n),
      },
    );
    term.attachCustomKeyEventHandler(keys.handler);
    host.addEventListener("compositionstart", () => keys.clearRepeat());
    host.addEventListener("focusin", () => leases.focus(display.instanceId));

    const display: Display = {
      isCurrent: () => currentTerminalDisplay(entry.reference),
      instanceId: `${entry.sessionId}#${++instanceCounter}`,
      entry,
      term,
      fit,
      search,
      host,
      keys,
      coalescer: createPtyWriteBuffer((data) => {
        if (!currentTerminalDisplay(entry.reference) || !isLiveStreamCurrent(display.stream))
          return;
        enqueue(display, { kind: "output", data, seqEnd: display.pendingSeqEnd, replay: false });
      }),
      queue: [],
      disposables: [],
      releaseInstance: acquireInstance("webglTerminal"),
      stream,
      busy: false,
      appliedSeq: 0,
      appliedResizeGen: snapshot.resize_gen,
      pendingSeqEnd: 0,
      replayDepth: 0,
      frozen: false,
      disposed: false,
    };
    display.disposables.push(
      term.onData((data) => {
        if (display.replayDepth > 0 || display.frozen) return;
        deliverInput(entry, data);
      }),
      term.onRender(() => notify(entry, (l) => l.render?.())),
    );
    // URL detection has nothing to linkify before output arrives.
    terminalFrameScheduler.schedule(() => {
      if (!display.disposed) term.loadAddon(new WebLinksAddon((_e, uri) => openExternal(uri)));
    });
    leases.add(display.instanceId, leaseRenderer(display));
    entry.display = display;
    entry.created++;
    logger.info("pty", `terminal display created: session_id=${entry.sessionId} reason=${reason}`);
    doneTiming?.();
    registerHarnessEntry(entry);
    return display;
  }

  function teardown(entry: SessionEntry, d: Display): void {
    if (d.disposed) return;
    d.disposed = true;
    for (const item of d.queue.splice(0)) if (item.kind === "task") item.abort?.();
    d.stream.close();
    leases.remove(d.instanceId);
    d.keys.clearRepeat();
    for (const disposable of d.disposables) disposable.dispose();
    d.releaseInstance();
    saved.delete(d);
    d.host.remove();
    d.term.dispose();
    if (entry.display === d) entry.display = null;
  }

  /** A private-account authority change: drop everything of this display —
   *  pending output unwritten, the xterm and its parser queue disposed — and
   *  tell the pane. Never reset and reused. */
  function retire(entry: SessionEntry, d: Display): void {
    if (d.disposed) return;
    d.coalescer.discard();
    dropOutgoing(entry);
    teardown(entry, d);
    entry.outcome = null;
    notify(entry, (l) => l.retired?.());
  }

  function handleOf(entry: SessionEntry): DisplayHandle {
    return {
      sessionId: entry.sessionId,
      instanceId: entry.display?.instanceId ?? null,
      outcome: entry.outcome ?? { kind: "failed", reason: RESTORE_FAILED },
    };
  }

  /** Build the display from `snapshot` and go live: construct at the recorded
   *  size, write `vt`, then the held stream events past it, in arrival order.
   *  Resolves once `vt` is parsed. A reconnect's display is built parked: the
   *  swap moves it into the slot. */
  function restore(
    entry: SessionEntry,
    reason: DisplayCreateReason,
    stream: LiveStream,
    snapshot: TerminalSnapshot,
  ): Promise<void> {
    const parked = reason === "reconnect" && !!entry.outgoing;
    const d = createDisplay(entry, reason, snapshot, stream);
    const written = new Promise<void>((resolve) => {
      enqueue(d, { kind: "output", data: snapshot.vt, seqEnd: snapshot.seq, replay: true });
      enqueue(d, {
        kind: "task",
        run: () => {
          d.term.scrollToBottom();
          resolve();
        },
        abort: resolve,
      });
    });
    stream.begin((item) => onLive(entry, d, item));
    // The PTY is at the snapshot's size; the slot may already want another.
    const want = entry.size;
    if (want && !entry.exited && (want.cols !== snapshot.cols || want.rows !== snapshot.rows)) {
      resizePty(terminalSessionReference(entry.reference), want.cols, want.rows).catch(() =>
        forgetRefusedSize(entry, want.cols, want.rows),
      );
    }
    if (parked) return written;
    const slot = entry.slot;
    if (slot) {
      leases.attached(d.instanceId);
      placeInSlot(entry, d, slot, entry.focusOnRestore);
    }
    entry.focusOnRestore = false;
    scheduleBudget();
    if (slot) notifyPlaced(entry);
    return written;
  }

  /** Subscribe and restore. A superseded attempt (a newer restore, a
   *  dispose) gives up and closes what it opened. */
  function connect(
    entry: SessionEntry,
    reason: DisplayCreateReason,
    retryDelays: readonly number[] = [],
  ): Promise<DisplayHandle | null> {
    const attempt = ++entry.attempt;
    const current = () =>
      sessions.get(entry.sessionId) === entry &&
      entry.attempt === attempt &&
      currentTerminalDisplay(entry.reference);
    const remoteEpoch = () =>
      typeof entry.reference === "string" ? undefined : entry.reference.privateAuthorityEpoch;
    const run = async (n: number): Promise<DisplayHandle | null> => {
      const result = await attachTerminal(entry.sessionId, {
        open: () =>
          openLiveStream({
            reserve: reserveTerminalStream,
            listen: (channel, onEvent) =>
              listen<TerminalStreamEvent>(channel, (event) => {
                if (!currentTerminalDisplay(entry.reference)) return;
                const epoch = remoteEpoch();
                if (epoch !== undefined && event.payload.privateAuthorityEpoch !== epoch) return;
                onEvent(event.payload);
              }),
            subscribe: async (streamId) => {
              const reply = await subscribeTerminal(
                terminalSessionReference(entry.reference),
                streamId,
              );
              const epoch = remoteEpoch();
              if (
                !currentTerminalDisplay(entry.reference) ||
                (epoch !== undefined && reply.privateAuthorityEpoch !== epoch)
              )
                throw new Error("Terminal authority changed while attaching");
              return reply;
            },
            unsubscribe: (streamId) => {
              unsubscribeTerminal(streamId).catch(() => {});
            },
          }),
        isAborted: () => !current(),
      });
      if (!current()) return null;
      if (!result) {
        // The stream's private authority retired mid-attach.
        entry.outcome = null;
        notify(entry, (l) => l.retired?.());
        return null;
      }
      if (!result.attached) {
        if (result.ended) {
          entry.outcome = { kind: "ended" };
        } else if (result.incompatible) {
          entry.outcome = { kind: "incompatible", reason: result.error };
        } else {
          const delay = retryDelays[n];
          if (delay !== undefined) {
            await new Promise((resolve) => setTimeout(resolve, delay));
            return current() ? run(n + 1) : null;
          }
          logger.error(
            "pty",
            `terminal restore failed: session_id=${entry.sessionId} — ${result.error}`,
          );
          entry.outcome = { kind: "failed", reason: RESTORE_FAILED };
        }
        return handleOf(entry);
      }
      entry.outcome = { kind: "attached" };
      await restore(entry, reason, result.stream, result.snapshot);
      return current() ? handleOf(entry) : null;
    };
    const opening = run(0).finally(() => {
      if (entry.opening === opening) entry.opening = null;
    });
    entry.opening = opening;
    return opening;
  }

  /** Dispose the display and restore it again: a restart in place. */
  function rebuild(
    entry: SessionEntry,
    reason: DisplayCreateReason,
  ): Promise<DisplayHandle | null> {
    const old = entry.display ?? entry.outgoing?.display;
    if (old && typeof entry.focusOnRestore !== "function") {
      entry.focusOnRestore = old.host.contains(document.activeElement);
    }
    dropOutgoing(entry);
    if (entry.display) teardown(entry, entry.display);
    entry.rebuilds++;
    entry.outcome = null;
    return connect(entry, reason);
  }

  /** Deregister `d` (no stream, no queued items, no input) but leave it
   *  painted, dimmed under the reconnecting label. */
  function freeze(entry: SessionEntry, d: Display): void {
    entry.outgoing = { display: d, position: readingPosition(d.term) };
    if (typeof entry.focusOnRestore !== "function") {
      entry.focusOnRestore = d.host.contains(document.activeElement);
    }
    d.frozen = true;
    d.coalescer.discard();
    for (const item of d.queue.splice(0)) if (item.kind === "task") item.abort?.();
    d.stream.close();
    d.keys.clearRepeat();
    d.term.options.disableStdin = true;
    entry.display = null;
    showReconnecting(d.host);
  }

  function dropOutgoing(entry: SessionEntry): void {
    const frozen = entry.outgoing?.display;
    entry.outgoing = null;
    if (frozen) teardown(entry, frozen);
  }

  /** A lost stream: freeze the display, rebuild a parked one from the
   *  snapshot, and swap it in one frame once its snapshot is parsed. A newer
   *  loss mid-swap drops the parked one and keeps the frozen display up. */
  function reconnect(entry: SessionEntry): void {
    const current = entry.display;
    if (current) {
      if (entry.outgoing) teardown(entry, current);
      else freeze(entry, current);
    }
    entry.rebuilds++;
    entry.outcome = null;
    const opening = connect(entry, "reconnect", RECOVERY_RETRY_DELAYS_MS);
    void opening.then((handle) => {
      // A newer restore owns the frozen display now.
      if (entry.opening) return;
      const incoming = entry.display;
      if (handle?.outcome.kind === "attached" && incoming) {
        requestAnimationFrame(() => swapIn(entry, incoming));
        return;
      }
      // Gave up: no dimmed display without a label — the pane shows why.
      dropOutgoing(entry);
      if (handle) notify(entry, (l) => l.rebuilt?.(handle));
    });
  }

  /** Replace the frozen display with `incoming` in one DOM step, keep the
   *  reading position and focus, then dispose the frozen one. */
  function swapIn(entry: SessionEntry, incoming: Display): void {
    if (incoming.disposed || entry.display !== incoming) return;
    const outgoing = entry.outgoing;
    entry.outgoing = null;
    const slot = entry.slot;
    if (slot) {
      leases.attached(incoming.instanceId);
      if (outgoing && outgoing.display.host.parentElement === slot) {
        outgoing.display.host.replaceWith(incoming.host);
      }
      placeInSlot(entry, incoming, slot, entry.focusOnRestore);
    }
    entry.focusOnRestore = false;
    if (outgoing) {
      restoreReadingPosition(incoming.term, outgoing.position);
      teardown(entry, outgoing.display);
    }
    if (slot) notifyPlaced(entry);
    notify(entry, (l) => l.rebuilt?.(handleOf(entry)));
  }

  /** Move the session's display into `slot`: "placed" when the built display is in it now,
   *  "waiting" when it moves in once built, null when the session shows nothing any more. */
  function moveIn(
    entry: SessionEntry,
    slot: HTMLElement,
    focus: FocusPolicy,
    visible: boolean,
  ): "placed" | "waiting" | null {
    if (!currentTerminalDisplay(entry.reference)) return null;
    entry.slot = slot;
    entry.visible = visible;
    touch(entry);
    const d = entry.display;
    const frozen = entry.outgoing?.display;
    if (frozen && frozen.host.parentElement !== slot) slot.appendChild(frozen.host);
    if (!d || entry.outgoing) {
      entry.focusOnRestore = focus;
      if (entry.evicted && shown(entry)) void revive(entry);
      return "waiting";
    }
    leases.attached(d.instanceId);
    placeInSlot(entry, d, slot, focus);
    return "placed";
  }

  function applyAppearance(entry: SessionEntry): void {
    const term = entry.display?.term;
    if (!term || !shown(entry)) return;
    const appearance = entry.appearance;
    if (term.options.fontSize !== appearance.fontSizePx)
      term.options.fontSize = appearance.fontSizePx;
    const theme = appearance.themeConfig?.theme;
    if (theme && term.options.theme !== theme) term.options.theme = theme;
  }

  function placeInSlot(
    entry: SessionEntry,
    d: Display,
    slot: HTMLElement,
    focus: FocusPolicy,
  ): void {
    if (d.host.parentElement !== slot) slot.appendChild(d.host);
    applyAppearance(entry);
    d.term.refresh(0, d.term.rows - 1);
    leases.setVisible(d.instanceId, shown(entry));
    if (permitsFocus(focus)) {
      leases.focus(d.instanceId);
      d.term.focus();
    }
  }
  // --- webdriver-only harness door ------------------------------------------

  function registerHarnessEntry(entry: SessionEntry): void {
    if (import.meta.env.MODE !== "webdriver") return;
    const registry = (window.__csTerminals ??= {});
    registry[entry.sessionId] = {
      get instanceId() {
        return entry.display?.instanceId ?? null;
      },
      get created() {
        return entry.created;
      },
      get rebuilds() {
        return entry.rebuilds;
      },
      get appliedSeq() {
        return entry.display?.appliedSeq ?? null;
      },
      debugDump: () => manager.debugDump(entry.reference),
      loseContext: () => {
        // The WebGL renderer's own context; the DOM renderer has none. Never
        // `canvas.getContext`, which would create one on a canvas without it.
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const core = (entry.display?.term as any)?._core;
        const gl: WebGL2RenderingContext | undefined = core?._renderService?._renderer?.value?._gl;
        const ext = gl?.getExtension("WEBGL_lose_context");
        if (!ext) return false;
        ext.loseContext();
        return true;
      },
    };
  }

  function installEvictHook(): void {
    if (import.meta.env.MODE !== "webdriver") return;
    window.__CODESENSE_TERMINAL_EVICT_E2E__ = {
      evict: (sessionId) => {
        const entry = sessions.get(sessionId);
        return !!entry && manager.evict(entry.reference);
      },
      memory: () => manager.memory(),
    };
  }

  function unregisterHarnessEntry(sessionId: string): void {
    if (import.meta.env.MODE !== "webdriver") return;
    if (window.__csTerminals) delete window.__csTerminals[sessionId];
  }

  // --- the manager ----------------------------------------------------------

  const manager: TerminalManager = {
    ensure(reference, opts) {
      const sessionId = terminalDisplayKey(reference);
      if (!currentTerminalDisplay(reference)) return Promise.resolve(null);
      installGlobals();
      const life = opts.life ?? 0;
      let entry = sessions.get(sessionId);
      if (!entry) {
        entry = {
          sessionId,
          reference,
          life,
          created: 0,
          rebuilds: 0,
          size: null,
          refusedSize: null,
          retryRefused: false,
          exited: false,
          display: null,
          outgoing: null,
          opening: null,
          attempt: 0,
          outcome: null,
          focusOnRestore: false,
          slot: null,
          visible: false,
          lenders: [],
          borrowed: false,
          heardShown: false,
          find: null,
          route: null,
          listeners: new Set(),
          sizeWaiters: new Set(),
          appearance: { fontSizePx: opts.fontSizePx, themeConfig: opts.themeConfig },
          lastSeen: 0,
          evicted: false,
        };
        sessions.set(sessionId, entry);
      }
      entry.reference = reference;
      if (entry.life !== life) {
        entry.life = life;
        entry.exited = false;
        entry.refusedSize = null;
        entry.retryRefused = false;
        return rebuild(entry, "respawn");
      }
      if (entry.opening) return entry.opening;
      if (entry.evicted) return revive(entry);
      if (entry.display || entry.outcome?.kind === "ended") {
        return Promise.resolve(handleOf(entry));
      }
      // Never built, or the last attempt failed: restore.
      return connect(entry, entry.created === 0 ? "first" : "reconnect");
    },

    attach(reference, slot, opts) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry || !currentTerminalDisplay(entry.reference)) return;
      const visible = opts.visible ?? true;
      if (opts.borrow) {
        if (!(entry.borrowed && entry.slot === slot)) lendFrom(entry, slot, true);
        entry.borrowed = true;
      } else if (entry.borrowed && entry.slot !== slot) {
        // A hidden slot (a tile remounting while the quick view has the display) lends it too;
        // one on screen takes it, and the borrower then has nothing to give back.
        if (!visible) {
          entry.lenders = entry.lenders.filter((l) => l.slot !== slot);
          entry.lenders.push({ slot, visible, focusOnRestore: opts.focus });
          notifyShown(entry);
          return;
        }
        entry.lenders = entry.lenders.filter((l) => l.slot !== slot);
        entry.borrowed = false;
      } else if (entry.slot !== slot) {
        lendFrom(entry, slot, false);
      }
      moveIn(entry, slot, opts.focus, visible);
      notifyShown(entry);
    },

    detach(reference, slot) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return;
      if (entry.slot !== slot) {
        // A slot that no longer holds it lets go: it wants nothing back. Nothing is on screen
        // differently unless it was a lender.
        // Lenders that left the page go too: they can never be given it back.
        const before = entry.lenders.length;
        entry.lenders = entry.lenders.filter((l) => l.slot !== slot && l.slot.isConnected);
        if (entry.lenders.length !== before) notifyShown(entry);
        return;
      }
      entry.borrowed = false;
      // Keys held in the slot letting go repeat no further, wherever the display goes.
      entry.display?.keys.clearRepeat();
      // The display goes back to the last slot that lent it and is still in the page, with the
      // focus it was owed; when there is none, or the session shows nothing any more, it parks.
      // The focus only while nothing else took it: a close that gave focus back to a row (the
      // quick view's Escape) keeps it there.
      entry.lenders = entry.lenders.filter((l) => l.slot.isConnected);
      const lender = entry.lenders.pop();
      const active = document.activeElement;
      const focusFree = !active || active === document.body || slot.contains(active);
      const back = lender
        ? moveIn(
            entry,
            lender.slot,
            () => focusFree && permitsFocus(lender.focusOnRestore),
            lender.visible,
          )
        : null;
      if (lender && back) {
        if (back === "placed") notifyPlaced(entry);
        notifyShown(entry);
        // Given back hidden, it counts against the hidden budget as a parked one does.
        if (!lender.visible) scheduleBudget();
        return;
      }
      entry.lenders = [];
      entry.slot = null;
      entry.focusOnRestore = false;
      const frozen = entry.outgoing?.display;
      if (frozen) parking().appendChild(frozen.host);
      const d = entry.display;
      notifyShown(entry);
      if (!d) return;
      parking().appendChild(d.host);
      d.keys.clearRepeat();
      leases.setVisible(d.instanceId, false);
      scheduleBudget();
    },

    repaint(reference, slot) {
      const entry = sessions.get(terminalDisplayKey(reference));
      if (!entry || entry.slot !== slot || !entry.display) return;
      const term = entry.display.term;
      term.refresh(0, term.rows - 1);
    },

    setVisible(reference, slot, visible) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return;
      // A lender's shown state is kept, so the display goes back to it shown or hidden as it is now.
      const lender = entry.lenders.find((l) => l.slot === slot);
      if (lender && lender.visible !== visible) {
        lender.visible = visible;
        notifyShown(entry);
      }
      if (entry.slot !== slot || entry.visible === visible) return;
      entry.visible = visible;
      notifyShown(entry);
      if (visible) {
        touch(entry);
        armRefusedRetry(entry);
      }
      const d = entry.display;
      if (!d) {
        if (visible && entry.evicted && !entry.opening) void revive(entry);
        return;
      }
      if (visible) {
        applyAppearance(entry);
        // A canvas can come back from `display:none` with an empty drawing
        // buffer, and xterm only redraws on change.
        d.term.refresh(0, d.term.rows - 1);
      } else {
        d.keys.clearRepeat();
      }
      leases.setVisible(d.instanceId, shown(entry));
      if (!visible) scheduleBudget();
    },

    requestResize(reference, cols, rows) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry || !currentTerminalDisplay(entry.reference) || cols <= 0 || rows <= 0) return;
      const refused = entry.refusedSize;
      if (refused && refused.cols === cols && refused.rows === rows && !entry.retryRefused) return;
      const current = manager.size(entry.reference);
      if (current && current.cols === cols && current.rows === rows) return;
      entry.refusedSize = null;
      entry.retryRefused = false;
      entry.size = { cols, rows };
      rememberTerminalSize(cols, rows);
      // Still restoring: the restore asks for this size once built. An
      // evicted display's session keeps running at the size asked for.
      if ((entry.display || entry.evicted) && !entry.exited) {
        resizePty(terminalSessionReference(entry.reference), cols, rows).catch(() =>
          forgetRefusedSize(entry, cols, rows),
        );
      }
    },

    size(reference) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return null;
      if (entry.size) return { ...entry.size };
      const d = entry.display;
      return d ? { cols: d.term.cols, rows: d.term.rows } : null;
    },

    sized(reference) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry || atSize(entry)) return Promise.resolve();
      return new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(timer);
          entry.sizeWaiters.delete(done);
          resolve();
        };
        const timer = setTimeout(done, SIZE_SETTLE_BOUND_MS);
        entry.sizeWaiters.add(done);
      });
    },

    holds(reference, slot) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.slot === slot;
    },

    holdsVisible(reference) {
      const entry = sessions.get(terminalDisplayKey(reference));
      return !!entry && holdsVisible(entry);
    },

    watchShown(reference, listener) {
      const sessionId = terminalDisplayKey(reference);
      let listeners = shownListeners.get(sessionId);
      if (!listeners) shownListeners.set(sessionId, (listeners = new Set()));
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0 && shownListeners.get(sessionId) === listeners) {
          shownListeners.delete(sessionId);
        }
      };
    },

    dispose(reference) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return;
      sessions.delete(sessionId);
      dropOutgoing(entry);
      if (entry.display) teardown(entry, entry.display);
      entry.listeners.clear();
      for (const waiter of [...entry.sizeWaiters]) waiter();
      unregisterHarnessEntry(sessionId);
      // Nothing is lent to a display that is gone: the page stops being watched for it.
      entry.lenders = [];
      entry.borrowed = false;
      notifyShown(entry);
    },

    evict(reference) {
      const entry = sessions.get(terminalDisplayKey(reference));
      if (!entry || !evictable(entry)) return false;
      evict(entry);
      return true;
    },

    memory,

    debugDump(reference) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      const d = entry?.display;
      if (!entry || !d) return Promise.reject(new Error(`no display for ${sessionId}`));
      return new Promise<DisplayDump>((resolve, reject) => {
        enqueue(d, {
          kind: "task",
          run: () =>
            resolve({
              instanceId: d.instanceId,
              created: entry.created,
              rebuilds: entry.rebuilds,
              restoredFromSnapshot: true,
              appliedSeq: d.appliedSeq,
              appliedResizeGen: d.appliedResizeGen,
              ...referenceBuffer(d.term),
            }),
          abort: () => reject(new Error(`display for ${sessionId} was disposed`)),
        });
      });
    },
    terminal(reference) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.display?.term ?? null;
    },
    terminalFor(machineId, sessionId) {
      for (const entry of sessions.values()) {
        const ref = entry.reference;
        const match =
          typeof ref === "string"
            ? machineId === "local" && ref === sessionId
            : ref.target.member.machineId === machineId && ref.target.session === sessionId;
        if (match && entry.display && !entry.display.disposed) return entry.display.term;
      }
      return null;
    },

    fitAddon(reference) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.display?.fit ?? null;
    },

    appliedSeq(reference) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.display?.appliedSeq ?? null;
    },

    focus(reference, policy = true) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry || !currentTerminalDisplay(entry.reference)) return;
      touch(entry);
      const d = entry.display;
      if (!d) {
        // Still restoring: focus it once built.
        if (entry.slot) entry.focusOnRestore = policy;
        return;
      }
      if (permitsFocus(policy)) {
        leases.focus(d.instanceId);
        d.term.focus();
      }
    },

    webglLease(reference) {
      const sessionId = terminalDisplayKey(reference);
      const d = sessions.get(sessionId)?.display;
      const state = d ? leases.state(d.instanceId) : null;
      return state ? { ...state, held: leases.held, cap: leases.cap } : null;
    },

    setAppearance(reference, appearance) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return;
      entry.appearance = appearance;
      applyAppearance(entry);
    },

    clearTextureAtlas(reference) {
      const sessionId = terminalDisplayKey(reference);
      const term = sessions.get(sessionId)?.display?.term;
      if (term && typeof term.clearTextureAtlas === "function") term.clearTextureAtlas();
    },

    atlasBuilt(reference, fontSizePx) {
      const term = sessions.get(terminalDisplayKey(reference))?.display?.term;
      return !!term && atlasFontPx.get(term) === fontSizePx;
    },

    markAtlasBuilt(reference, fontSizePx) {
      const term = sessions.get(terminalDisplayKey(reference))?.display?.term;
      if (term) atlasFontPx.set(term, fontSizePx);
    },

    routeInput(reference, route) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return () => {};
      entry.route = route;
      return () => {
        if (entry.route === route) entry.route = null;
      };
    },

    listen(reference, listener) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (!entry) return () => {};
      entry.listeners.add(listener);
      return () => entry.listeners.delete(listener);
    },

    findState(reference) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.find ?? null;
    },

    setFindState(reference, state) {
      const sessionId = terminalDisplayKey(reference);
      const entry = sessions.get(sessionId);
      if (entry) entry.find = state;
    },

    search(reference) {
      const sessionId = terminalDisplayKey(reference);
      return sessions.get(sessionId)?.display?.search ?? null;
    },
  };
  return manager;
}

/** The app's one terminal manager. */
export const terminalManager = createTerminalManager();

export interface HarnessTerminalEntry {
  readonly instanceId: string | null;
  readonly created: number;
  readonly rebuilds: number;
  readonly appliedSeq: number | null;
  debugDump(): Promise<DisplayDump>;
  /** Force the display's WebGL context lost, as a GPU reset would. False when
   *  it holds no WebGL context. */
  loseContext(): boolean;
}

declare global {
  interface Window {
    /** Webdriver builds only: every live display's counters and dump, by
     *  session id. Proven absent from release builds. */
    __csTerminals?: Record<string, HarnessTerminalEntry>;
    /** Webdriver builds only: force a hidden display's eviction, and read the
     *  manager's live display count and buffer estimate. */
    __CODESENSE_TERMINAL_EVICT_E2E__?: {
      evict(sessionId: string): boolean;
      memory(): TerminalMemory;
    };
  }
}

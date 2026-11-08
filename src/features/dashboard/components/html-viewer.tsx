// SPDX-License-Identifier: MIT
import { ReloadStateCapture } from "../lib/reload-state";
import { useOwningScope } from "@/shared/lib/warm-scopes";
import { FrameOutbox } from "../lib/frame-outbox";
import {
  bridgeTiming,
  installBridgeTimingProbes,
  lagField,
  noteHostMessage,
  nowMs,
  safeCmd,
} from "../lib/bridge-timing";
import { usePaneFileDrop } from "../hooks/use-pane-file-drop";
import { deliverMenuAction } from "./menu-action-delivery";
import { hostedMenuRect } from "./hosted-menu-rect";
import { HostMenuContext, type HostMenuRequest } from "@/features/shell/host-menu-context";
import {
  registerFrame,
  acknowledgeFlush,
  markFrameLive,
  markFrameReady,
} from "../lib/frame-lifecycle";
import { focusActiveWorkspaceTerminal } from "@/features/viewer/lib/focus-cascade";
import { buildTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import { useViewerStore, selectFocusedSession } from "@/shared/stores/viewer-store";
import { messageFrameHeight } from "@/features/weft/lib/inbox-message-frame";
import { matchShortcut } from "@/shared/hooks/use-shortcut-handler";
import { dispatchMenuAction, getMenuActionGate } from "@/app/menu-action-dispatch";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import {
  registerRenderedDocument,
  type RenderedDocumentFormat,
} from "@/shared/hooks/use-rendered-document-bridge";
import { releaseFrame, type FrameRecord, type RenderedDocumentCapture } from "@/tauri/commands";
import {
  useState,
  useContext,
  useEffect,
  useLayoutEffect,
  useCallback,
  useMemo,
  useRef,
  type ReactNode,
} from "react";
import { liveHub, paneViewKey, type HubFrame } from "../lib/live-hub";
import {
  DOCK_FRAME_PANEL_ID,
  FILE_NAV_FRAME_PANEL_ID,
  SLOT_REGISTRY,
  type SlotId,
} from "@/features/shell/slots/slot-registry";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Camera } from "lucide-react";
import { toast } from "sonner";
import { bridgeResolveOpenTarget, getHttpApiPort, writeContentFile } from "@/tauri/commands";
import { switchContext } from "@/features/viewer/lib/context-switch";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  HTML_FRAME_ALLOW,
  buildBaselineViewerUrl,
  buildMessageViewerUrl,
  buildPaneViewerUrl,
  buildViewerUrl,
  resolveViewerLinkPath,
  snapshotArchivePath,
  snapshotTimestamp,
  type CommonViewerProps,
} from "@/shared/lib/viewer-file-type";
import { FileDeletedPlaceholder } from "@/shared/components/file-deleted-placeholder";
import { closeActiveFileTab } from "@/shared/lib/close-active-file-tab";
import { useEffectiveRefreshKey } from "@/shared/hooks/use-effective-refresh-key";
import { useInstanceCount } from "@/shared/hooks/use-instance-count";
import { useActionGate } from "./use-action-gate";
import { mcpAppsReply } from "./mcp-apps-binding";
import { useFrameRecord } from "./use-frame-record";
import { useRegisterDashboardLaunchController } from "./dashboard-launch-controller";
import { type DocTheme } from "./doc-theme";
import { usePaneFirstPaint } from "./pane-startup-surface";
import { useNavSelectionPush, type NavSelection } from "./nav-selection";
import { dashboardPrefsKey } from "../lib/dashboard-prefs";
import { pageSdk } from "../lib/page-sdk";
import { FRAME_PRESENTATION_UNAVAILABLE } from "../lib/frame-presentation";
import { READ_VERBS, resolveFrameCall, type BridgeMessageIdentity } from "./html-bridge";

// SW-SD-1: cap the wait for a doc to answer a `codesense:snapshot-request` so a
// wedged/never-responding doc can't strand a pending correlation entry or leave
// the capture hanging. A snapshot is a one-shot user action; 10s is generous.
/** A frame whose page never acks its theme still reveals after this. */
const THEME_ACK_FALLBACK_MS = 500;
const SNAPSHOT_TIMEOUT_MS = 10_000;

// Child-frame focus does not bubble into the host document.
const focusOwner: {
  current: HTMLElement | null;
  previous: WeakMap<HTMLElement, HTMLElement>;
  returning: WeakSet<HTMLElement>;
} = { current: null, previous: new WeakMap(), returning: new WeakSet() };
function recordFocusOwner(element: HTMLElement): void {
  if (focusOwner.current === element) return;
  if (focusOwner.current) focusOwner.returning.delete(focusOwner.current);
  if (focusOwner.current && !focusOwner.returning.has(element))
    focusOwner.previous.set(element, focusOwner.current);
  focusOwner.current = element;
}

// A live doc renders first-class: a real (non-opaque) origin plus full web
// capabilities, parity with a browser tab. The frame is served from
// a fresh frame hostname, cross-origin to the `tauri://` app webview, so
// `allow-same-origin` cannot escape the sandbox or reach `window.__TAURI__`
// (the codesense bridge is a postMessage RPC, not direct invoke). A baseline
// render is a historical blob shown for comparison only, so it keeps the
// opaque-origin, scripts-only sandbox and gets no bridge.
const LIVE_SANDBOX =
  "allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads";
const BASELINE_SANDBOX = "allow-scripts";

// SF-1: HtmlViewer accepts the shared viewer contract directly. `teamContentPath`
// is the single root used for both `buildViewerUrl` (iframe URL) and
// `resolveViewerLinkPath` (internal-link postMessage). The legacy
// `contextRepoRoot` prop was retired here.
//
// SW-FX-5: `changedPaths` mirrors the markdown viewer's reload gate
// (`markdown-viewer.tsx`). `refreshKey` is a per-repo key bumped on any
// watcher tick, so without a gate every open HTML iframe would full-reload
// on unrelated changes. We only advance the iframe's effective refresh key
// when this file is in the changed set.
type HtmlViewerProps = CommonViewerProps & {
  viewStateKey?: string;
  pendingHash?: string | null;
  onPendingHashConsumed?: () => void;
  frameTitle?: string;
  /** The slot still displays fallback while this frame retries. */
  captureUnavailable?: string;
  /** Off-screen replacements warm data without capturing or consuming visible-view intents. */
  presented?: boolean;
  /** A follows-context frame kept for a team that is not on screen (another org's dock) stays bound
   *  to it instead of re-binding to the active team. */
  keepTeam?: boolean;
  /** Optional handoff from the frozen document; navigation never waits for it. */
  reloadState?: Promise<unknown> | null;
  /** Host-owned window scroll carried into a replacement document. */
  initialScrollY?: number;
  /** Deliver selection before an off-screen replacement is presented, without focusing it. */
  prepareIntent?: boolean;
  /** Slot-owned reloads use the same buffer as source saves. */
  onReloadPane?: () => void;
  onCandidateFocus?: () => void;
  onFrameElement?: (element: HTMLIFrameElement | null) => void;
  allowAutoFocus?: boolean;
  /** A minted child rendered inside a pane; this host owns its bridge, not its DOM. */
  hostedFrame?: FrameRecord;
  /** Overlay owner when this viewer bridges a message nested inside a pane. */
  parentPanelId?: string;
  parentElement?: HTMLIFrameElement | null;
  hostTheme?: DocTheme | null;
  parentDashboard?: HostMenuRequest["dashboard"];
  parentMenuPage?: HostMenuRequest["page"];
  changedPaths?: Set<string> | null;
  /**
   * SW-GS-3: when set, render the file's PUBLISH-BASELINE version (the "before"
   * side of the diff toggle) from the git blob instead of the working tree.
   */
  baselineSource?: { orgRoot: string; relPath: string; tree?: string } | null;
  /**
   * Render a custom right pane: the file `fileName` in `panesDir` on the `/pane`
   * route, with the SDK's reads bound to `contentRoot`. A pane has no Snapshot
   * band and does not close the centre tab on ⌘W.
   */
  pane?: {
    contentRoot: string;
    panesDir: string;
    fileName: string;
    dashboardRef?: import("@/tauri/commands").DashboardRef;
    builtinTeam?: import("@/tauri/commands").FrameTeam;
    slotId?: string;
    panelId?: string;
    /** Architecture: static declaration forwarded by the pane entry. */
    badge?: "live";
  } | null;
  /** Told about every problem the doc hits, so the host can show it in place:
   *  a failed load, an uncaught error or rejection inside the doc, and a rejected
   *  bridge call (which still rejects in the doc unchanged). */
  onDocProblem?: (problem: DocProblem) => void;
  /** Told when the doc now loaded announces itself: it was served and its
   *  shim runs, so any earlier load problem no longer holds. */
  onDocLoaded?: () => void;
  /** Explicit page readiness, independent of the transport handshake. */
  onDocLive?: () => void;
  /** A new document/frame attempt starts, independently of shim transport readiness. */
  onPageStart?: (documentKey: string) => void;
  /** The current iframe document and its external assets have finished loading. */
  onPageLoad?: () => void;
  onFrameViewKey?: (viewKey: string) => void;
  /** Rendered instead of the default deleted-file placeholder on a 404. */
  renderMissing?: () => ReactNode;
  /** Whether the host shows the doc right now (default true); the doc hears it,
   *  together with the app window's own visibility, through `onVisibility`. */
  visible?: boolean;
  intent?: Record<string, unknown>;
  onIntentDelivered?: () => void;
  /** Told when the doc sets or clears its chrome badge (`codesense.setBadge`). */
  onBadge?: (badge: DocBadge | null) => void;
  /**
   * Render an `html` inbox message as a live doc instead of a file: served on the
   * `/message` route with the same shim, bridge and action gate a dashboard file
   * gets. `filePath` is then the message's doc key ({@link messageDocKey}), which
   * the action gate and launch controller key on, and `teamContentPath` is the
   * message's team content root (bridge reads anchor there). Like a pane, it has
   * no Snapshot band and does not close the centre tab on ⌘W.
   */
  message?: BridgeMessageIdentity | null;
  /** Told the doc's laid-out height whenever it changes (message docs report it),
   *  so an inline host can size the frame to its content. */
  onContentSize?: (height: number) => void;
};

export { messageDocKey } from "@/features/weft/lib/inbox-message-frame";

export type DocBadgeSeverity = "info" | "warning" | "error";

/** A count a doc asks its host to show in the chrome. */
export interface DocBadge {
  count: number;
  severity: DocBadgeSeverity;
}

/** A doc's badge message as a badge (`null` clears it), or `undefined` when malformed. */
function parseDocBadge(raw: unknown): DocBadge | null | undefined {
  if (raw === null) return null;
  if (!raw || typeof raw !== "object") return undefined;
  const { count, severity } = raw as { count?: unknown; severity?: unknown };
  if (typeof count !== "number" || !Number.isFinite(count) || count < 0) return undefined;
  const sev = severity === "warning" || severity === "error" ? severity : "info";
  return count === 0 ? null : { count: Math.floor(count), severity: sev };
}

/** Match a key forwarded from a frame against the registry with the host's UI gate,
 *  and run what it finds. */
function dispatchFrameKey(
  key: KeyboardEventInit & { key: string; code: string },
  editorTarget = false,
  textFieldTarget = false,
): boolean {
  const action = matchShortcut(
    new KeyboardEvent("keydown", key),
    useShortcutsStore.getState().shortcuts,
    getMenuActionGate(),
    editorTarget,
    textFieldTarget,
  );
  if (action) dispatchMenuAction(action.id);
  return !!action;
}

/** What a doc's `sessions()` would return now, as a comparable key. Every live
 *  viewer asks on every roster write (heartbeats included), so the key is built
 *  once per change of either store, not once per viewer. */
/** Whether `frame` is laid out on screen: false while it or an ancestor is
 *  `display:none` (an inactive kept-alive tab, a collapsed section) or it has no
 *  area. True where
 *  the browser can't tell. */
function useFrameShown(frame: HTMLIFrameElement | null): boolean {
  const [shown, setShown] = useState(true);
  useEffect(() => {
    if (!frame || typeof IntersectionObserver !== "function") return;
    const observer = new IntersectionObserver((entries) => {
      const entry = entries[entries.length - 1];
      if (entry)
        setShown(entry.boundingClientRect.width > 0 && entry.boundingClientRect.height > 0);
    });
    observer.observe(frame);
    return () => observer.disconnect();
  }, [frame]);
  return shown;
}

/** Whether the app window itself is on screen. */
function useDocumentVisible(): boolean {
  const [shown, setShown] = useState(() => document.visibilityState !== "hidden");
  useEffect(() => {
    const onChange = () => setShown(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", onChange);
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);
  return shown;
}

/** How long a doc whose load pre-check failed at the network level (no HTTP
 *  status) has to announce itself before the failure is reported. */
export const NETWORK_PROBLEM_GRACE_MS = 5000;

/** A problem a live doc hit, for its host to show. */
export interface DocProblem {
  kind: "load" | "script" | "bridge";
  message: string;
}

/**
 * Port resolution state:
 * - `undefined` → still resolving
 * - `null`      → server disabled
 * - `number`    → resolved port
 */
type PortState = number | null | undefined;

// SW-SD-1: the single origin-pinned reply for EVERY doc→parent RPC branch
// (`codesense:invoke`, `codesense:snapshot`). Centralizing this keeps the
// security-critical SW-LD-1 r3 rule in ONE place so the two callers can't drift:
//   - Reply ONLY to `source` — the exact window that made the request — never a
//     re-read `iframeRef.contentWindow`. A frame that reloaded/navigated
//     mid-flight is a detached window → `postMessage` is a safe no-op (no
//     cross-doc misdelivery, no stale-id collision).
//   - A live doc's reply carries real DATA, so pin the target origin to the
//     registered frame origin — a frame that navigated
//     away can't receive it.
//   - A baseline frame's denial reply MUST use "*": it renders opaque-origin
//     ("null"), which can NEVER match a pinned origin, so a pinned reject is
//     silently dropped and the doc's Promise hangs forever.
function replyToDoc(
  source: Window | null,
  resultType: "codesense:invoke-result" | "codesense:snapshot-result",
  id: number,
  origin: string | undefined,
  live: boolean,
  payload: { ok: boolean; data?: unknown; error?: string },
): void {
  const targetOrigin = live ? origin : "*";
  if (!targetOrigin) return;
  source?.postMessage({ type: resultType, id, ...payload }, targetOrigin);
}

// SV-ENG-120: a hidden tab document makes no reads. A read that arrives while it is hidden waits
// for the tab to be shown, then runs in one flush. Panes and messages are exempt: a closed pane
// keeps its badge live on purpose. Subscriptions and the identity call stay immediate: they cost
// nothing and a page needs them to start.
const HIDDEN_IMMEDIATE_VERBS = new Set(["subscribe", "unsubscribe", "context"]);
const isHeldRead = (command: unknown) =>
  typeof command === "string" &&
  READ_VERBS.includes(command) &&
  !HIDDEN_IMMEDIATE_VERBS.has(command);
const HELD_READ_LIMIT = 64;
const HELD_READ_BUSY = "busy: too many reads held while this tab is hidden; retry when shown";
interface HeldRead {
  documentKey: string | null;
  key: string | null;
  invoke: () => Promise<unknown>;
  answers: Array<(call: Promise<unknown>) => void>;
}
// Same verb and args merge into one held call; unserialisable args never merge.
const heldReadKey = (command: unknown, args: unknown): string | null => {
  try {
    return `${String(command)}\u0000${JSON.stringify(args) ?? ""}`;
  } catch {
    return null;
  }
};

export function HtmlViewer({
  filePath,
  frameTitle,
  captureUnavailable,
  presented = true,
  prepareIntent = false,
  onReloadPane,
  onCandidateFocus,
  onFrameElement,
  allowAutoFocus = true,
  reloadState = null,
  initialScrollY = 0,
  hostTeamKey,
  keepTeam = false,
  refreshKey,
  teamContentPath,
  onNavigate,
  changedPaths,
  baselineSource,
  pane,
  onDocProblem,
  onDocLoaded,
  onDocLive,
  onPageStart,
  onPageLoad,
  onFrameViewKey,
  renderMissing,
  visible = true,
  intent,
  onIntentDelivered,
  onBadge,
  message,
  onContentSize,
  hostedFrame,
  parentPanelId,
  parentElement,
  hostTheme,
  parentDashboard,
  parentMenuPage,
  pendingHash,
  onPendingHashConsumed,
  viewStateKey,
}: HtmlViewerProps) {
  const paneRef = pane?.dashboardRef;
  const overlayPanelId =
    parentPanelId ??
    // The dock is its own instance: right-pane retirement and focus never match it.
    (pane?.slotId === "dock"
      ? DOCK_FRAME_PANEL_ID
      : pane?.slotId === "file-nav"
        ? FILE_NAV_FRAME_PANEL_ID
        : undefined) ??
    pane?.panelId ??
    (paneRef && paneRef.scope !== "built-in"
      ? paneRef.scope === "private"
        ? `pane:private:${paneRef.org}/${paneRef.id}`
        : `pane:${paneRef.org}:${paneRef.team}/${paneRef.id}`
      : paneRef
        ? pane?.panelId
        : undefined);
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const routeKey = viewStateKey ?? filePath;
  const routeRef = useRef({
    key: routeKey,
    hash: viewStateKey ? (useViewerStore.getState().readViewState(viewStateKey)?.hash ?? "") : "",
  });
  if (routeRef.current.key !== routeKey)
    routeRef.current = {
      key: routeKey,
      hash: viewStateKey ? (useViewerStore.getState().readViewState(viewStateKey)?.hash ?? "") : "",
    };
  const rememberRoute = useCallback(
    (hash: string) => {
      routeRef.current.hash = hash;
      if (viewStateKey) {
        const store = useViewerStore.getState(),
          previous = store.readViewState(viewStateKey);
        if (previous?.hash !== hash) store.captureViewState(viewStateKey, { ...previous, hash });
      }
    },
    [viewStateKey],
  );
  const hostedWindow = useRef<Window | null>(null);
  const menuLifetime = useRef(new AbortController());
  const menuActionsEnabled = useRef(false);
  const endpoint = useCallback(
    () =>
      hostedFrame
        ? { contentWindow: hostedWindow.current, src: hostedFrame.origin }
        : iframeRef.current,
    [hostedFrame],
  );
  const [children, setChildren] = useState<FrameRecord[]>([]);
  const childRecords = useRef<FrameRecord[]>([]);
  const parentRecord = useRef<FrameRecord | null>(null);
  const releaseMessage = useCallback((frameId: string) => {
    const removed = childRecords.current.filter((r) => r.frameId === frameId);
    for (const record of removed) void releaseFrame(record.frameId).catch(console.error);
    childRecords.current = childRecords.current.filter((r) => !removed.includes(r));
    setChildren(childRecords.current);
  }, []);
  const embedMessage = useCallback((record: FrameRecord) => {
    if (record.parentFrame !== parentRecord.current?.frameId) {
      void releaseFrame(record.frameId).catch(console.error);
      return;
    }
    childRecords.current = [...childRecords.current, record];
    setChildren(childRecords.current);
  }, []);
  const [frame, setFrame] = useState<HTMLIFrameElement | null>(null);
  const focusElement = hostedFrame ? parentElement : frame;

  const [retiring, setRetiring] = useState(false);
  const retiringRef = useRef(false);
  const documentVisible = useDocumentVisible();
  const frameShown = useFrameShown(frame);
  const docVisible = visible && documentVisible && frameShown && !retiring;
  const unsaved = useViewerStore((s) =>
    Object.values(s.editingDirtyPaths ?? {}).some((paths) => paths.includes(filePath ?? "")),
  );
  const [port, setPort] = useState<PortState>(
    hostedFrame ? Number(new URL(hostedFrame.origin).port) : undefined,
  );
  const frameDocumentPath = baselineSource
    ? `${baselineSource.orgRoot.replace(/\/$/, "")}/${baselineSource.relPath.replace(/^\/+/, "")}`
    : filePath;
  const slotId = baselineSource
    ? "baseline"
    : message
      ? "message"
      : pane
        ? (pane.slotId ?? "right-pane")
        : "centre";
  const minted = useFrameRecord(
    port,
    !hostedFrame && filePath && frameDocumentPath
      ? {
          documentPath: frameDocumentPath,
          contentRoot: pane?.contentRoot ?? teamContentPath,
          slot: slotId,
          ...(pane?.builtinTeam ? { builtinTeam: pane.builtinTeam } : {}),
          ...(message && !baselineSource
            ? { message: { org: message.org, team: message.team, messageId: message.messageId } }
            : {}),
        }
      : null,
    {
      visible: visible && documentVisible && frameShown,
      retiring,
      unsaved,
    },
  );
  const frameRecord = hostedFrame ?? minted.frameRecord;
  // Architecture: minting chooses a usable root for bundled readers independently of checkout availability.
  const paneContentRoot =
    frameRecord?.kind === "builtin" ? frameRecord.contentRoot : pane?.contentRoot;
  useLayoutEffect(() => {
    parentRecord.current = frameRecord;
    return () => {
      parentRecord.current = null;
    };
  }, [frameRecord]);
  const frameError = minted.frameError;
  const remintFrame = minted.remintFrame;
  const rebindTeam = minted.rebindTeam;
  const frameOrigin = frameRecord?.origin;
  const [presentation, setPresentation] = useState("docked");
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    if (!frame) return;
    const measure = () => {
      setWidth(frame.getBoundingClientRect().width);
      setPresentation(frame.closest('[data-fullscreen="true"]') ? "overlay" : "docked");
    };
    measure();
    const resize = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    resize?.observe(frame);
    const mutation = new MutationObserver(measure);
    let parent = frame.parentElement;
    while (parent) {
      mutation.observe(parent, { attributes: true, attributeFilter: ["data-fullscreen"] });
      parent = parent.parentElement;
    }
    return () => {
      resize?.disconnect();
      mutation.disconnect();
    };
  }, [frame]);
  const [fileDeleted, setFileDeleted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // RF-perf-4 (AC-1): count this iframe-backed viewer for the gated memory line.
  // Inactive-tab HtmlViewers are kept mounted (display:none) by the keep-alive
  // LRU, holding a sandbox context each — this attributes that footprint.
  useInstanceCount("iframe");
  // SW-FX-5: latest scroll offset reported by the injected script for this
  // file. The iframe is opaque-origin (no storage), so the parent is the
  // durable store. One ref suffices — each HtmlViewer instance is pinned to
  // a single `filePath` via `key={p}` in team-workspace-view. The reset
  // below is belt-and-suspenders so the ref can never carry one file's
  // offset into another even if that keying ever changes.
  const scrollYRef = useRef(initialScrollY);
  useEffect(() => {
    scrollYRef.current = initialScrollY;
  }, [filePath, initialScrollY]);

  // SW-SD-1: correlation for the parent→doc snapshot-request round-trip. The
  // parent owns the id here (mirror of the doc shim's `_n`/`_p`, now on the
  // parent). Each entry carries its timeout handle so a settle can cancel it.
  const snapshotWaitersRef = useRef(
    new Map<
      number,
      {
        resolve: (capture: RenderedDocumentCapture) => void;
        reject: (e: Error) => void;
        timer: number;
        format?: RenderedDocumentFormat;
        sourceUrl: string;
      }
    >(),
  );
  const snapshotIdRef = useRef(0);
  const rejectCaptures = useCallback(() => {
    snapshotWaitersRef.current.forEach((w) => {
      clearTimeout(w.timer);
      w.reject(new Error("Document closed or navigated; select the current document and retry."));
    });
    snapshotWaitersRef.current.clear();
  }, []);
  // Clear any in-flight snapshot timers on unmount so a late timeout can't fire
  // a toast after the viewer is gone.
  useEffect(() => {
    const waiters = snapshotWaitersRef.current;
    return () => {
      waiters.forEach((w) => {
        clearTimeout(w.timer);
        w.reject(new Error("Document closed"));
      });
      waiters.clear();
    };
  }, []);

  // RF-perf-15 (F-1): blank the iframe on teardown so WebKit deterministically
  // tears down the nested child document + its compositor/GPU layers +
  // decoded-image cache, instead of retaining them well past React's detach
  // (this viewer is kept mounted display:none by the keep-alive LRU, so "unmount"
  // is an LRU eviction — exactly when we want the native backing back).
  //
  // A callback ref (stable identity — never re-attaches, so it can't fire mid-life)
  // with a React 19 cleanup return blanks the node the instant it detaches: on true
  // unmount AND on a remount, with the real node in
  // scope. This beats a mount-time capture, which would miss the node entirely —
  // the iframe is rendered only after `port` resolves, so it isn't in the DOM yet
  // when a `[]` effect first runs. `src` is 100% render-driven (no async loader),
  // so this can't race a pending load or flash a spurious reload (OQ-1). O(1).
  // The mounted frame, as state: the theme and on-screen reads follow it.
  const onFrameElementRef = useRef(onFrameElement);
  onFrameElementRef.current = onFrameElement;
  const attachIframe = useCallback(
    (node: HTMLIFrameElement | null) => {
      iframeRef.current = node;
      setFrame(node);
      onFrameElementRef.current?.(node);
      if (!node) return;
      return () => {
        rejectCaptures();
        // Blank only a node that really left the document. React 19 StrictMode
        // runs a ref's cleanup and re-attaches the SAME node on mount; blanking it
        // then would leave a live frame on about:blank, since React never re-applies
        // an unchanged `src`. A microtask later a true detach is complete.
        // Teardown must never throw (a thrown cleanup breaks React's unmount of the
        // rest of the tree). Setting `src` to about:blank is a safe navigation in a
        // real browser; guard only against DOM shims that reject the navigation.
        queueMicrotask(() => {
          if (node.isConnected) return;
          try {
            node.src = "about:blank";
          } catch {
            // best-effort reclamation
          }
        });
        // Only clear the shared ref if it still points at THIS node — on a keyed
        // remount React may attach the new node before running the old node's
        // cleanup, and an unconditional null would clobber the live new registration.
        if (iframeRef.current === node) {
          iframeRef.current = null;
          onFrameElementRef.current?.(null);
        }
      };
    },
    [rejectCaptures],
  );

  // SW-SD-1: the shared archive seam — build the ABSOLUTE context path and reuse
  // the existing watcher-suppressed `writeContentFile`. Kept independent of the
  // button/message plumbing so SW-SD-3's rolling capture can call it directly.
  // A relative path would be rejected by Rust `validate_context_path`, hence the
  // `snapshotArchivePath` helper (see the story's Validation review #1).
  const archiveSnapshot = useCallback(
    async (html: string, name?: string): Promise<void> => {
      if (!filePath) throw new Error("no active file");
      // Uniqueness (review r1): the sortable UTC stamp is millisecond-resolution,
      // so two captures in the same ms — a double-click, or SW-SD-3's rolling
      // verb — would resolve to the SAME path and the second write would silently
      // clobber the first (`write_content_file` overwrites). A short random token
      // makes each snapshot a distinct git object by construction; the timestamp
      // still orders them chronologically. Kept in the caller so the archive-path
      // helper stays pure/deterministic.
      const nonce = Math.floor(Math.random() * 0x100000000).toString(36); // 1–7 base36 chars, never empty
      const stamp = `${snapshotTimestamp(new Date())}-${nonce}`;
      const path = snapshotArchivePath(teamContentPath, filePath, name, stamp);
      await writeContentFile(path, html);
    },
    [filePath, teamContentPath],
  );

  // SW-SD-1: chrome-button trigger. Post `snapshot-request` into the (live,
  // real-origin) doc pinned to its origin — never "*" into a live frame —
  // await the correlated frozen HTML, then archive. Fully async: nothing on the
  // render path awaits this (AC-6, non-blocking).
  const captureDocument = useCallback(
    (format?: RenderedDocumentFormat): Promise<RenderedDocumentCapture> => {
      if (format && captureUnavailable) return Promise.reject(new Error(captureUnavailable));
      const frame = iframeRef.current;
      const cw = frame?.contentWindow;
      if (!cw || typeof port !== "number")
        return Promise.reject(new Error("Document is not ready"));
      if (format && !frame.getClientRects().length) {
        return Promise.reject(
          new Error(
            "Select this document's tab or show its pane in CodeSense and retry; hidden pages are not captured.",
          ),
        );
      }
      const id = ++snapshotIdRef.current;
      const sourceUrl = frame.src;
      return new Promise<RenderedDocumentCapture>((resolve, reject) => {
        const timer = window.setTimeout(() => {
          if (snapshotWaitersRef.current.delete(id))
            reject(new Error("Document capture timed out; select the current document and retry."));
        }, SNAPSHOT_TIMEOUT_MS);
        snapshotWaitersRef.current.set(id, { resolve, reject, timer, format, sourceUrl });
        cw.postMessage(
          { type: "codesense:snapshot-request", id, format, expectedUrl: sourceUrl },
          frameOrigin ?? "*",
        );
      });
    },
    [port, frameOrigin, captureUnavailable],
  );

  const handleSnapshotClick = useCallback(() => {
    void captureDocument()
      .then((capture) => archiveSnapshot(capture.content))
      .then(() => toast.success("Snapshot saved to context."))
      .catch(() => toast.error("Couldn't save snapshot.", { id: `snapshot-error:${filePath}` }));
  }, [captureDocument, archiveSnapshot, filePath]);

  // SW-FX-5 (AC-7): gate the per-repo `refreshKey` down to a per-file key so
  // a watcher tick only reloads the iframe whose file actually changed —
  // unrelated/hidden tabs hold their prior key and never reload. Mirrors
  // `markdown-viewer.tsx`'s changedPaths skip.
  const effectiveRefreshKey = useEffectiveRefreshKey(refreshKey, changedPaths, filePath);

  // Every doc renders first-class with the bridge, except a baseline/diff render
  // (`baselineSource`): a read-only "before"/redline blob from a historical or
  // other tree, shown for comparison, never a running dashboard.
  const live = !baselineSource;
  const isPane = !!pane;
  // A pane or a message sits inside another surface: no Snapshot band, no ⌘W tab
  // close, themed to where it sits, links resolve from the team content root.
  const embedded = isPane || !!message;
  const messageOrg = message?.org;
  const messageTeam = message?.team;
  const messageId = message?.messageId;

  // Resolve the HTTP API port once.
  useEffect(() => {
    let cancelled = false;
    getHttpApiPort()
      .then((p) => {
        if (!cancelled) setPort(p);
      })
      .catch(() => {
        if (!cancelled) {
          setPort(null);
          setError("Failed to connect to HTTP API server");
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Build the iframe URL from resolved port + props. The base64-encoded
  // root used here is `teamContentPath` (post-PTS-2); the prior
  // `contextRepoRoot` divergence was the root cause of SF-1 Bug A.
  //
  const firstPaint = usePaneFirstPaint(
    `${frameRecord?.frameId}:${frameDocumentPath}:${effectiveRefreshKey}`,
    live,
    docVisible,
    hostTheme,
  );
  const candidateAtBoot = useRef(!presented).current;
  const baseViewerUrl =
    port == null ||
    !filePath ||
    !frameRecord ||
    frameRecord.documentPath !== frameDocumentPath ||
    (live && !firstPaint.initialTheme)
      ? null
      : baselineSource
        ? buildBaselineViewerUrl(
            frameRecord.origin,
            baselineSource.orgRoot,
            baselineSource.relPath,
            {
              tree: baselineSource.tree,
              v: effectiveRefreshKey,
            },
          )
        : pane
          ? buildPaneViewerUrl(port, paneContentRoot!, pane.panesDir, pane.fileName, {
              v: effectiveRefreshKey,
              origin: frameRecord.origin,
              theme: firstPaint.initialTheme,
            })
          : message
            ? buildMessageViewerUrl(port, message.org, message.team, message.messageId, {
                origin: frameRecord.origin,
                theme: firstPaint.initialTheme,
              })
            : buildViewerUrl(port, teamContentPath, filePath, {
                v: effectiveRefreshKey,
                origin: frameRecord.origin,
                theme: firstPaint.initialTheme,
              });
  const viewerUrl =
    baseViewerUrl && candidateAtBoot
      ? `${baseViewerUrl}${baseViewerUrl.includes("?") ? "&" : "?"}codesenseCandidate=1`
      : baseViewerUrl;

  useEffect(() => {
    if (!presented || !live || !filePath || !viewerUrl || fileDeleted) return;
    const unregister = registerRenderedDocument(filePath, captureDocument, frameRecord?.frameId);
    return () => {
      unregister();
      rejectCaptures();
    };
  }, [
    presented,
    live,
    filePath,
    viewerUrl,
    fileDeleted,
    captureDocument,
    rejectCaptures,
    frameRecord?.frameId,
  ]);

  // Read through a ref so a host passing a fresh callback each render never
  // re-subscribes the message listener or re-runs the load check.
  const onDocProblemRef = useRef(onDocProblem);
  const onDocLoadedRef = useRef(onDocLoaded);
  const onDocLiveRef = useRef(onDocLive);
  const onFrameViewKeyRef = useRef(onFrameViewKey);
  const onPageStartRef = useRef(onPageStart);
  const onPageLoadRef = useRef(onPageLoad);
  useLayoutEffect(() => {
    onDocProblemRef.current = onDocProblem;
    onDocLoadedRef.current = onDocLoaded;
    onDocLiveRef.current = onDocLive;
    onFrameViewKeyRef.current = onFrameViewKey;
    onPageStartRef.current = onPageStart;
    onPageLoadRef.current = onPageLoad;
  }, [onDocProblem, onDocLoaded, onDocLive, onFrameViewKey, onPageStart, onPageLoad]);
  const documentKey = viewerUrl ? `${frameRecord?.frameId}:${viewerUrl}` : null;
  const outbox = useMemo(() => {
    const messages = new FrameOutbox();
    messages.begin(documentKey);
    return messages;
  }, [documentKey]);
  const postToFrame = useCallback(
    (
      target: { contentWindow: Window | null; src: string } | null,
      message: Record<string, unknown>,
    ) => {
      outbox.post(target, message);
    },
    [outbox],
  );
  const reloadCapture = useMemo(
    () => new ReloadStateCapture((message) => postToFrame(endpoint(), message), documentKey ?? ""),
    [documentKey, endpoint, postToFrame],
  );
  useEffect(() => () => reloadCapture.cancel(), [reloadCapture]);
  const currentDocumentKey = useRef(documentKey);
  currentDocumentKey.current = documentKey;
  const heldReads = useRef<HeldRead[]>([]);
  useLayoutEffect(() => {
    heldReads.current = [];
  }, [documentKey]);
  const reloadStateRef = useRef(reloadState);
  reloadStateRef.current = reloadState;
  useLayoutEffect(() => {
    outbox.begin(documentKey);
    if (documentKey) onPageStartRef.current?.(documentKey);
    return () => outbox.begin(null);
  }, [documentKey, outbox]);
  // SV-ENG-120: when each document load starts, whether it is hidden, and why it remounted.
  const docStartedAt = useRef(0);
  const timingGap = useRef(false);
  const timingPrev = useRef<{ frameId?: string; viewerUrl: string | null; live: boolean } | null>(
    null,
  );
  useLayoutEffect(() => {
    installBridgeTimingProbes();
    if (!documentKey) {
      // No document (frame evicted or released, or a mint pending): the next one names this gap.
      timingGap.current = true;
      return;
    }
    const prev = timingPrev.current;
    const next = { frameId: frameRecord?.frameId, viewerUrl, live };
    const why = !prev
      ? "first-mount"
      : [
          prev.frameId !== next.frameId && "frame-id-changed",
          prev.viewerUrl !== next.viewerUrl && "viewer-url-changed",
          prev.live !== next.live && "live-toggled",
        ]
          .filter(Boolean)
          .join(",") || "document-key-changed";
    timingPrev.current = next;
    const gap = timingGap.current;
    timingGap.current = false;
    docStartedAt.current = nowMs();
    bridgeTiming(
      () =>
        `doc-start doc=${documentKey} file=${filePath} t=${docStartedAt.current} visible=${docVisible} documentVisibility=${document.visibilityState} why=${gap ? `after-release:${why}` : why}`,
    );
    return () =>
      bridgeTiming(() => `doc-end doc=${documentKey} t=${nowMs()} ready=${docReadyRef.current}`);
    // Only a new document is an event; the rest is read as of that load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentKey]);
  useEffect(() => {
    const reason = frameError ?? error ?? (port === null ? "HTTP server unavailable" : null);
    if (reason) onDocProblemRef.current?.({ kind: "load", message: `mint_frame: ${reason}` });
  }, [frameError, error, port]);
  // Whether the doc now loaded has announced itself; pushes wait for it, and a
  // new load announces again.
  const docReadyRef = useRef(false);
  const loadedDocumentKey = useRef<string | null>(null);
  const documentLoads = useRef(0);

  const slotStartup = !!pane;
  // Pre-check URL to detect 404 (file deleted).
  useEffect(() => {
    // Always reset on URL change so stale deleted state doesn't flash
    // through to the next file's render. Synchronous setState in an
    // effect is intentional here — `fileDeleted` is async (depends on
    // a fetch result) so it can't be a pure derived value, and we
    // explicitly want the reset committed before the fetch starts.
    setFileDeleted(false);
    if (!viewerUrl) return;
    let cancelled = false;
    let networkProblemTimer: ReturnType<typeof setTimeout> | undefined;

    fetch(viewerUrl, { method: "HEAD" })
      .then((resp) => {
        if (cancelled) return;
        setFileDeleted(resp.status === 404);
        if (!resp.ok && resp.status !== 404) {
          onDocProblemRef.current?.({
            kind: "load",
            message: `${filePath}: HTTP ${resp.status}${resp.statusText ? ` ${resp.statusText}` : ""}`,
          });
        }
      })
      .catch((err: unknown) => {
        // A network-level failure (no status at all) says nothing about the
        // iframe's own load, which may still succeed. A host that shows problems
        // learns the doc is unreachable only if the doc then never announces
        // itself; otherwise the iframe shows its own error.
        if (cancelled || docReadyRef.current || slotStartup) return;
        const message = `${filePath}: ${err instanceof Error ? err.message : String(err)}`;
        networkProblemTimer = setTimeout(() => {
          if (cancelled || docReadyRef.current) return;
          onDocProblemRef.current?.({ kind: "load", message });
        }, NETWORK_PROBLEM_GRACE_MS);
      });

    return () => {
      cancelled = true;
      clearTimeout(networkProblemTimer);
    };
  }, [viewerUrl, filePath, slotStartup]);

  // SW-DX-7: THIS live doc's execution-loop controller (openWorkspace/runPrompt).
  // Its bridge calls carry it, so a pane and a centre dashboard live side by side
  // each launch for their own doc.
  const launchController = useRegisterDashboardLaunchController(filePath, live);
  // A pane lives beside content/, so a context query anchors at the content root
  // it reads (its team's, or its private space's), as a dashboard there would. A
  // message anchors in its own team's content root the same way, so `context()`,
  // `queryContext` and `openFile` resolve to the message's team.
  const queryAnchor = pane
    ? `${paneContentRoot}/${pane.fileName}`
    : messageId !== undefined && teamContentPath
      ? `${teamContentPath}/inbox-message-${messageId}.html`
      : undefined;
  // SW-DNS-2: which dashboard instance this is, for its own `prefs`: a pane's own
  // file (a team and a private pane of one id differ), a message's id, else the
  // doc's path. Never something the doc names.
  const prefsKey = useMemo(
    () =>
      live
        ? dashboardPrefsKey({
            docPath: filePath ?? undefined,
            dashboardRef: pane?.dashboardRef,
            pane: pane ? { panesDir: pane.panesDir, fileName: pane.fileName } : undefined,
            message:
              messageOrg !== undefined && messageTeam !== undefined && messageId !== undefined
                ? { org: messageOrg, team: messageTeam, messageId }
                : undefined,
          })
        : null,
    [live, filePath, pane, messageOrg, messageTeam, messageId],
  );
  // `openFile` opens a file in the team on screen through the host's own opener.
  const hubFrameRef = useRef<HubFrame | null>(null);
  const hubFrameReadyRef = useRef<Promise<HubFrame> | null>(null);
  const liveFrame = useCallback(
    async () => hubFrameRef.current ?? (await hubFrameReadyRef.current),
    [],
  );
  const localTheme = firstPaint.theme;
  const theme = hostTheme === undefined ? localTheme : hostTheme;
  const showHostMenu = useContext(HostMenuContext);
  const presentationRef = useRef<(verb: string, args: unknown) => Promise<unknown>>(async () => {
    throw new Error(FRAME_PRESENTATION_UNAVAILABLE);
  });
  const present = useCallback(
    (verb: string, args: unknown) => presentationRef.current(verb, args),
    [],
  );
  const bridgeContext = useMemo(
    () => ({
      frame: frameRecord,
      theme,
      embedMessage,
      releaseMessage,
      presented,
      presentation: present,
      controller: launchController,
      liveFrame,
      prefsSource: pane?.dashboardRef?.installedFrom,
      ...(prefsKey ? { prefsKey } : {}),
      ...(queryAnchor ? { docPath: queryAnchor } : {}),
      ...(onNavigate ? { navigate: onNavigate } : {}),
      ...(frameRecord?.message
        ? { message: { ...frameRecord.message, sender: frameRecord.grant.principal } }
        : {}),
    }),
    [
      frameRecord,
      theme,
      embedMessage,
      releaseMessage,
      launchController,
      present,
      presented,
      liveFrame,
      pane?.dashboardRef?.installedFrom,
      prefsKey,
      queryAnchor,
      onNavigate,
    ],
  );

  // Every page call enters the native admission gate before a host handler runs.
  // The page's own readiness: its host hears it and queued commands are delivered.
  const docLive = useCallback(() => {
    if (frameRecord) markFrameLive(frameRecord.frameId);
    onDocLiveRef.current?.();
  }, [frameRecord]);
  const onAdmitted = useCallback(
    (verb: string) => {
      if (verb === "subscribe") docLive();
    },
    [docLive],
  );
  const { runGatedAction } = useActionGate(filePath, bridgeContext, onAdmitted);

  // The doc's theme, read where its frame sits, and whether it is on screen. Both
  // are pushed on change and answered when the shim announces itself, so a doc
  // that reloads hears them again.
  // Every live doc gets the theme's tokens and the server sets its initial UA
  // scheme before content. Later bridge updates only apply that scheme to panes
  // and centre docs marked theme-aware, preserving authored looks.
  const themeRef = useRef(theme);
  const visibleRef = useRef(docVisible);
  useEffect(() => {
    if (!live || !frameRecord) return;
    let disposed = false;
    hubFrameReadyRef.current = resolveFrameCall(frameRecord, {
      origin: frameRecord.origin,
      token: frameRecord.token,
    }).then(async (grant) => {
      const viewKey = await paneViewKey(frameRecord);
      onFrameViewKeyRef.current?.(viewKey);
      if (disposed) throw new Error("live frame unmounted");
      const subscription = liveHub.attach(
        grant.frameId,
        viewKey,
        (message) => {
          if (!disposed && docReadyRef.current) {
            endpoint()?.contentWindow?.postMessage(
              { ...message, token: frameRecord.token },
              frameRecord.origin,
            );
          }
        },
        visibleRef.current,
        false,
        undefined,
        { badge: pane?.badge, owner: pane?.panelId },
      );
      hubFrameRef.current = subscription;
      return subscription;
    });
    void hubFrameReadyRef.current.catch(() => {});
    return () => {
      disposed = true;
      hubFrameRef.current?.dispose();
      hubFrameRef.current = null;
      hubFrameReadyRef.current = null;
    };
  }, [live, frameRecord, endpoint, pane?.badge, pane?.panelId]);
  useEffect(() => {
    hubFrameRef.current?.visible(docVisible && !retiring);
  }, [docVisible, retiring]);
  // A follows-context slot (the dock) keeps its frame across team switches and re-binds it:
  // the shim holds its calls, Rust re-mints the grant for the new team and revokes the old
  // token, the shim replays with the new one, and the hub moves the frame's subscriptions.
  const [shimReadyCount, setShimReadyCount] = useState(0);
  // The document whose page has applied its theme: until then the frame is transparent over the
  // themed surface behind it, so neither an unpainted (white) document nor an unthemed one shows.
  const [readyUrl, setReadyUrl] = useState<string | null>(null);
  // Reveals a frame whose theme ack never arrives (a hidden frame runs no animation frames).
  const revealTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(revealTimer.current), []);
  const rebindAckRef = useRef<(() => void) | null>(null);
  const rebindChain = useRef<Promise<void>>(Promise.resolve());
  const followsContext = SLOT_REGISTRY[slotId as SlotId]?.teamBinding === "follows-context";
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  useEffect(() => {
    if (
      !followsContext ||
      keepTeam ||
      !live ||
      hostedFrame ||
      !frameRecord ||
      !activeOrg ||
      !activeTeam
    )
      return;
    if (!shimReadyCount) return;
    let disposed = false;
    // Every context change queues one task; inside it the current team is read again and the
    // frame re-binds until it matches, so the last switch wins (A → B → A ends on A).
    rebindChain.current = rebindChain.current.then(async () => {
      for (;;) {
        const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
        const bound = frameRecord.grant.team;
        if (disposed || !org || !team || (bound?.org === org && bound?.team === team)) return;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, 500);
          rebindAckRef.current = () => {
            clearTimeout(timer);
            resolve();
          };
          postToFrame(endpoint(), { type: "codesense:rebind-pending", token: frameRecord.token });
        });
        rebindAckRef.current = null;
        let grantRebound = false;
        try {
          const record = await rebindTeam({ org, team });
          grantRebound = true;
          const viewKey = await paneViewKey(record);
          const hubFrame = hubFrameRef.current;
          if (!hubFrame) throw new Error("Frame hub unavailable during rebind");
          hubFrame.rekey(viewKey, () => {
            onFrameViewKeyRef.current?.(viewKey);
            postToFrame(endpoint(), { type: "codesense:rebind", token: record.token });
          });
        } catch (error) {
          // Only a failed grant rebind leaves the old token valid. Once it changes, held
          // calls must stay held unless the attached host key has advanced too.
          if (!grantRebound)
            postToFrame(endpoint(), { type: "codesense:rebind", token: frameRecord.token });
          onDocProblemRef.current?.({
            kind: "load",
            message: `rebind_frame_team: ${String(error)}`,
          });
          return;
        }
      }
    });
    return () => {
      disposed = true;
    };
  }, [
    followsContext,
    live,
    hostedFrame,
    frameRecord,
    activeOrg,
    activeTeam,
    keepTeam,
    shimReadyCount,
    rebindTeam,
    postToFrame,
    endpoint,
  ]);
  // The team's selection is resolved once per settled change and rides in slot context.
  const navDoc = useMemo(
    () => ({
      docPath: queryAnchor ?? filePath ?? "",
      pane: isPane,
      message:
        messageOrg !== undefined && messageTeam !== undefined
          ? { org: messageOrg, team: messageTeam }
          : undefined,
    }),
    [queryAnchor, filePath, isPane, messageOrg, messageTeam],
  );
  const [navState, setNavState] = useState<{
    key: string | null;
    value: NavSelection | null;
  } | null>(null);
  const selection = navState?.key === viewerUrl ? navState?.value : undefined;
  useNavSelectionPush({
    live,
    docKey: viewerUrl,
    doc: navDoc,
    visible: docVisible,
    onSelection: (value) => setNavState({ key: viewerUrl, value }),
  });
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  const [binding, setBinding] = useState<{
    frameId?: string;
    token?: string;
    team: { org: string; team: string } | null;
  }>({ team: null });
  if (binding.frameId !== frameRecord?.frameId || binding.token !== frameRecord?.token)
    setBinding({
      frameId: frameRecord?.frameId,
      token: frameRecord?.token,
      team: frameRecord?.grant.team ?? (org && team ? { org, team } : null),
    });
  const hostTeam = hostTeamKey ? parseTeamKey(hostTeamKey) : null;
  const contextTeam = hostTeam?.teamName
    ? { org: hostTeam.orgSlug, team: hostTeam.teamName }
    : binding.team;
  const teamKey =
    hostTeamKey ?? (contextTeam ? buildTeamKey(contextTeam.org, contextTeam.team) : "_none");
  const focusedSession = useViewerStore((s) => selectFocusedSession(s, teamKey));
  // The one owner of this frame, for lifecycle, visibility and commands: the team whose view mounts
  // it (the owning scope), never the grant's team (an inherited pane's grant names its source, a
  // rebinding dock's the team it is leaving). Only a frame kept for another org's dock stays bound
  // to its own team. Read live by the lifecycle registration, so a re-bound dock keeps it.
  const owningScope = useOwningScope();
  const frameOwner = (keepTeam ? null : owningScope) ?? teamKey;
  const frameOwnerRef = useRef(frameOwner);
  useLayoutEffect(() => {
    frameOwnerRef.current = frameOwner;
  });
  const currentFile = selection?.file?.path;
  const slotContext = {
    slotId,
    presentation,
    width,
    narrow: width < (SLOT_REGISTRY[slotId as SlotId]?.narrowBelowPx ?? 300),
    team: contextTeam,
    focusedSession,
    currentFile,
    selection,
    theme,
  };
  // What the shim's has() answers from rides beside the context, not in it.
  const grant = frameRecord?.effectiveGrant;
  usePaneFileDrop({
    frame,
    origin: frameOrigin,
    enabled:
      !!pane &&
      !message &&
      !hostedFrame &&
      live &&
      docVisible &&
      ["right-pane", "dock", "file-nav"].includes(slotId) &&
      !!grant?.includes("files:read"),
    readyRef: docReadyRef,
  });
  const sdk = useMemo(() => pageSdk({ effectiveGrant: grant }), [grant]);
  // Architecture: the shim uses the host-resolved declaration, including manifest-only panes.
  const slotMessage = {
    type: "codesense:slot-context",
    context: slotContext,
    sdk,
    declaration: { badge: pane?.badge },
    token: frameRecord?.token,
  };
  const slotMessageKey = JSON.stringify(slotMessage);
  const slotMessageRef = useRef(slotMessage);
  useLayoutEffect(() => {
    slotMessageRef.current = slotMessage;
  });
  useEffect(() => {
    if (live && docReadyRef.current) postToFrame(endpoint(), slotMessageRef.current);
  }, [live, slotMessageKey, endpoint, postToFrame]);
  const focusTerminal = () => focusActiveWorkspaceTerminal(teamKey);
  const focusTerminalRef = useRef(focusTerminal);
  useLayoutEffect(() => {
    focusTerminalRef.current = focusTerminal;
  });
  useLayoutEffect(() => {
    if (!frame) return;
    const hostFocus = (event: FocusEvent) => {
      if (event.target instanceof HTMLElement) recordFocusOwner(event.target);
    };
    if (document.activeElement instanceof HTMLElement) recordFocusOwner(document.activeElement);
    document.addEventListener("focusin", hostFocus);
    return () => {
      document.removeEventListener("focusin", hostFocus);
    };
  }, [frame]);
  const dashboardActions: HostMenuRequest["dashboard"] = parentDashboard ?? {
    centre: slotId === "centre" || slotId === "message",
    reload: onReloadPane ?? (() => remintFrame()),
    openAsTab:
      pane && filePath
        ? () => {
            if (onNavigate) onNavigate(filePath, { openInNewTab: true });
            else useViewerStore.getState().openFile(teamKey, teamContentPath, filePath);
          }
        : undefined,
    editSource:
      !message && filePath && paneRef?.scope !== "built-in"
        ? () => {
            if (onNavigate) onNavigate(filePath, { openInNewTab: true });
            else useViewerStore.getState().openFile(teamKey, teamContentPath, filePath);
            useViewerStore.getState().requestEditActiveFile(teamKey);
          }
        : undefined,
  };
  useEffect(
    () => () => {
      menuLifetime.current.abort();
      menuLifetime.current = new AbortController();
    },
    [viewerUrl],
  );
  // Architecture: hosted message actions use the inbox parent's existing action gate and callback.
  const menuPage: NonNullable<HostMenuRequest["page"]> = {
    enabled: () => menuActionsEnabled.current,
    notify: (itemId, row, waitForPage) => {
      const message = { type: "codesense:menu-action", itemId, row };
      const target = endpoint();
      if (waitForPage && target)
        return deliverMenuAction(target, message, menuLifetime.current.signal);
      postToFrame(target, message);
    },
    run: (command, args, options) =>
      runGatedAction(
        command,
        args,
        { origin: frameRecord?.origin ?? "", token: frameRecord?.token },
        options,
      ),
  };
  useLayoutEffect(() => {
    presentationRef.current = async (verb, args) => {
      if (verb === "focusTerminal") {
        focusTerminalRef.current();
        return { focused: true };
      }
      if (verb === "focusOut") {
        const previous = focusElement && focusOwner.previous.get(focusElement);
        if (previous?.isConnected && previous !== document.body) previous.focus();
        else focusTerminalRef.current();
        return { focused: true };
      }
      const currentFrame = hostedFrame ? parentElement : iframeRef.current;
      if (!currentFrame) throw new Error(FRAME_PRESENTATION_UNAVAILABLE);
      const values = (args ?? {}) as Partial<HostMenuRequest> & {
        defaultMenu?: boolean;
        textId?: number;
      };
      const lifetime = menuLifetime.current.signal;
      let anchorRect = values.anchorRect ?? { x: 0, y: 0, width: 0, height: 0 };
      if (hostedFrame) {
        const child = await hostedMenuRect(currentFrame, hostedFrame.origin, lifetime);
        if (!child || lifetime.aborted) return { closed: true };
        anchorRect = { ...anchorRect, x: child.x + anchorRect.x, y: child.y + anchorRect.y };
      }
      const post = (message: Record<string, unknown>) =>
        endpoint()?.contentWindow?.postMessage(message, live ? (frameRecord?.origin ?? "*") : "*");
      const result = await showHostMenu({
        ...values,
        type:
          values.type === "text" ||
          values.type === "native" ||
          values.type === "frame" ||
          values.type === "inbox"
            ? values.type
            : verb === "showPopover"
              ? "popover"
              : values.defaultMenu
                ? "default"
                : "menu",
        frame: currentFrame,
        origin: live ? (frameRecord?.origin ?? "*") : "*",
        signal: lifetime,
        teamKey,
        team: contextTeam ?? undefined,
        page: parentMenuPage ?? menuPage,
        contentRoot: pane?.contentRoot ?? teamContentPath,
        dashboard: live ? dashboardActions : undefined,
        text: values.text,
        editText: async (action) => {
          const text = action === "paste" ? await navigator.clipboard.readText() : undefined;
          if (!lifetime.aborted)
            post({ type: "codesense:text-edit", action, text, textId: values.textId });
        },
        returnFocus: () => {
          if (!currentFrame.isConnected || lifetime.aborted) return;
          focusOwner.returning.add(currentFrame);
          currentFrame.focus();
          if (hostedFrame)
            postToFrame(currentFrame, {
              type: "codesense:child-focus",
              origin: hostedFrame.origin,
            });
          post({ type: "codesense:focus-return", anchorId: values.anchorId });
        },
        anchorRect,
        run: (command, args, options) =>
          runGatedAction(
            command,
            args,
            { origin: frameRecord?.origin ?? "", token: frameRecord?.token },
            options,
          ),
      });
      if (values.type !== "inbox" && "itemId" in result && !result.builtin)
        (parentMenuPage ?? menuPage).notify(
          result.itemId,
          (result.row ?? {}) as Record<string, unknown>,
        );
      return result;
    };
  });
  const panelId = overlayPanelId;
  useLayoutEffect(() => {
    if (!frameRecord) {
      retiringRef.current = false;
      queueMicrotask(() => setRetiring(false));
      return;
    }
    if (!live || (!hostedFrame && (!frame || frame !== iframeRef.current))) return;
    retiringRef.current = false;
    queueMicrotask(() => setRetiring(false));
    return registerFrame({
      frameId: frameRecord.frameId,
      parentFrame: frameRecord.parentFrame ?? undefined,
      dashboard: frameRecord.grant.dashboard,
      documentPath: filePath ?? undefined,
      panelId,
      slotId,
      get teamKey() {
        return frameOwnerRef.current;
      },
      element: hostedFrame ? undefined : (frame ?? undefined),
      post: (message) =>
        postToFrame(hostedFrame ? endpoint() : frame, { ...message, token: frameRecord.token }),
      beforeReload: () => reloadCapture.capture(),
      scrollY: () => scrollYRef.current,
      freeze: (frozen) => postToFrame(endpoint(), { type: "codesense:freeze", frozen }),
      remint: hostedFrame ? undefined : remintFrame,
      hide: () => {
        retiringRef.current = true;
        setRetiring(true);
        if (!hostedFrame && frame) {
          frame.inert = true;
          frame.style.visibility = "hidden";
        }
      },
    });
  }, [
    live,
    frameRecord,
    frame,
    filePath,
    panelId,
    slotId,
    remintFrame,
    reloadCapture,
    hostedFrame,
    endpoint,
    postToFrame,
  ]);
  const missedPongs = useRef(0);
  useEffect(() => {
    missedPongs.current = 0;
    if (!live || !docVisible || retiring || !frameRecord) return;
    const timer = setInterval(() => {
      if (!docReadyRef.current || retiringRef.current) return;
      if (missedPongs.current >= 3) {
        clearInterval(timer);
        onDocProblemRef.current?.({ kind: "load", message: "frame stopped responding" });
        return;
      }
      missedPongs.current += 1;
      postToFrame(endpoint(), { type: "codesense:ping" });
    }, 5000);
    return () => clearInterval(timer);
  }, [live, docVisible, retiring, frameRecord, endpoint, postToFrame]);
  useEffect(() => {
    if (!hostedFrame && !docVisible && frame && document.activeElement === frame) {
      const previous = focusOwner.previous.get(frame);
      if (previous?.isConnected && previous !== document.body)
        previous.focus({ preventScroll: true });
    }
  }, [docVisible, frame, hostedFrame]);
  useLayoutEffect(() => {
    if (docReadyRef.current)
      postToFrame(endpoint(), {
        type: "codesense:presentation",
        shown: presented,
        focusAllowed: allowAutoFocus,
      });
  }, [presented, allowAutoFocus, endpoint, postToFrame]);
  const intentRef = useRef(intent);
  const deliveredRef = useRef(onIntentDelivered);
  useEffect(() => {
    intentRef.current = intent;
    deliveredRef.current = onIntentDelivered;
  }, [intent, onIntentDelivered]);
  const deliverIntent = useCallback(() => {
    if (
      (!presented && !prepareIntent) ||
      !docReadyRef.current ||
      !intentRef.current ||
      !endpoint()?.contentWindow
    )
      return;
    if (presented && (intentRef.current.focusList || intentRef.current.focusComposer)) {
      focusElement?.focus({ preventScroll: true });
    }
    postToFrame(endpoint(), { type: "codesense:intent", intent: intentRef.current });
    intentRef.current = undefined;
    deliveredRef.current?.();
  }, [endpoint, focusElement, postToFrame, presented, prepareIntent]);
  useLayoutEffect(() => {
    docReadyRef.current = false;
    loadedDocumentKey.current = null;
    menuActionsEnabled.current = false;
    menuLifetime.current.abort();
    menuLifetime.current = new AbortController();
  }, [documentKey]);
  useEffect(() => {
    deliverIntent();
  }, [intent, viewerUrl, deliverIntent, presented]);
  useEffect(() => {
    themeRef.current = theme;
    if (theme && docVisible && docReadyRef.current) {
      postToFrame(endpoint(), { type: "codesense:theme", theme, applyScheme: embedded });
    }
  }, [theme, embedded, endpoint, docVisible, postToFrame]);
  const requestReadyAgain = useCallback(() => {
    const target = endpoint();
    const loaded = hostedFrame
      ? !!hostedWindow.current
      : loadedDocumentKey.current === currentDocumentKey.current;
    if (
      !visibleRef.current ||
      docReadyRef.current ||
      !loaded ||
      !live ||
      !frameRecord ||
      !target?.contentWindow
    )
      return;
    bridgeTiming(() => `hello doc=${currentDocumentKey.current} t=${nowMs()}`);
    target.contentWindow.postMessage({ type: "codesense:hello" }, frameRecord.origin);
  }, [endpoint, hostedFrame, live, frameRecord]);
  useEffect(() => {
    visibleRef.current = docVisible;
    bridgeTiming(
      () =>
        `visibility doc=${currentDocumentKey.current} t=${nowMs()} visible=${docVisible} ${docReadyRef.current ? "posted" : "queued-until-ready"}`,
    );
    if (docReadyRef.current) {
      postToFrame(endpoint(), { type: "codesense:visibility", visible: docVisible });
    }
    if (!docVisible) return;
    // Shown: a page whose `ready` never reached us is asked to say it again, and every read
    // held while hidden runs now. A second `ready` for a handshake already done is ignored.
    requestReadyAgain();
    const held = heldReads.current.splice(0);
    for (const entry of held) {
      if (entry.documentKey !== currentDocumentKey.current) continue;
      const result = entry.invoke();
      for (const answer of entry.answers) answer(result);
    }
  }, [docVisible, endpoint, postToFrame, requestReadyAgain]);
  const onBadgeRef = useRef(onBadge);
  const onContentSizeRef = useRef(onContentSize);
  useEffect(() => {
    onBadgeRef.current = onBadge;
    onContentSizeRef.current = onContentSize;
  }, [onBadge, onContentSize]);

  // Bind before a newly committed frame can run its shim and post its first call.
  useLayoutEffect(() => {
    if (!filePath) return;

    const handler = (event: MessageEvent) => {
      const data = event.data ?? {};
      if (hostedFrame && !hostedWindow.current) {
        if (
          event.origin !== hostedFrame.origin ||
          data.token !== hostedFrame.token ||
          !event.source
        )
          return;
        hostedWindow.current = event.source as Window;
      }
      const type = data.type;
      if (event.source !== endpoint()?.contentWindow) return;
      if (type === "codesense:ready" || type === "codesense:invoke") noteHostMessage(type);
      if (
        live &&
        type !== "codesense:invoke" &&
        (!frameRecord || event.origin !== frameRecord.origin || data.token !== frameRecord.token)
      ) {
        if (type === "codesense:ready")
          bridgeTiming(
            () =>
              `drop ready doc=${documentKey} t=${nowMs()} reason=${!frameRecord ? "no-frame-record" : "origin-or-token"}`,
          );
        return;
      }
      if (data.jsonrpc === "2.0") {
        if (!live || !frameRecord) return;
        const source = event.source as Window | null;
        const identity = { origin: event.origin, token: data.token };
        void mcpAppsReply(data, (verb, args) => runGatedAction(verb, args, identity)).then(
          (reply) => {
            if (reply) source?.postMessage(reply, frameRecord.origin);
          },
        );
        return;
      }

      // SW-FX-5: scroll reports carry `y`, not `href` — dispatch this branch
      // BEFORE the `href` guard below, which would otherwise drop them.
      // `Number.isFinite` rejects `NaN`/`Infinity` (both `typeof "number"`).
      if (type === "codesense:route-changed") {
        if (typeof data.hash === "string" && (!data.hash || data.hash.startsWith("#")))
          rememberRoute(data.hash);
        return;
      }
      if (type === "codesense:scroll") {
        if (Number.isFinite(data.y)) scrollYRef.current = data.y;
        return;
      }

      // SW-LD-1: ⌘W forwarded from the focused iframe (which would otherwise
      // swallow the keydown so the app's global Cmd+W never fires). Route to the
      // same close primitive as every other affordance — parity with markdown/
      // image tabs, which close on ⌘W because they render in-app.
      if (type === "codesense:close-tab") {
        // A pane or a message is not a tab: ⌘W inside it must not close the centre tab.
        if (!embedded) closeActiveFileTab(filePath);
        return;
      }

      if (type === "codesense:before-reload-result") {
        reloadCapture.receive(data.id, data.state, data.warning);
        return;
      }
      if (type === "codesense:live") {
        if (live) docLive();
        return;
      }
      // The page has applied the theme the host posted: now the frame may show.
      if (type === "codesense:theme-applied") {
        if (!live) return;
        clearTimeout(revealTimer.current);
        setReadyUrl(viewerUrl);
        return;
      }
      // The shim is running: tell it its theme and whether it is on screen.
      if (type === "codesense:ready") {
        if (!live) return;
        // The answer to our `hello`; the handshake it asks for is already done.
        if (data.retry === true && docReadyRef.current) return;
        bridgeTiming(
          () =>
            `ready doc=${documentKey} t=${nowMs()} sinceDocStart=${nowMs() - docStartedAt.current} again=${docReadyRef.current} visible=${visibleRef.current}${lagField(data.sentAt)}`,
        );
        if (docReadyRef.current) {
          outbox.begin(`${documentKey}:load:${++documentLoads.current}`);
          onPageStartRef.current?.(`${documentKey}:load:${documentLoads.current}`);
          menuLifetime.current.abort();
          menuLifetime.current = new AbortController();
        }
        docReadyRef.current = true;
        outbox.ready(endpoint());
        setShimReadyCount((n) => n + 1);
        // Every load, a reload included, stays transparent until its theme ack or the fallback.
        clearTimeout(revealTimer.current);
        setReadyUrl(null);
        revealTimer.current = setTimeout(() => setReadyUrl(viewerUrl), THEME_ACK_FALLBACK_MS);
        postToFrame(endpoint(), slotMessageRef.current);
        onDocLoadedRef.current?.();
        const source = event.source as Window | null;
        source?.postMessage(
          { type: "codesense:theme", theme: themeRef.current, applyScheme: embedded },
          frameRecord?.origin ?? "*",
        );
        source?.postMessage(
          { type: "codesense:visibility", visible: visibleRef.current },
          frameRecord?.origin ?? "*",
        );
        deliverIntent();
        // Pages see slot context, theme, visibility and intent first; a focus held for presentation
        // is applied when it arrives, and reloadState() is a promise the page awaits.
        postToFrame(endpoint(), {
          type: "codesense:presentation",
          shown: presented,
          focusAllowed: allowAutoFocus,
        });
        const currentDocument = documentKey;
        const load = documentLoads.current;
        const handoff = load === 0 ? reloadStateRef.current : null;
        const sendState = (state: unknown) =>
          postToFrame(endpoint(), { type: "codesense:reload-state", state });
        if (!handoff) sendState(null);
        else
          void handoff.then((state) => {
            if (
              iframeRef.current?.src === viewerUrl &&
              currentDocument === currentDocumentKey.current &&
              load === documentLoads.current
            )
              sendState(state);
          });
        if (frameRecord) markFrameReady(frameRecord.frameId);
        if (presented && !hostedFrame && frame && document.activeElement === frame) {
          recordFocusOwner(frame);
          postToFrame(endpoint(), { type: "codesense:focus-in" });
        }
        return;
      }

      if (type === "codesense:focus-suppressed") {
        onCandidateFocus?.();
        return;
      }
      if (type === "codesense:frame-focus") {
        // Focus inside another document can bypass the embedding element's inertness.
        if (!presented) {
          onCandidateFocus?.();
          return;
        }
        if (focusElement) {
          recordFocusOwner(focusElement);
          if (focusOwner.returning.delete(focusElement)) return;
        }
        if (docReadyRef.current && !retiringRef.current)
          postToFrame(endpoint(), { type: "codesense:focus-in" });
        return;
      }
      if (type === "codesense:menu-actions") {
        menuActionsEnabled.current = data.enabled === true;
        return;
      }
      if (type === "codesense:contextmenu") {
        const target = data.target ?? (data.row ? "row" : "frame");
        if (!live && target !== "text") return;
        const present = (menuType: HostMenuRequest["type"]) =>
          presentationRef.current?.("showMenu", { ...data, type: menuType });
        if (target === "text") {
          const lifetime = menuLifetime.current.signal;
          void present("text")
            .finally(() => {
              if (!lifetime.aborted)
                endpoint()?.contentWindow?.postMessage(
                  { type: "codesense:text-menu-closed", textId: data.textId },
                  live ? (frameRecord?.origin ?? "*") : "*",
                );
            })
            .catch(console.error);
        } else if (data.row?.kind === "agent")
          void runGatedAction(
            "showMenu",
            { ...data, defaultMenu: true },
            { origin: event.origin, token: data.token },
          )
            .catch(() => present("frame"))
            .catch(console.error);
        else if (
          target === "row" &&
          ["story", "epic", "file", "workspace", "group"].includes(data.row?.kind)
        )
          void present("native").catch(console.error);
        else if (
          target === "row" &&
          ["conversation", "message", "attachment", "role-queue"].includes(data.row?.kind)
        )
          void present("inbox").catch(console.error);
        else if (frameRecord?.message) {
          void presentationRef
            .current?.("showMenu", {
              ...data,
              type: "inbox",
              row: {
                kind: "message",
                ...frameRecord.message,
                team: { org: frameRecord.message.org, team: frameRecord.message.team },
              },
            })
            .catch(console.error);
        } else void present("frame").catch(console.error);
        return;
      }
      if (type === "codesense:rebind-ack") {
        rebindAckRef.current?.();
        return;
      }
      if (type === "codesense:flush-ack") {
        if (frameRecord) acknowledgeFlush(frameRecord.frameId, data.id);
        return;
      }
      if (type === "codesense:pong") {
        missedPongs.current = 0;
        return;
      }
      if (type === "codesense:key-escape") {
        if (dispatchFrameKey({ key: "Escape", code: "Escape" })) return;
        const overlay = focusElement?.closest('[data-fullscreen="true"]');
        if (overlay)
          overlay.dispatchEvent(new CustomEvent("codesense:frame-escape", { bubbles: true }));
        else if (
          slotId === "right-pane" ||
          slotId === "dock" ||
          slotId === "file-nav" ||
          parentPanelId
        )
          focusTerminalRef.current();
        return;
      }

      // A message doc reports its laid-out height so its host can size the frame.
      if (type === "codesense:content-size") {
        if (live && Number.isFinite(data.height) && data.height >= 0) {
          onContentSizeRef.current?.(data.height);
        }
        return;
      }

      if (type === "codesense:badge") {
        if (!live) return;
        const badge = parseDocBadge(data.badge);
        if (badge !== undefined) onBadgeRef.current?.(badge);
        return;
      }

      // An uncaught error, failed asset or unhandled rejection the shim forwarded.
      if (type === "codesense:doc-error") {
        if (typeof data.message === "string") {
          onDocProblemRef.current?.({
            kind: "script",
            message: data.source
              ? `${data.message} (${data.source}:${data.line ?? 0})`
              : data.message,
          });
        }
        return;
      }

      // Frame keys use the whole registry and current user bindings inside the source guard.
      if (type === "codesense:shortcut") {
        if (
          typeof data.key !== "string" ||
          typeof data.code !== "string" ||
          [data.metaKey, data.ctrlKey, data.shiftKey, data.altKey].some(
            (v) => typeof v !== "boolean",
          )
        )
          return;
        const key = {
          key: data.key,
          code: data.code,
          metaKey: data.metaKey as boolean,
          ctrlKey: data.ctrlKey as boolean,
          shiftKey: data.shiftKey as boolean,
          altKey: data.altKey as boolean,
        };
        dispatchFrameKey(key, data.editorTarget === true, data.textFieldTarget === true);
        return;
      }

      // SW-LD-1: scoped codesense bridge (postMessage RPC). Handled BEFORE the
      // `href` guard (invoke messages carry no href). Every live doc is serviced
      // for any allowlisted command (runBridgeCommand enforces the allowlist); a
      // baseline render has no bridge.
      if (type === "codesense:invoke") {
        const id = data.id;
        // The shim always sends a numeric correlation id; a non-number means a
        // malformed/hand-forged message that can't be correlated to a reply —
        // drop it (mirrors the scroll handler's `typeof d.y` discipline).
        if (typeof id !== "number") return;
        // The iframe is served from the local HTTP server; without a resolved
        // port there is no valid doc frame, so drop (no reply target).
        if (typeof port !== "number") {
          bridgeTiming(
            () =>
              `drop invoke doc=${documentKey} id=${id} cmd=${safeCmd(data.command)} reason=no-port`,
          );
          return;
        }
        // Origin-pinned reply — see `replyToDoc` for the full SW-LD-1 r3 rule.
        const source = event.source as Window | null;
        const reply = (payload: { ok: boolean; data?: unknown; error?: string }) =>
          replyToDoc(source, "codesense:invoke-result", id, frameOrigin, live, payload);
        if (!live) {
          reply({ ok: false, error: "unavailable: baseline view has no bridge" });
          return;
        }
        if (!frameRecord) {
          reply({ ok: false, error: "denied: frame identity" });
          return;
        }
        // A forged `{type:"codesense:invoke", command:"rm_rf"}` hits this SAME
        // path and is refused by the allowlist like any other unknown command.
        // Receipt and dispatch are the same tick: the gate is called synchronously below.
        const receivedAt = nowMs();
        bridgeTiming(
          () =>
            `invoke-recv doc=${documentKey} id=${id} cmd=${safeCmd(data.command)} t=${receivedAt} sinceDocStart=${receivedAt - docStartedAt.current} ready=${docReadyRef.current} visible=${visibleRef.current}${lagField(data.sentAt)}`,
        );
        const settled = (outcome: string) =>
          bridgeTiming(
            () =>
              `invoke-settle doc=${documentKey} id=${id} cmd=${safeCmd(data.command)} t=${nowMs()} took=${nowMs() - receivedAt} outcome=${outcome} stale=${currentDocumentKey.current !== documentKey}`,
          );
        const invoke = () =>
          runGatedAction(
            data.command,
            data.args,
            { origin: event.origin, token: data.token },
            { isCurrent: () => currentDocumentKey.current === documentKey },
          );
        const answer = (call: Promise<unknown>) =>
          call
            .then((result) => {
              settled("ok");
              reply({ ok: true, data: result });
            })
            .catch((err: unknown) => {
              settled("error");
              const error = err instanceof Error ? err.message : String(err);
              reply({ ok: false, error });
              onDocProblemRef.current?.({ kind: "bridge", message: `${data.command}: ${error}` });
            });
        if (!embedded && !visibleRef.current && isHeldRead(data.command)) {
          // Identical reads share one held call; distinct ones are capped so a polling page
          // cannot queue an unbounded burst for the moment the tab is shown.
          const key = heldReadKey(data.command, data.args);
          const queue = heldReads.current;
          const twin =
            key === null
              ? undefined
              : queue.find((entry) => entry.documentKey === documentKey && entry.key === key);
          if (twin) twin.answers.push(answer);
          else if (queue.length >= HELD_READ_LIMIT) reply({ ok: false, error: HELD_READ_BUSY });
          else queue.push({ documentKey, key, invoke, answers: [answer] });
        } else answer(invoke());
        return;
      }

      // SW-SD-1: the doc's answer to a chrome-button `snapshot-request`. Correlate
      // to the pending waiter (numeric id, `event.source`-guarded above) and
      // settle it; the `.then` chain in `handleSnapshotClick` archives + toasts.
      if (type === "codesense:snapshot-result") {
        const id = data.id;
        if (typeof id !== "number") return;
        if (!live || typeof port !== "number" || event.origin !== frameOrigin) return;
        const waiter = snapshotWaitersRef.current.get(id);
        if (!waiter) return;
        clearTimeout(waiter.timer);
        snapshotWaitersRef.current.delete(id);
        const content = waiter.format ? data.content : data.html;
        if (!data.ok) {
          waiter.reject(new Error(typeof data.error === "string" ? data.error : "snapshot failed"));
        } else if (waiter.format && !iframeRef.current?.getClientRects().length) {
          waiter.reject(
            new Error("Document is hidden; select its tab or show its pane and retry."),
          );
        } else if (
          waiter.format &&
          (typeof data.sourceUrl !== "string" ||
            data.sourceUrl.split("#")[0] !== waiter.sourceUrl.split("#")[0])
        ) {
          waiter.reject(
            new Error("Document navigated or is not ready; select the current document and retry."),
          );
        } else if (data.ok && typeof content === "string")
          waiter.resolve({
            content,
            capturedAt:
              typeof data.capturedAt === "string" ? data.capturedAt : new Date().toISOString(),
            truncated: data.truncated === true,
          });
        else
          waiter.reject(new Error(typeof data.error === "string" ? data.error : "snapshot failed"));
        return;
      }

      // SW-SD-1: doc-initiated `window.codesense.snapshot(state)` — the doc
      // serialized itself and asks the parent to archive. Id/origin discipline
      // mirrors `codesense:invoke` exactly. The parent MUST reply so the doc's
      // Promise settles: a baseline frame's denial goes to "*" because an
      // opaque-origin frame can never match a pinned origin (the LD-1 r3 hang
      // lesson); a live doc's ack is origin-pinned.
      if (type === "codesense:snapshot") {
        const id = data.id;
        if (typeof id !== "number") return;
        const source = event.source as Window | null;
        // Reply defensively even before the port resolves (review r1): returning
        // silently would strand the doc's `snapshot()` Promise forever (there is
        // no doc-side timeout on the verb, unlike the button path). An error
        // reply at "*" is safe — it carries no data and the frame may be
        // opaque-origin. In practice a live doc only runs once the port is
        // resolved, so this is defense-in-depth.
        if (typeof port !== "number") {
          source?.postMessage(
            { type: "codesense:snapshot-result", id, ok: false, error: "viewer not ready" },
            "*",
          );
          return;
        }
        // Origin-pinned reply — same SW-LD-1 r3 rule as invoke (see `replyToDoc`).
        const reply = (payload: { ok: boolean; error?: string }) =>
          replyToDoc(source, "codesense:snapshot-result", id, frameOrigin, live, payload);
        if (!live) {
          reply({ ok: false, error: "unavailable: baseline view has no bridge" });
          return;
        }
        // A pane or a message is not content: archiving it would put a file in
        // the team's content tree.
        if (embedded) {
          reply({
            ok: false,
            error: `unavailable: a ${isPane ? "pane" : "message"} has no snapshot archive`,
          });
          return;
        }
        if (typeof data.html !== "string") {
          reply({ ok: false, error: "no snapshot html" });
          return;
        }
        // Two-arg `.then` (review r1): a throw in the success handler (e.g. the
        // toast) must NOT route to the rejection handler and send a contradictory
        // second `{ok:false}` reply for an id the doc already resolved.
        archiveSnapshot(data.html, typeof data.name === "string" ? data.name : undefined)
          .then(
            () => {
              reply({ ok: true });
              toast.success("Snapshot saved to context.");
            },
            (err: unknown) =>
              reply({ ok: false, error: err instanceof Error ? err.message : String(err) }),
          )
          // Terminal catch (review r2): a throw inside either handler above (e.g.
          // the toast) must not surface as an unhandled rejection — the doc was
          // already replied to. Mirrors the invoke branch's `.catch`.
          .catch(() => {});
        return;
      }

      const href = data.href;
      if (typeof href !== "string") return;

      if (type === "codesense:navigate") {
        if (!onNavigate) return;
        // PTS-2: pass teamContentPath as the fallback root for absolute hrefs.
        // A pane lives beside content/, not in it, and sees content the way a
        // dashboard at its root does, so its links resolve from there.
        const linkOwner = embedded ? `${teamContentPath}/index.html` : filePath;
        const resolved = resolveViewerLinkPath(href, linkOwner, teamContentPath);
        if (!resolved) return;
        const fragment = href.split("#")[1];
        const openInNewTab = !!(event.data.metaKey || event.data.ctrlKey);
        const navigate = (path: string) =>
          onNavigate(path, {
            ...(openInNewTab ? { openInNewTab: true } : {}),
            ...(fragment ? { hash: `#${fragment}` } : {}),
          });
        // A full filesystem path outside this doc's own root (a row's `path` in
        // another team) opens in that team when it is inside a known team content
        // root; anything else resolves against this doc's root as before.
        const hrefPath = href.split("#")[0];
        if (live && hrefPath.startsWith("/") && resolved !== hrefPath) {
          bridgeResolveOpenTarget(queryAnchor ?? filePath, hrefPath)
            .then((target) => {
              const org = useOrgStore.getState();
              if (org.activeOrgSlug === target.org && org.activeTeamName === target.team) {
                navigate(target.path);
              } else {
                openFileInTeam(target, {
                  newTab: openInNewTab,
                  switchTeam: (teamKey) => switchContext({ teamKey }, "nav"),
                });
              }
            })
            .catch(() => navigate(resolved));
          return;
        }
        navigate(resolved);
      } else if (type === "codesense:external-link") {
        // Validate scheme before passing to OS handler — only allow
        // http(s) and mailto to prevent file://, javascript://, etc.
        let url = href;
        if (url.startsWith("//")) url = `https:${url}`;
        if (/^https?:\/\/|^mailto:/i.test(url)) {
          openUrl(url).catch(() => {});
        }
      }
    };

    window.addEventListener("message", handler);
    return () => window.removeEventListener("message", handler);
    // PTS-2: `teamContentPath` participates in `resolveViewerLinkPath`'s
    // anchor fallback for absolute hrefs (post-F4).
    // `live` gates the bridge branch; `port` pins the bridge reply's target origin.
    // SW-SD-1: `archiveSnapshot` is the doc-initiated snapshot sink (stable per
    // filePath/teamContentPath).
    // `runGatedAction` is stable per filePath — re-subscribe when it changes so
    // the bridge dispatch always routes through the current doc's guards.
  }, [
    filePath,
    onNavigate,
    teamContentPath,
    live,
    port,
    archiveSnapshot,
    runGatedAction,
    isPane,
    slotId,
    embedded,
    queryAnchor,
    deliverIntent,
    docLive,
    frameRecord,
    hostedFrame,
    overlayPanelId,
    parentPanelId,
    focusElement,
    frame,
    endpoint,
    frameOrigin,
    documentKey,
    viewerUrl,
    outbox,
    reloadCapture,
    postToFrame,
    presented,
    allowAutoFocus,
    onCandidateFocus,
    rememberRoute,
  ]);

  // Assigning only the URL fragment navigates inside this cross-origin document;
  // it neither reads its DOM nor replaces its minted frame or bridge identity.
  // The src prop seeds a document once. Its live route belongs to that document;
  // reflecting route-changed into this prop can replay an older hash on a render.
  const initialFrameSrc = useMemo(() => {
    const hash = pendingHash ?? routeRef.current.hash;
    return viewerUrl
      ? `${viewerUrl.split("#")[0]}${hash ? (hash.startsWith("#") ? hash : `#${hash}`) : ""}`
      : undefined;
  }, [viewerUrl]);
  const hashLoaded = useRef(false);
  const appliedHash = useRef<{
    node: HTMLIFrameElement;
    url: string;
    hash: string;
    consume?: () => void;
  } | null>(null);
  const hashRef = useRef(pendingHash);
  hashRef.current = pendingHash;
  const consumeHash = useRef(onPendingHashConsumed);
  consumeHash.current = onPendingHashConsumed;
  const routeNonce = useRef(0);
  const applyPendingHash = useCallback(() => {
    // Saved routes seed src once. WebKit also fires load on fragment changes;
    // replaying the previous saved route here would bounce an internal navigation.
    const hash = hashRef.current;
    const node = iframeRef.current;
    if (!hash || !node || !viewerUrl) return;
    if (
      appliedHash.current?.node === node &&
      appliedHash.current.url === viewerUrl &&
      appliedHash.current.hash === hash &&
      appliedHash.current.consume === consumeHash.current
    )
      return;
    appliedHash.current = { node, url: viewerUrl, hash, consume: consumeHash.current };
    const target = `${viewerUrl.split("#")[0]}${hash.startsWith("#") ? hash : `#${hash}`}`;
    // history.replaceState changes the frame URL without changing its src attribute.
    // A repeated src assignment is then a WebKit no-op; Location.href's setter
    // is allowed across origins and navigates the actual document fragment.
    if (node.contentWindow) node.contentWindow.location.href = target;
    if (node.getAttribute("src") !== target) node.src = target;
    const fragment = hash.startsWith("#") ? hash : `#${hash}`;
    // Location.href emits no hashchange for an identical fragment. Deliver every
    // explicit open request so the existing document can reveal its anchor again.
    if (hashRef.current && live && frameRecord)
      postToFrame(endpoint(), {
        type: "codesense:navigate",
        hash: fragment,
        nonce: ++routeNonce.current,
        token: frameRecord.token,
      });
    rememberRoute(fragment);
    if (hashRef.current) consumeHash.current?.();
  }, [viewerUrl, rememberRoute, live, frameRecord, postToFrame, endpoint]);
  useEffect(() => {
    if (!pendingHash) appliedHash.current = null;
    else if (hashLoaded.current) applyPendingHash();
  }, [pendingHash, onPendingHashConsumed, applyPendingHash]);

  // SW-FX-5: after the iframe (re)loads, hand the saved scroll offset back
  // across the postMessage boundary. The injected script registers its
  // restore listener at parse time (script is prepended), so it is ready by
  // the time `load` fires. Buffered replacements inherit the outgoing offset.
  const handleIframeLoad = useCallback(() => {
    loadedDocumentKey.current = currentDocumentKey.current;
    // A new iframe inherits the host origin until navigation commits. Recover a
    // missed ready only after load, when the pinned frame origin can receive it.
    requestReadyAgain();
    if (viewerUrl) onPageLoadRef.current?.();
    hashLoaded.current = true;
    applyPendingHash();
    rejectCaptures();
    // One frame owner: explicit fragment/route wins, then saved reading position, then top.
    // The frame owns fragment positioning; host restoration must yield on every such load.
    if (hashRef.current || routeRef.current.hash) return;
    if (live) postToFrame(endpoint(), { type: "codesense:restore-scroll", y: scrollYRef.current });
    else
      endpoint()?.contentWindow?.postMessage(
        { type: "codesense:restore-scroll", y: scrollYRef.current },
        "*",
      );
  }, [rejectCaptures, viewerUrl, endpoint, postToFrame, live, requestReadyAgain, applyPendingHash]);

  // SW-FX-38: route through the shared `closeActiveFileTab` primitive — a
  // slug-less workspace (repoPath null) must STILL close its tab; `repoPath`
  // is best-effort for the reveal, never a precondition.
  const handleClose = useCallback(() => closeActiveFileTab(filePath), [filePath]);

  useEffect(
    () => () => {
      for (const child of childRecords.current)
        void releaseFrame(child.frameId).catch(console.error);
      childRecords.current = [];
      setChildren([]);
    },
    [viewerUrl],
  );

  if (hostedFrame) return null;
  if (!filePath) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Select a file to view
      </div>
    );
  }

  // Still resolving port.
  if (port === undefined) {
    return (
      <div
        ref={firstPaint.ref}
        style={embedded ? firstPaint.style : { ...firstPaint.style, colorScheme: undefined }}
        className="flex items-center justify-center h-full text-muted-foreground text-sm"
      >
        Loading...
      </div>
    );
  }

  if (port === null || error || frameError) {
    return (
      <div className="flex items-center justify-center h-full text-red-500 text-sm">
        {error ?? frameError ?? "HTTP API server is not available"}
      </div>
    );
  }

  if (fileDeleted) {
    if (renderMissing) return <>{renderMissing()}</>;
    const fileName = filePath.split("/").pop() ?? "File";
    return <FileDeletedPlaceholder fileName={fileName} onClose={handleClose} />;
  }

  // Transparent until the shim reports ready, so the unpainted document never shows. One rule
  // for sight and input: a frame the user cannot see (not presented, retiring, or still starting)
  // takes no Tab, Enter or click. `retiring` keeps a readiness update from revealing a retired
  // frame, whose `inert` and `visibility` the retire path sets directly.
  const awaitingReady = live && embedded && readyUrl !== viewerUrl;
  const inputBlocked = !presented || retiring || awaitingReady;

  return (
    <div
      ref={firstPaint.ref}
      style={embedded ? firstPaint.style : { ...firstPaint.style, colorScheme: undefined }}
      className="flex h-full w-full flex-col"
    >
      {/* SW-SD-1: universal Snapshot affordance — freeze the current render into
          the git-versioned context repo. Hidden on a baseline render: its
          opaque-origin frame can't be reached by the request. SW-FX-62: docked in
          a normal-flow toolbar band ABOVE the iframe (right-aligned) instead of
          floating over the content — so no control overlays the HTML doc. */}
      {live && !embedded && (
        <div className="flex flex-shrink-0 items-center justify-end px-2 py-1">
          <button
            type="button"
            onClick={handleSnapshotClick}
            aria-label="Snapshot this dashboard to context"
            className="flex items-center gap-1.5 rounded-md border border-border bg-popover px-2 py-1 text-xs text-foreground shadow-sm transition-colors hover:bg-popover/80"
          >
            <Camera size={14} />
            Snapshot
          </button>
        </div>
      )}
      {children.map(
        (child) =>
          child.message && (
            <HtmlViewer
              key={child.frameId}
              hostedFrame={child}
              presented={presented}
              allowAutoFocus={allowAutoFocus}
              onCandidateFocus={onCandidateFocus}
              hostTheme={theme}
              parentPanelId={overlayPanelId}
              parentElement={frame}
              parentDashboard={dashboardActions}
              parentMenuPage={parentMenuPage ?? menuPage}
              hostTeamKey={teamKey}
              keepTeam={keepTeam}
              filePath={child.documentPath}
              teamContentPath={child.contentRoot}
              message={{ ...child.message, sender: child.grant.principal }}
              visible={docVisible}
              onContentSize={(height) =>
                postToFrame(endpoint(), {
                  type: "codesense:message-size",
                  ...child.message,
                  height: messageFrameHeight(height),
                })
              }
            />
          ),
      )}
      <iframe
        inert={inputBlocked || undefined}
        title={frameTitle ?? filePath.split("/").pop() ?? "Dashboard"}
        ref={attachIframe}
        // The sandbox/origin are fixed at load time, so toggling the baseline view
        // remounts the frame rather than swapping `sandbox` on a live frame.
        key={live ? (frameRecord?.frameId ?? "live-pending") : "baseline"}
        sandbox={live ? LIVE_SANDBOX : BASELINE_SANDBOX}
        allow={HTML_FRAME_ALLOW}
        src={initialFrameSrc}
        onLoad={handleIframeLoad}
        onError={() => onDocProblemRef.current?.({ kind: "load", message: "frame crashed" })}
        // Matching the doc's own `color-scheme` keeps the frame transparent over
        // the surface it sits on.
        style={{
          ...(embedded ? firstPaint.style : { ...firstPaint.style, colorScheme: undefined }),
          ...(awaitingReady ? { opacity: 0 } : undefined),
        }}
        className="min-h-0 w-full flex-1 border-0"
      />
    </div>
  );
}

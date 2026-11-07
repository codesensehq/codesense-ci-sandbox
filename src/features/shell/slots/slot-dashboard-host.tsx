// SPDX-License-Identifier: MIT
import {
  PaneStartupSurface,
  usePaneFirstPaint,
} from "@/features/dashboard/components/pane-startup-surface";
import { logger } from "@/shared/lib/logger";
import { Inbox } from "lucide-react";
import { isInboxDashboard } from "@/features/dashboard/lib/dashboard-profile";
import { canonicalSurface } from "./slot-registry";
import { createPortal } from "react-dom";
import { registerRenderedDocument } from "@/shared/hooks/use-rendered-document-bridge";
import { ProblemBanner } from "../right-pane/panes/pane-dev-banner";
import {
  cloneElement,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type ReactElement,
  type ComponentProps,
} from "react";
import {
  HtmlViewer,
  type DocBadge,
  type DocProblem,
} from "@/features/dashboard/components/html-viewer";
import { useDashboardPolicy } from "@/features/dashboard/lib/verb-policy";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import {
  captureReloadState,
  setFrameFrozen,
  frameScrollY,
  restoreFrameFocus,
  flushFrameWrites,
  dropQueuedCommands,
  retireAfterFlush,
  PANE_STARTUP_TIMEOUT_MS,
} from "@/features/dashboard/lib/frame-lifecycle";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  listBuiltinPanes,
  type FrameTeam,
  type PaneEntry,
  type PaneListing,
} from "@/tauri/commands";
import { buildTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import { usePanelRegistry } from "../right-pane/panel-registry";
import type { PaneIntent } from "../right-pane/panes/pane-store";
import { nativeSurface, SLOT_REGISTRY, type SlotId } from "./slot-registry";
import { SlotFallbackStrip, type SlotFailure } from "./slot-fallback-strip";
import { NativeSlotProvider } from "./native-slot-context";
import { useSlotDebugTrace, type SlotRefreshTrigger } from "./slot-debug";

type PaneFrame = {
  attempt: number;
  viewer: ReactElement<ComponentProps<typeof HtmlViewer>>;
  wasLive: boolean;
};

type SlotDiagnosticState = {
  attempt: number;
  isLive: boolean;
  probing: boolean;
  frozen: boolean;
  retainedAttempt: number | null;
  failureKind: SlotFailure["kind"] | null;
  visible: boolean;
};

/** Sample host and buffered frame geometry without pane content, paths or credentials. */
function sampleSlotGeometry(root: HTMLDivElement | null) {
  if (!root) return null;
  const host = root.getBoundingClientRect();
  const wrappers = [
    ...root.querySelectorAll<HTMLElement>("[data-pane-current], [data-pane-retained]"),
  ].slice(0, 2);
  return {
    host: { x: host.x, y: host.y, w: host.width, h: host.height },
    frames: wrappers.map((wrapper) => {
      const iframe = wrapper.querySelector("iframe");
      const style = iframe ? getComputedStyle(iframe) : null;
      const rect = iframe ? iframe.getBoundingClientRect() : null;
      return {
        role: wrapper.hasAttribute("data-pane-current") ? "current" : "retained",
        hidden: wrapper.classList.contains("invisible"),
        inert: wrapper.hasAttribute("inert"),
        rect: rect ? { x: rect.x, y: rect.y, w: rect.width, h: rect.height } : null,
        style: style
          ? { opacity: style.opacity, visibility: style.visibility, display: style.display }
          : null,
      };
    }),
  };
}

function dashboardRejection(status: PaneEntry["status"], reason?: string): SlotFailure | null {
  return status === "rejected"
    ? { kind: "rejected", technical: `Rejected: ${reason ?? "Dashboard rejected"}` }
    : null;
}

/** Recovery never borrows a retained listing's root from the team being left. */
function useBuiltinInboxRecovery(enabled: boolean, attempt: number, hostTeamKey?: string) {
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const bound = hostTeamKey ? parseTeamKey(hostTeamKey) : null;
  const org = hostTeamKey ? bound?.orgSlug : activeOrg;
  const team = hostTeamKey ? bound?.teamName : activeTeam;
  const teamKey = org && team ? buildTeamKey(org, team) : null;
  const [resolved, setResolved] = useState<{ teamKey: string; listing: PaneListing | null }>();
  const needed = enabled;
  useEffect(() => {
    if (!needed || !org || !team || !teamKey) return;
    let active = true;
    void listBuiltinPanes(org, team)
      .then((listing) => {
        if (active) setResolved({ teamKey, listing });
      })
      .catch((error) => {
        console.warn("Built-in Inbox recovery unavailable", error);
        if (active) setResolved({ teamKey, listing: null });
      });
    return () => {
      active = false;
    };
  }, [needed, org, team, teamKey, attempt]);
  return {
    listing: resolved?.teamKey === teamKey ? resolved.listing : null,
    pending: needed && !!teamKey && resolved?.teamKey !== teamKey,
    team: org && team ? { org, team } : undefined,
  };
}

export function SlotDashboardHost({
  slotId,
  entry,
  panelId = entry.panelId,
  contentRoot,
  teamKey,
  visible,
  intent,
  onIntentDelivered,
  onReload,
  onNavigate,
  onBadge,
  unavailable,
  refreshKey = 0,
  renderNative,
  stripClassName,
  headerless = false,
  fallbackTarget,
  fallback: fallbackConfig,
  keepTeam = false,
}: {
  fallbackTarget?: HTMLElement;
  /** The frame stays bound to the team it has: a warm org's dock while another org is on screen. */
  keepTeam?: boolean;
  fallback?: { entry: PaneEntry; contentRoot: string; readyWithinMs: number };
  slotId: SlotId;
  entry: PaneEntry;
  /** The host instance; the document keeps its own dashboard identity. */
  panelId?: string;
  contentRoot: string;
  /** The body owns this team while outgoing frames flush after a team switch. */
  teamKey?: string;
  visible: boolean;
  intent?: PaneIntent;
  /**
   * The page's version: a change refreshes the page. It must be a document revision
   * (`pageRevision`), never a per-reader one: Rust renumbers `sourceRevision` whenever a team's
   * source is re-created, so a per-reader key re-mints the frame on every warm return.
   */
  refreshKey?: number;
  onIntentDelivered?: () => void;
  onReload?: () => Promise<PaneEntry | null>;
  onNavigate?: (path: string) => void;
  onBadge?: (badge: DocBadge | null) => void;
  unavailable?: SlotFailure;
  /** The built-in body when the slot's native default is a component, not a panel. */
  renderNative?: () => ReactNode;
  /** Wraps the fallback strip, e.g. to cap its height in a narrow column. */
  stripClassName?: string;
  /** The slot draws no header row: after a successful Reload focus moves into the page. */
  headerless?: boolean;
}) {
  const activeOrg = useOrgStore((s) => s.activeOrgSlug);
  const activeTeam = useOrgStore((s) => s.activeTeamName);
  const unavailableKind = unavailable?.kind;
  const unavailableTechnical = unavailable?.technical;
  const initialFailure = unavailable ?? dashboardRejection(entry.status, entry.rejectedReason);
  const [problems, setProblems] = useState<DocProblem[]>([]);
  const [failure, setFailure] = useState<SlotFailure | null>(initialFailure);
  const [failureAttempt, setFailureAttempt] = useState<number | null>(null);
  const [retiredFailureAttempt, setRetiredFailureAttempt] = useState<number | null>(null);
  const failureRef = useRef(failure);
  failureRef.current = failure;
  const [isLive, setIsLive] = useState(false);
  const [readyEpoch, setReadyEpoch] = useState(0);
  const [frozen, setFrozen] = useState(false);
  const [probing, setProbing] = useState(!initialFailure);
  const captureUnavailable = failure
    ? `Pane is showing fallback: ${failure.technical}. Fix the pane source, then reload its panel and retry.`
    : undefined;
  useEffect(() => {
    if (!captureUnavailable || probing || retainedRef.current) return;
    return registerRenderedDocument(entry.path, async () => {
      throw new Error(captureUnavailable);
    });
  }, [captureUnavailable, probing, entry.path]);
  const [reloading, setReloading] = useState(false);
  const [attempts, setAttempts] = useState(1);
  const [triedAt, setTriedAt] = useState<Date>();
  const [announcement, setAnnouncement] = useState("");
  const [viewKey, setViewKey] = useState<string | null>(null);
  const nativeKey =
    slotId === "file-nav" && activeOrg && activeTeam ? `team:${activeOrg}/${activeTeam}` : viewKey;
  const actualViewKey = useRef<string | null>(null);
  const [returnFocus, setReturnFocus] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  // A frame that has gone live keeps the root it was minted for; the next attempt takes the
  // current one, so a save never swaps the shown frame's identity before its replacement is live.
  const attemptRoot = useRef<{
    attempt: number;
    contentRoot: string;
    settled: boolean;
    builtinTeam?: FrameTeam;
  }>({ attempt: attempts, contentRoot, settled: false });
  if (attemptRoot.current.attempt !== attempts)
    attemptRoot.current = { attempt: attempts, contentRoot, settled: false };
  else if (!attemptRoot.current.settled) attemptRoot.current.contentRoot = contentRoot;
  const live = useRef(false);
  const startupStarted = useRef(performance.now());
  const fallbackRemaining = useRef(fallbackConfig?.readyWithinMs ?? 0);
  const documentIdentity = useRef<string | null>(null);
  const [documentGeneration, setDocumentGeneration] = useState(0);
  const [retained, setRetained] = useState<PaneFrame | null>(null);
  const renderedViewer = useRef<PaneFrame | null>(null);
  const retainedRef = useRef(retained);
  retainedRef.current = retained;
  const retainShownFrame = useCallback(() => {
    const starting = !failureRef.current && failedAttempt.current !== badgeAttempt.current;
    if (renderedViewer.current && (live.current || (starting && !retainedRef.current))) {
      const frame = { ...renderedViewer.current, wasLive: live.current };
      retainedRef.current = frame;
      setRetained(frame);
    }
  }, []);
  const shownAttempt = useRef(1);
  const frameElements = useRef(new Map<number, HTMLIFrameElement>());
  const refreshTrigger = useRef<SlotRefreshTrigger>("reload");
  const wasFrameFocused = useRef(false);
  const frozenFrame = useRef<HTMLIFrameElement | null>(null);
  const unfreezeFrames = useRef(() => {});
  const [allowAutoFocus, setAllowAutoFocus] = useState(true);
  const onCandidateFocus = useCallback(() => {
    if (!preparation.current) return;
    const owner = document.activeElement;
    if (owner && owner !== document.body && !root.current?.contains(owner)) return;
    if (wasFrameFocused.current) restoreFrameFocus(frozenFrame.current);
  }, []);
  const preparation = useRef<{
    ready: boolean;
    source: boolean;
    writes: Promise<void>;
  } | null>(null);
  const [reloadState, setReloadState] = useState<Promise<unknown> | null>(null);
  const [reloadScrollY, setReloadScrollY] = useState(0);
  const sourceTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const pendingSource = useRef(false);
  const pageBadge = useRef<DocBadge | null>(null);
  // The one owner of the current attempt: only a real new attempt advances it, and `attempts`
  // mirrors it once that attempt's frame mounts. Unmount never moves it (StrictMode re-runs
  // mount effects), so async continuations check `disposed` instead.
  const badgeAttempt = useRef(1);
  const disposed = useRef(false);
  const isCurrentAttempt = useCallback(
    (attempt: number) => !disposed.current && badgeAttempt.current === attempt,
    [],
  );
  const failedAttempt = useRef<number | null>(null);
  const retryFocused = useRef(false);
  const manualSourceRefresh = useRef(false);
  const previousDiagnostic = useRef<SlotDiagnosticState | null>(null);
  const onPageStart = useCallback(
    (key: string) => {
      if (!key || key === documentIdentity.current) return;
      documentIdentity.current = key;
      if (live.current) {
        startupStarted.current = performance.now();
        setDocumentGeneration((n) => n + 1);
      }
      if (live.current) onBadge?.(null);
      live.current = false;
      pageBadge.current = null;
      setIsLive(false);
    },
    [onBadge],
  );
  const [retainedIntent, retainIntent] = useState(intent);
  const pageReceivedIntent = useRef(false);
  useEffect(() => {
    if (intent) {
      pageReceivedIntent.current = false;
      retainIntent(intent);
    }
  }, [intent]);
  const onFallbackIntentDelivered = useCallback(() => {
    retainIntent(undefined);
    onIntentDelivered?.();
  }, [onIntentDelivered]);
  const onPageIntentDelivered = useCallback(
    (attempt: number) => {
      if (
        shownAttempt.current === attempt &&
        !preparation.current &&
        (live.current || retainedRef.current)
      ) {
        retainIntent(undefined);
        onIntentDelivered?.();
      } else if (isCurrentAttempt(attempt)) pageReceivedIntent.current = true;
    },
    [isCurrentAttempt, onIntentDelivered],
  );
  const inbox =
    slotId === "right-pane" &&
    (isInboxDashboard(entry) ||
      canonicalSurface(entry.surface ?? entry.declaration?.surface) === "inbox");
  const canRecoverInbox = inbox && entry.dashboardRef?.scope !== "built-in";
  const recovery = useBuiltinInboxRecovery(canRecoverInbox && !!failure, attempts, teamKey);
  // A follows-context frame rebinds to the new team in place; its built-in team is part of the
  // mint identity only until it settles, so a switch never forces a re-mint.
  // A retained host (`keepTeam`) keeps the team it was minted for until it is ready or released.
  if (
    SLOT_REGISTRY[slotId].teamBinding === "follows-context" &&
    !attemptRoot.current.settled &&
    !(keepTeam && attemptRoot.current.builtinTeam)
  )
    attemptRoot.current.builtinTeam = recovery.team;
  const fallbackInbox = canRecoverInbox ? recovery.listing?.builtinInbox : undefined;
  const native = nativeSurface(
    slotId,
    entry.surface ??
      entry.declaration?.surface ??
      entry.dashboardRef?.installedFrom?.id ??
      entry.id,
  );
  const nativePanel = usePanelRegistry((s) =>
    native?.panelId ? s.panels[native.panelId] : undefined,
  );
  const nativeBody = native?.entry === "component" ? renderNative : nativePanel?.render;
  // A rejected dashboard has no grant; only its fallback shared UI follows this policy.
  const policy = useDashboardPolicy();
  useEffect(() => {
    const org = entry.dashboardRef?.org ?? useOrgStore.getState().activeOrgSlug;
    if (policy && !actualViewKey.current)
      setViewKey(
        policy.preset === "team-safe" && entry.scope === "team" && org && entry.team
          ? `team:${org}/${entry.team}`
          : "all",
      );
  }, [policy, entry.dashboardRef?.org, entry.scope, entry.team]);
  const dashboardKey = entry.dashboardRef ? dashboardRefKey(entry.dashboardRef) : null;
  const loggedFailure = useRef<string | null>(null);
  // Focus is ours to move only while it is unowned (body) or already inside this slot.
  const focusStillOurs = useCallback(() => {
    const owner = document.activeElement;
    return !owner || owner === document.body || !!root.current?.contains(owner);
  }, []);
  const fail = useCallback(
    (problem: SlotFailure) => {
      if (failedAttempt.current !== badgeAttempt.current && !retainedRef.current) onBadge?.(null);
      failedAttempt.current = badgeAttempt.current;
      setFailureAttempt(badgeAttempt.current);
      setRetiredFailureAttempt(null);
      const key = JSON.stringify([badgeAttempt.current, problem.kind, problem.technical]);
      if (loggedFailure.current !== key) {
        loggedFailure.current = key;
        logger.warn(
          "slot-fallback",
          JSON.stringify({
            slot: slotId,
            ref: dashboardKey ?? entry.path,
            attempt: badgeAttempt.current,
            instance: documentIdentity.current,
            elapsedMs: Math.max(0, Math.round(performance.now() - startupStarted.current)),
            reason: problem.kind,
            detail: problem.technical,
          }),
        );
      }
      manualSourceRefresh.current = false;
      live.current = false;
      setIsLive(false);
      setFrozen(false);
      unfreezeFrames.current();
      preparation.current = null;
      if (retainedRef.current && !retainedRef.current.wasLive) {
        retainedRef.current = null;
        setRetained(null);
      }
      clearTimeout(sourceTimer.current);
      // A page that fails while focused hands focus to the slot, never to <body>.
      if (
        !retainedRef.current &&
        !retryFocused.current &&
        root.current?.contains(document.activeElement)
      )
        root.current.focus({ preventScroll: true });
      // Commands queued for a page that won't load never run; the strip offers Reload.
      if (dashboardKey) dropQueuedCommands(dashboardKey, panelId);
      setFailure(problem);
      setProbing(!!fallbackConfig && !retainedRef.current);
      setReloading(false);
      if (retryFocused.current && focusStillOurs()) setReturnFocus(true);
      retryFocused.current = false;
    },
    [dashboardKey, panelId, onBadge, slotId, entry.path, fallbackConfig, focusStillOurs],
  );
  useEffect(() => {
    if (unavailableKind) fail({ kind: unavailableKind, technical: unavailableTechnical ?? "" });
  }, [unavailableKind, unavailableTechnical, fail]);
  useEffect(() => {
    // The cold shown attempt answers to the fallback clock below; every other attempt
    // (a save candidate, a retry) keeps this absolute deadline.
    const coldFallback = !!fallbackConfig && !retained && (!failure || failureAttempt === attempts);
    if ((!probing && !frozen) || isLive || coldFallback) return;
    const remaining = Math.max(
      0,
      PANE_STARTUP_TIMEOUT_MS - (performance.now() - startupStarted.current),
    );
    const timer = setTimeout(() => {
      if (!live.current)
        fail({ kind: "timeout", technical: "timed out waiting for codesense.ready()" });
    }, remaining);
    return () => clearTimeout(timer);
  }, [
    probing,
    frozen,
    isLive,
    failure,
    failureAttempt,
    retained,
    attempts,
    documentGeneration,
    fallbackConfig,
    fail,
  ]);
  useEffect(() => {
    fallbackRemaining.current = fallbackConfig?.readyWithinMs ?? 0;
  }, [attempts, fallbackConfig?.readyWithinMs]);
  useEffect(() => {
    if (!fallbackConfig || !visible || !probing || retained || failure || isLive) return;
    const started = performance.now();
    const timer = window.setTimeout(
      () => {
        fallbackRemaining.current = 0;
        if (isCurrentAttempt(attempts) && !live.current)
          fail({ kind: "timeout", technical: "timed out waiting for codesense.ready()" });
      },
      Math.max(0, fallbackRemaining.current),
    );
    return () => {
      window.clearTimeout(timer);
      fallbackRemaining.current = Math.max(
        0,
        fallbackRemaining.current - (performance.now() - started),
      );
    };
  }, [
    fallbackConfig,
    visible,
    probing,
    retained,
    failure,
    isLive,
    attempts,
    fail,
    isCurrentAttempt,
  ]);
  useEffect(() => {
    const fallbackEntryId = fallbackConfig?.entry.id;
    if (!fallbackEntryId || !failure || retained || failureAttempt === null) return;
    const timer = window.setTimeout(() => setRetiredFailureAttempt(failureAttempt), 300);
    return () => window.clearTimeout(timer);
  }, [fallbackConfig?.entry.id, failure, retained, failureAttempt]);
  useLayoutEffect(() => {
    if (!frozen && failure && retained && document.activeElement === frozenFrame.current)
      restoreFrameFocus(frozenFrame.current);
  }, [frozen, failure, retained]);
  const focusPage = useRef(false);
  useEffect(() => {
    if (failure || !focusPage.current) return;
    focusPage.current = false;
    const frame = frameElements.current.get(shownAttempt.current);
    if (!frame) return;
    frame.focus({ preventScroll: true });
    // Not every engine reports the frame's own focus event on a programmatic focus.
    frame.contentWindow?.postMessage({ type: "codesense:focus-in" }, new URL(frame.src).origin);
  }, [failure, isLive]);
  const keepFailedForLateLive =
    !!fallbackConfig &&
    !retained &&
    failureAttempt === attempts &&
    retiredFailureAttempt !== attempts;
  const onDocLive = useCallback(() => {
    if (!isCurrentAttempt(attempts) || live.current) return;
    if (failedAttempt.current === attempts) {
      if (!keepFailedForLateLive) return;
      failedAttempt.current = null;
    }
    live.current = true;
    attemptRoot.current.settled = true;
    setReadyEpoch((epoch) => epoch + 1);
    shownAttempt.current = attempts;
    setFrozen(false);
    preparation.current = null;
    loggedFailure.current = null;
    const outgoing = frozenFrame.current;
    const canFocus = focusStillOurs();
    setAllowAutoFocus(canFocus);
    if (outgoing && wasFrameFocused.current && canFocus) focusPage.current = true;
    manualSourceRefresh.current = false;
    onBadge?.(pageBadge.current);
    setIsLive(true);
    if (pageReceivedIntent.current) {
      retainIntent(undefined);
      onIntentDelivered?.();
    }
    setFailure(null);
    setFailureAttempt(null);
    setRetiredFailureAttempt(null);
    setReloading(false);
    setReturnFocus(false);
    setAnnouncement(`${entry.title} pane loaded.`);
    if (retryFocused.current && headerless && canFocus) {
      // No header row of its own (the dock): focus moves into the page (the shim's focus-in),
      // once the strip is gone and the page is no longer hidden under it.
      focusPage.current = true;
    } else if (retryFocused.current && canFocus) {
      let container = root.current?.parentElement;
      while (container) {
        const header = container.querySelector<HTMLElement>("[data-panel-header-row]");
        if (header) {
          header.tabIndex = -1;
          header.focus({ preventScroll: true });
          break;
        }
        container = container.parentElement;
      }
    }
    retryFocused.current = false;
  }, [
    entry.title,
    headerless,
    focusStillOurs,
    attempts,
    isCurrentAttempt,
    keepFailedForLateLive,
    onBadge,
    onIntentDelivered,
  ]);
  // Let the replacement commit paint before hiding and flushing the outgoing document.
  useLayoutEffect(() => {
    if (!retained || !isLive) return;
    const outgoing = frameElements.current.get(retained.attempt);
    const outgoingAttempt = retained.attempt;
    const paint = requestAnimationFrame(() => {
      void retireAfterFlush(
        (frame) => frame.element === outgoing,
        () => setRetained((current) => (current?.attempt === outgoingAttempt ? null : current)),
      );
    });
    return () => cancelAnimationFrame(paint);
  }, [retained, isLive]);
  const onDocProblem = useCallback(
    (problem: DocProblem) => {
      if (!isCurrentAttempt(attempts)) return;
      if (problem.kind === "bridge" || (problem.kind === "script" && live.current)) {
        setProblems((previous) =>
          problem.kind === "script" &&
          previous
            .slice(-3)
            .some((p) => p.kind === "bridge" && p.message.endsWith(`: ${problem.message}`))
            ? previous
            : [...previous.slice(-49), problem],
        );
        return;
      }
      fail({
        kind:
          problem.message === "frame crashed" || problem.message === "frame stopped responding"
            ? "crash"
            : problem.kind === "script"
              ? "script"
              : problem.message.includes("not found") || problem.message.includes("404")
                ? "missing"
                : "connection",
        technical: problem.message,
      });
    },
    [attempts, fail, isCurrentAttempt],
  );
  const begin = useCallback(
    (next: PaneEntry | null) => {
      if (failedAttempt.current === badgeAttempt.current) return;
      const problem = next
        ? dashboardRejection(next.status, next.rejectedReason)
        : { kind: "missing" as const, technical: `not found: ${entry.path}` };
      if (problem) fail(problem);
    },
    [entry.path, fail],
  );
  const freezeForReload = useCallback(
    (source = false) => {
      clearTimeout(sourceTimer.current);
      // Invalidate every callback from a superseded candidate before it can report ready.
      badgeAttempt.current += 1;
      failedAttempt.current = null;
      pageReceivedIntent.current = false;
      documentIdentity.current = null;
      startupStarted.current = performance.now();
      if (!preparation.current) {
        retainShownFrame();
        const outgoing = frameElements.current.get(shownAttempt.current);
        wasFrameFocused.current = !!outgoing && document.activeElement === outgoing;
        frozenFrame.current = outgoing ?? null;
        unfreezeFrames.current = setFrameFrozen(frozenFrame.current, true);
        const state = outgoing ? captureReloadState(outgoing) : Promise.resolve(null);
        const writes = outgoing
          ? flushFrameWrites(outgoing, Math.min(300, PANE_STARTUP_TIMEOUT_MS))
          : Promise.resolve();
        const prepared = { ready: !outgoing, source, writes };
        preparation.current = prepared;
        void writes.then(() => {
          prepared.ready = true;
        });
        setReloadState(state);
      }
      preparation.current.source = source;
      live.current = false;
      setIsLive(false);
      setFrozen(true);
      setProbing(false);
      setProblems([]);
      setReturnFocus(false);
      setReloading(true);
      setDocumentGeneration((n) => n + 1);
      setTriedAt(new Date());
      setAnnouncement(`Reloading ${entry.title} pane.`);
    },
    [entry.title, retainShownFrame],
  );
  const mountPrepared = useCallback(() => {
    const attempt = badgeAttempt.current;
    const mount = () => {
      if (!isCurrentAttempt(attempt) || failedAttempt.current === attempt) return;
      pendingSource.current = false;
      pageBadge.current = null;
      setReloadScrollY(frameScrollY(frozenFrame.current));
      setAttempts(attempt);
      setProbing(true);
    };
    if (!preparation.current || preparation.current.ready) mount();
    else void preparation.current.writes.then(mount);
  }, [isCurrentAttempt]);
  const startAttempt = useCallback(() => {
    refreshTrigger.current = "reload";
    pendingSource.current = false;
    freezeForReload();
    mountPrepared();
  }, [freezeForReload, mountPrepared]);
  const onPageBadge = useCallback(
    (badge: DocBadge | null) => {
      if (
        shownAttempt.current === attempts &&
        (live.current || retainedRef.current?.attempt === attempts)
      ) {
        onBadge?.(badge);
      }
      if (isCurrentAttempt(attempts)) pageBadge.current = badge;
    },
    [attempts, isCurrentAttempt, onBadge],
  );
  // A retained page reports its badge once; its owner showing it again republishes that badge.
  const onBadgeRef = useRef(onBadge);
  useEffect(() => {
    onBadgeRef.current = onBadge;
  });
  useEffect(() => {
    if (visible && live.current) onBadgeRef.current?.(pageBadge.current);
  }, [visible]);
  const onFallbackBadge = useCallback(
    (badge: DocBadge | null) => {
      if (!live.current && isCurrentAttempt(attempts)) onBadge?.(badge);
    },
    [attempts, isCurrentAttempt, onBadge],
  );
  const sourceRevision = JSON.stringify([entry.path, entry.modified ?? refreshKey]);
  const refreshIdentity = JSON.stringify([refreshKey, sourceRevision]);
  const previousRefresh = useRef(refreshIdentity);
  const previousSourceRevision = useRef(sourceRevision);
  useEffect(() => {
    const changed = previousRefresh.current !== refreshIdentity;
    const sourceChanged = previousSourceRevision.current !== sourceRevision;
    if (changed) previousRefresh.current = refreshIdentity;
    if (sourceChanged) previousSourceRevision.current = sourceRevision;
    if (unavailableKind) return;
    const rejected = dashboardRejection(entry.status, entry.rejectedReason);
    if (rejected) {
      if (live.current) retainShownFrame();
      fail(rejected);
      return;
    }
    if (changed && manualSourceRefresh.current) {
      manualSourceRefresh.current = false;
      begin(entry);
      return;
    }
    // Keep an active cold attempt alive until its first ready signal. Preserve source
    // work while it starts, then drain it (or wait for reveal if hidden). A failed
    // attempt must retry a corrected source.
    const activeCold =
      !live.current && !retainedRef.current && failedAttempt.current !== badgeAttempt.current;
    if (activeCold && !visible) {
      if (changed && sourceChanged) pendingSource.current = true;
      // Hidden before it goes live, the Reload's focus intent dies with the reveal.
      retryFocused.current = false;
      return;
    }
    if (changed) pendingSource.current = true;
    if (!visible) {
      if (preparation.current?.source) {
        clearTimeout(sourceTimer.current);
        badgeAttempt.current += 1;
        preparation.current = null;
        unfreezeFrames.current();
        setFrozen(false);
        setReloading(false);
        setProbing(false);
        pendingSource.current = true;
        // The cancelled Reload never goes live or fails; its focus intent dies with it.
        retryFocused.current = false;
      }
      return;
    }
    if (!pendingSource.current) return;
    // A user Reload still in flight keeps its focus intent across this source-refresh remount.
    refreshTrigger.current = "source";
    freezeForReload(true);
    sourceTimer.current = setTimeout(() => {
      mountPrepared();
    }, 150);
    return () => clearTimeout(sourceTimer.current);
  }, [
    refreshIdentity,
    sourceRevision,
    retainShownFrame,
    visible,
    unavailableKind,
    freezeForReload,
    mountPrepared,
    begin,
    entry,
    fail,
  ]);
  useEffect(() => {
    if (!readyEpoch || !pendingSource.current || !visible || !live.current) return;
    refreshTrigger.current = "readyEpoch";
    freezeForReload(true);
    sourceTimer.current = setTimeout(() => {
      mountPrepared();
    }, 150);
    return () => clearTimeout(sourceTimer.current);
  }, [readyEpoch, visible, freezeForReload, mountPrepared]);
  useEffect(() => {
    disposed.current = false;
    return () => {
      disposed.current = true;
      clearTimeout(sourceTimer.current);
    };
  }, []);
  const reload = () => {
    retryFocused.current = !!root.current?.contains(document.activeElement);
    manualSourceRefresh.current = !!onReload;
    startAttempt();
    const attempt = badgeAttempt.current;
    if (onReload)
      void onReload()
        .then((next) => {
          if (isCurrentAttempt(attempt)) begin(next);
        })
        .catch((error) => {
          if (isCurrentAttempt(attempt) && failedAttempt.current !== attempt)
            fail({ kind: "connection", technical: String(error) });
        })
        // A read that changed nothing must not swallow the next real source change.
        .finally(() => {
          if (isCurrentAttempt(attempt)) manualSourceRefresh.current = false;
        });
    else begin(entry);
  };
  const starting = (probing || frozen) && !failure && !isLive;
  const presentation = usePaneFirstPaint(String(attempts), true, visible);
  const presentationRef = presentation.ref;
  useSlotDebugTrace({
    slotId,
    panelId,
    teamKey,
    attempts,
    refreshKey,
    sourceRevision,
    visible,
    trigger: refreshTrigger,
    root,
    frames: frameElements,
  });
  useEffect(() => {
    const current: SlotDiagnosticState = {
      attempt: attempts,
      isLive,
      probing,
      frozen,
      retainedAttempt: retained?.attempt ?? null,
      failureKind: failure?.kind ?? null,
      visible,
    };
    const previous = previousDiagnostic.current;
    previousDiagnostic.current = current;
    let transition: string | null = null;
    if (!previous) {
      transition = current.probing
        ? "startup"
        : current.isLive
          ? "live"
          : current.failureKind
            ? "fallback"
            : null;
    } else {
      if (!previous.failureKind && current.failureKind) transition = "fallback";
      else if (!previous.isLive && current.isLive) transition = "live";
      else if (previous.attempt !== current.attempt && !current.isLive) transition = "startup";
      else if (previous.visible && !current.visible) transition = "hidden";
      else if (!previous.visible && current.visible) transition = "revealed";
    }
    if (!transition) return;
    logger.info(
      "slot-lifecycle",
      JSON.stringify({
        slot: slotId,
        panelId,
        transition,
        ...current,
        geometry: sampleSlotGeometry(root.current),
      }),
    );
  }, [
    attempts,
    isLive,
    probing,
    frozen,
    retained?.attempt,
    failure?.kind,
    visible,
    slotId,
    panelId,
  ]);
  const attachRoot = useCallback(
    (node: HTMLDivElement | null) => {
      root.current = node;
      presentationRef(node);
    },
    [presentationRef],
  );
  const showingEntryFallback = !!failure && !retained && !!fallbackConfig;
  const strip = failure && !showingEntryFallback && (
    <SlotFallbackStrip
      entry={entry}
      surface={retained && !isLive ? undefined : fallbackInbox ? "Inbox" : native?.label}
      keepingPrevious={!!retained && !isLive}
      failure={failure}
      reloading={reloading}
      attempts={attempts}
      triedAt={triedAt}
      onReload={reload}
      returnFocus={returnFocus}
    />
  );
  const fallback = (
    <>
      {strip && (stripClassName ? <div className={stripClassName}>{strip}</div> : strip)}
      {failure && !retained && inbox && (
        <div className="flex min-h-0 flex-1 flex-col">
          {fallbackInbox ? (
            <SlotDashboardHost
              slotId={slotId}
              entry={fallbackInbox}
              panelId={panelId}
              contentRoot={recovery.listing!.contentRoot}
              teamKey={teamKey}
              visible={visible && !reloading}
              intent={retainedIntent}
              onIntentDelivered={onFallbackIntentDelivered}
              onNavigate={onNavigate}
              onBadge={onFallbackBadge}
            />
          ) : recovery.pending ? (
            <p role="status" className="p-6 text-center text-sm text-muted-foreground">
              Loading the built-in Inbox…
            </p>
          ) : (
            <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center text-sm text-muted-foreground">
              <Inbox className="size-6" aria-hidden="true" />
              <p className="font-medium">Your messages are safe.</p>
              <p>New mail still arrives. To read an agent's mail now, open its session's inbox.</p>
            </div>
          )}
        </div>
      )}
      {failure && !retained && nativeBody && (
        <div className="flex min-h-0 flex-1 flex-col">
          <NativeSlotProvider
            key={`${slotId}:${nativeKey}`}
            slotId={slotId}
            panelId={panelId}
            viewKey={nativeKey}
            intent={retainedIntent}
            onIntentDelivered={onFallbackIntentDelivered}
          >
            {nativeBody()}
          </NativeSlotProvider>
        </div>
      )}
      {failure && !retained && fallbackConfig && (
        <div
          className="flex min-h-0 flex-1 flex-col"
          data-slot-fallback-entry={fallbackConfig.entry.id}
        >
          <div
            className="flex shrink-0 items-center justify-between gap-3 px-3 py-2 text-xs"
            role="status"
          >
            <span className="min-w-0 truncate">{failure.technical}</span>
            <button className="shrink-0 underline" onClick={reload} type="button">
              Retry
            </button>
          </div>
          <SlotDashboardHost
            slotId={slotId}
            entry={fallbackConfig.entry}
            contentRoot={fallbackConfig.contentRoot}
            teamKey={teamKey}
            keepTeam={keepTeam}
            visible={visible}
            intent={retainedIntent}
            onIntentDelivered={onFallbackIntentDelivered}
            onNavigate={onNavigate}
            onBadge={onFallbackBadge}
          />
        </div>
      )}
    </>
  );
  const viewer = (
    <HtmlViewer
      keepTeam={keepTeam}
      refreshKey={attempts}
      reloadState={reloadState}
      initialScrollY={reloadScrollY}
      onReloadPane={reload}
      onCandidateFocus={onCandidateFocus}
      allowAutoFocus={allowAutoFocus}
      filePath={entry.path}
      frameTitle={entry.title}
      captureUnavailable={captureUnavailable}
      teamContentPath={attemptRoot.current.contentRoot}
      pane={{
        slotId,
        panelId,
        badge: entry.declaration?.badge,
        contentRoot: attemptRoot.current.contentRoot,
        panesDir: entry.panesDir,
        fileName: entry.path.split("/").pop() ?? `${entry.id}.html`,
        dashboardRef: entry.dashboardRef,
        ...(entry.dashboardRef?.scope === "built-in"
          ? { builtinTeam: attemptRoot.current.builtinTeam ?? recovery.team }
          : {}),
      }}
      visible={visible && (!inbox || !failure || reloading)}
      intent={retainedIntent}
      onIntentDelivered={() => onPageIntentDelivered(attempts)}
      onPageStart={(key) => {
        if (isCurrentAttempt(attempts)) onPageStart(key);
      }}
      onDocLive={onDocLive}
      onDocProblem={onDocProblem}
      onFrameViewKey={(key) => {
        actualViewKey.current = key;
        setViewKey(key);
      }}
      onNavigate={onNavigate}
      onBadge={onPageBadge}
    />
  );
  const currentFrame = { attempt: attempts, viewer, wasLive: live.current };
  if (isLive || probing) renderedViewer.current = currentFrame;
  const frames = [
    ...(retained ? [retained] : []),
    ...((probing && !(fallbackConfig && failure && failureAttempt === attempts)) ||
    keepFailedForLateLive
      ? [currentFrame]
      : []),
  ];
  return (
    <div
      ref={attachRoot}
      tabIndex={-1}
      className="relative flex h-full min-h-0 w-full flex-col outline-none"
      style={{
        ...presentation.style,
        pointerEvents: fallbackTarget && failure && !retained ? "none" : "auto",
      }}
      data-slot-id={slotId}
      aria-busy={starting || undefined}
    >
      <span className="sr-only" aria-live="polite">
        {announcement}
      </span>
      {problems.length > 0 && !failure && (
        <ProblemBanner
          problems={problems}
          total={problems.length}
          onDismiss={() => setProblems([])}
        />
      )}
      {fallbackTarget ? createPortal(fallback, fallbackTarget) : fallback}
      <PaneStartupSurface
        title={entry.title}
        state={
          isLive ? "ready" : retained && frozen ? "recovering" : starting ? "starting" : "ready"
        }
        onReload={reload}
        onEdit={onNavigate ? () => onNavigate(entry.path) : undefined}
      />
      {frames.map((item) => {
        const shown = item.attempt === shownAttempt.current;
        const hidden = !shown || (!!failure && !retained);
        return (
          <div
            key={item.attempt}
            data-pane-current={item.attempt === attempts || undefined}
            data-pane-retained={item.attempt !== attempts || undefined}
            className={
              hidden
                ? "pointer-events-none invisible absolute inset-0"
                : frozen
                  ? "pointer-events-none min-h-0 flex-1"
                  : "min-h-0 flex-1"
            }
            aria-hidden={hidden || undefined}
            inert={hidden || undefined}
          >
            {cloneElement(item.viewer, {
              onFrameElement: (element: HTMLIFrameElement | null) => {
                if (element) frameElements.current.set(item.attempt, element);
                else frameElements.current.delete(item.attempt);
              },
              visible: visible && (shown || probing),
              hostTheme: presentation.theme,
              presented: shown && !hidden,
              prepareIntent: !shown && probing,
              intent: !shown || !frozen ? retainedIntent : undefined,
              onIntentDelivered: () => onPageIntentDelivered(item.attempt),
            })}
          </div>
        );
      })}
    </div>
  );
}

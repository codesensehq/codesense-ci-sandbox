// SPDX-License-Identifier: MIT
/**
 * SW-RP-5 — the ONE shared, store-agnostic "live list" primitive both realtime
 * right-pane panes (Agents / Worktrees) render through, so realtime cues and
 * degraded recovery are defined once and can't diverge.
 *
 * It holds NO domain data: the caller passes a keyed, ordered `items` array + a
 * `refetch` closure from its own roster store (SW-AM-9 / SW-WU-9). This component
 * diffs `items` by key across renders and renders the cues:
 *  - add tint (fade of a decaying background — `rp5-live-added` keyframe),
 *  - user-removal collapse (`grid-template-rows` → 0fr, never `height`),
 *  - background-eviction tombstone (dimmed, strikethrough, `Removed`, ~2s),
 *  - `aria-live="polite"` announcements (coalesced on a burst),
 *  - a shared empty state + degraded banner.
 *
 * It renders NO title bar — SW-RP-1/3 already own the section header. The panel
 * feeds the live count into the registry `badge` slot via {@link LiveCountBadge}
 * (derived from the SAME `items`, so the count can never drift from the rows) and
 * its primary actions into the registry `headerActions` slot. There is no Refresh
 * button: recovery refetches happen silently on mount (pane open) and window
 * focus; channel-reconnect detection lives in the caller's store, which flips
 * {@link LiveListProps.degraded}.
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

import { cn } from "@/lib/utils";
import { usePrefersReducedMotion } from "@/shared/lib/use-prefers-reduced-motion";

/** How long the add tint lingers before it is cleared (matches the CSS keyframe). */
const ADD_TINT_MS = 1200;
/** How long an evicted row shows its `Removed` tombstone before it collapses. */
const TOMBSTONE_MS = 2000;
/** Collapse-transition duration (matches the `.rp5-row` CSS transition). */
const COLLAPSE_MS = 150;
/** A frame's grace so the row paints at full height before it transitions to 0fr. */
const COLLAPSE_ARM_MS = 20;
/** Net add+remove churn above this in one batch = a "burst": suppress per-row
 *  tints/tombstones and coalesce the announcement to counts. */
const BURST_THRESHOLD = 5;
/** How long the badge pulse flag stays set before it clears — a touch longer than
 *  the 600ms CSS keyframe so the animation always completes. */
const PULSE_CLEAR_MS = 650;

export type LiveListDegraded = { status: "reconnecting" } | { status: "failed" };

/** Nouns for coalesced burst announcements, e.g. `{ one: "Agent", many: "agents" }`. */
export interface LiveListNoun {
  one: string;
  many: string;
}

export interface LiveListProps<T> {
  /**
   * Ordered, keyed items. `null` ⇒ not-yet-loaded (renders `loadingState`); `[]`
   * ⇒ loaded-empty (renders `emptyState`). Live rows always render from this
   * array (the source of truth) so in-place updates are never stale.
   */
  items: T[] | null;
  getKey: (item: T) => string;
  renderRow: (item: T) => ReactNode;
  /** Human label for a row — used ONLY for aria-live announcements. */
  getLabel?: (item: T) => string;
  /** Nouns for coalesced burst announcements ("3 agents added"). */
  noun?: LiveListNoun;
  /**
   * Keys removed by BACKGROUND eviction (e.g. a worktree reap) → they get the
   * `Removed` tombstone. Any removal NOT in this set is treated as
   * user-initiated → it simply collapses. Read at diff time only.
   */
  evictedKeys?: readonly string[];
  /** Degraded live-channel state; `null`/omitted ⇒ healthy (no banner). */
  degraded?: LiveListDegraded | null;
  /** Invoked by the failed-state `Retry` button. */
  onRetry?: () => void;
  /**
   * Silent recovery refetch — invoked on mount (pane open) and on window focus.
   * May return a promise; while it is pending, further recovery calls are
   * skipped (the "already in flight" guard). Memoize it (`useCallback`).
   */
  refetch?: () => void | Promise<unknown>;
  /** Refetch on mount (pane open). Default `true`. */
  recoverOnMount?: boolean;
  /** Caller-provided copy for the loaded-empty state (parallel voice across panes). */
  emptyState?: ReactNode;
  /** Shown while `items === null`. */
  loadingState?: ReactNode;
  className?: string;
  "data-testid"?: string;
}

interface ExitingRow<T> {
  /** Unique per exit instance — the React key + the timer target — so a key
   *  that is removed and re-added before its tombstone drops never collides
   *  with the live row (or with a later exit of the same key). */
  id: number;
  key: string;
  item: T;
  /** Position in the list when the row was removed (for in-place tombstoning). */
  index: number;
  /** Background eviction (tombstone) vs user removal (plain collapse). */
  evicted: boolean;
  collapsing: boolean;
}

function announcement(
  addedLabels: string[],
  removedLabels: string[],
  noun: LiveListNoun | undefined,
): string {
  const one = noun?.one ?? "Item";
  const many = noun?.many ?? "items";
  const parts: string[] = [];
  if (addedLabels.length === 1) parts.push(`${one} added: ${addedLabels[0]}`);
  else if (addedLabels.length > 1) parts.push(`${addedLabels.length} ${many} added`);
  if (removedLabels.length === 1) parts.push(`${one} removed: ${removedLabels[0]}`);
  else if (removedLabels.length > 1) parts.push(`${removedLabels.length} ${many} removed`);
  return parts.join(", ");
}

export function LiveList<T>({
  items,
  getKey,
  renderRow,
  getLabel,
  noun,
  evictedKeys,
  degraded,
  onRetry,
  refetch,
  recoverOnMount = true,
  emptyState,
  loadingState,
  className,
  "data-testid": testid,
}: LiveListProps<T>) {
  const reducedMotion = usePrefersReducedMotion();

  const [recentlyAdded, setRecentlyAdded] = useState<Set<string>>(() => new Set());
  const [exiting, setExiting] = useState<ExitingRow<T>[]>([]);
  const [announce, setAnnounce] = useState("");
  const [reconciledItems, setReconciledItems] = useState(items);
  if (items !== reconciledItems) {
    setReconciledItems(items);
    if (items != null) {
      const currentKeys = new Set(items.map(getKey));
      setExiting((previous) => previous.filter((row) => !currentKeys.has(row.key)));
    }
  }
  const [wasLoading, setWasLoading] = useState(items == null);
  if (wasLoading !== (items == null)) {
    setWasLoading(items == null);
    if (items == null) {
      setExiting([]);
      setRecentlyAdded(new Set());
    }
  }

  // Snapshot of the last-seen items (key → item) so a removed row still has its
  // data to render as a tombstone after it has left `items`.
  const prevMapRef = useRef<Map<string, T>>(new Map());
  const hasRenderedRef = useRef(false);
  const wasDegradedRef = useRef(false);
  const timersRef = useRef<Set<ReturnType<typeof setTimeout>>>(new Set());
  const exitSeqRef = useRef(0);
  const announceSeqRef = useRef(0);
  // Per-key add-tint clear timers, so a key that flaps (re-added while its tint
  // is still running) re-arms its own timer instead of a stale batch timer
  // cutting the fresh tint short.
  const addTintTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  // Set when the channel recovers (degraded → healthy). The NEXT reconcile that
  // carries changes lands silently, even if the corrected roster arrives a tick
  // after the degraded flag clears (the common reconnect ordering).
  const pendingReconcileRef = useRef(false);

  // Keep the latest render helpers in a ref so the diff effect can depend on
  // `items`/`degraded`/`reducedMotion` alone (callers need not memoize
  // getKey/getLabel/etc.). Synced in a layout effect — which runs before the
  // diff effect in the same commit — rather than during render (the house idiom
  // for a latest-value ref; see settings-panel activeSectionRef).
  const helpers = useRef({ getKey, getLabel, noun, evictedKeys });
  useLayoutEffect(() => {
    helpers.current = { getKey, getLabel, noun, evictedKeys };
  });

  const track = useCallback((fn: () => void, ms: number) => {
    const t = setTimeout(() => {
      timersRef.current.delete(t);
      fn();
    }, ms);
    timersRef.current.add(t);
    return t;
  }, []);

  // Clear every pending timer on unmount so a burst can't leave orphaned timers.
  useEffect(() => {
    const timers = timersRef.current;
    const tintTimers = addTintTimersRef.current;
    return () => {
      for (const t of timers) clearTimeout(t);
      timers.clear();
      for (const t of tintTimers.values()) clearTimeout(t);
      tintTimers.clear();
    };
  }, []);

  const scheduleExit = useCallback(
    (id: number, isEvicted: boolean, reduced: boolean) => {
      const drop = () =>
        setExiting((previous) =>
          previous.some((row) => row.id === id)
            ? previous.filter((row) => row.id !== id)
            : previous,
        );
      const startCollapse = () =>
        setExiting((previous) =>
          previous.some((row) => row.id === id)
            ? previous.map((row) => (row.id === id ? { ...row, collapsing: true } : row))
            : previous,
        );

      if (reduced) {
        // Only evicted rows reach here under reduced motion; hold the tombstone
        // text, then drop with no collapse animation.
        track(drop, TOMBSTONE_MS);
        return;
      }
      if (isEvicted) {
        track(startCollapse, TOMBSTONE_MS);
        track(drop, TOMBSTONE_MS + COLLAPSE_MS);
      } else {
        track(startCollapse, COLLAPSE_ARM_MS);
        track(drop, COLLAPSE_ARM_MS + COLLAPSE_MS);
      }
    },
    [track],
  );

  // --- The diff: detect adds/removes across `items` changes and drive the cues.
  useEffect(() => {
    if (items == null) {
      // Loading blip: forget the prior snapshot so the eventual reload
      // reconciles silently (no phantom add/remove from a stale diff), clear any
      // lingering exit/added state (a fresh load replaces the list), and cancel
      // pending cue timers so a rapid reload cycle can't pile them up.
      hasRenderedRef.current = false;
      prevMapRef.current = new Map();
      for (const t of timersRef.current) clearTimeout(t);
      timersRef.current.clear();
      for (const t of addTintTimersRef.current.values()) clearTimeout(t);
      addTintTimersRef.current.clear();
      return;
    }
    const { getKey: key, getLabel: label, noun: n, evictedKeys: evicted } = helpers.current;
    const reduced = reducedMotion;
    const curMap = new Map<string, T>();
    for (const it of items) curMap.set(key(it), it);
    const prevMap = prevMapRef.current;

    const first = !hasRenderedRef.current;
    hasRenderedRef.current = true;
    // Detect the recovery edge (degraded → healthy). Because the store may clear
    // `degraded` a tick BEFORE the corrected roster arrives, latch it and
    // silence the NEXT reconcile that carries changes — not just this render.
    if (wasDegradedRef.current && degraded == null) pendingReconcileRef.current = true;
    wasDegradedRef.current = degraded != null;
    // First population never animates (opening the pane isn't "N items added").
    const silent = first || pendingReconcileRef.current;

    const addedKeys: string[] = [];
    for (const k of curMap.keys()) if (!prevMap.has(k)) addedKeys.push(k);
    const removedKeys: string[] = [];
    for (const k of prevMap.keys()) if (!curMap.has(k)) removedKeys.push(k);
    const hasChanges = addedKeys.length > 0 || removedKeys.length > 0;

    if (silent) {
      // First population / post-recovery reconcile: adopt silently. Consume the
      // recovery latch once the delta it was waiting for actually lands, so the
      // next real change animates normally.
      if (pendingReconcileRef.current && hasChanges) pendingReconcileRef.current = false;
    } else if (hasChanges) {
      const labelOf = (m: Map<string, T>, k: string) => {
        const it = m.get(k);
        return it && label ? label(it) : k;
      };
      // A tiny alternating zero-width marker so two identical consecutive
      // messages still change the region's text and re-announce.
      const nonce = "​".repeat(announceSeqRef.current % 2);
      announceSeqRef.current += 1;
      setAnnounce(
        announcement(
          addedKeys.map((k) => labelOf(curMap, k)),
          removedKeys.map((k) => labelOf(prevMap, k)),
          n,
        ) + nonce,
      );

      const burst = addedKeys.length + removedKeys.length > BURST_THRESHOLD;
      const evictedSet = new Set(evicted ?? []);
      const prevKeys = [...prevMap.keys()];

      if (!burst) {
        if (addedKeys.length && !reduced) {
          setRecentlyAdded((prev) => new Set([...prev, ...addedKeys]));
          for (const k of addedKeys) {
            // Per-key re-arm: clear any prior tint timer for this key so a flap
            // (re-add during the tint) restarts cleanly instead of being cut short.
            const existing = addTintTimersRef.current.get(k);
            if (existing) clearTimeout(existing);
            const t = setTimeout(() => {
              addTintTimersRef.current.delete(k);
              setRecentlyAdded((prev) => {
                if (!prev.has(k)) return prev;
                const next = new Set(prev);
                next.delete(k);
                return next;
              });
            }, ADD_TINT_MS);
            addTintTimersRef.current.set(k, t);
          }
        }

        for (const k of removedKeys) {
          const item = prevMap.get(k) as T;
          const isEvicted = evictedSet.has(k);
          // Reduced motion: user removals drop instantly; evicted rows still
          // hold their `Removed` text (meaning), then drop with no collapse.
          if (reduced && !isEvicted) continue;
          const id = exitSeqRef.current++;
          const index = Math.max(0, prevKeys.indexOf(k));
          setExiting((prev) => [
            ...prev,
            { id, key: k, item, index, evicted: isEvicted, collapsing: false },
          ]);
          scheduleExit(id, isEvicted, reduced);
        }
        // On a burst: no per-row tints, no tombstones — removed rows drop
        // immediately (nothing added to `exiting`); the badge pulses instead
        // (LiveCountBadge detects the same key-churn independently).
      }
    }

    prevMapRef.current = curMap;
    // getKey/getLabel/noun/evictedKeys are read from a ref (synced above) so
    // inline caller closures don't re-subscribe the effect.
  }, [items, degraded, reducedMotion, scheduleExit, track]);

  // --- Headless recovery: silent refetch on mount (pane open) + window focus.
  const inFlightRef = useRef(false);
  const didMountRecover = useRef(false);
  const doRefetch = useCallback(() => {
    if (!refetch || inFlightRef.current) return;
    const r = refetch();
    if (r && typeof (r as Promise<unknown>).then === "function") {
      // While in flight, a focus + visibilitychange pair on one tab-return
      // collapses to a single call. `then(release, release)` releases on resolve
      // OR reject and — unlike `.finally` — swallows the rejection (no unhandled
      // rejection, no permanent wedge).
      inFlightRef.current = true;
      const release = () => {
        inFlightRef.current = false;
      };
      void (r as Promise<unknown>).then(release, release);
    }
  }, [refetch]);

  useEffect(() => {
    if (!refetch) return;
    if (recoverOnMount && !didMountRecover.current) {
      didMountRecover.current = true;
      doRefetch();
    }
    const onFocus = () => doRefetch();
    const onVisible = () => {
      if (typeof document !== "undefined" && document.visibilityState === "visible") doRefetch();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [doRefetch, recoverOnMount, refetch]);

  // --- Render ---------------------------------------------------------------
  const liveRows: Array<{ key: string; item: T; added: boolean }> = (items ?? []).map((item) => {
    const k = getKey(item);
    return { key: k, item, added: recentlyAdded.has(k) };
  });
  const liveKeys = new Set(liveRows.map((r) => r.key));

  // Splice tombstones/collapsing rows back into their last-known position. Exit
  // rows are keyed by their unique exit id (never colliding with a live row's
  // key) and a re-added key is shown as a live row, not a tombstone.
  type Rendered =
    | { kind: "live"; key: string; item: T; added: boolean }
    | { kind: "exit"; key: string; row: ExitingRow<T> };
  const rendered: Rendered[] = liveRows.map((r) => ({ kind: "live" as const, ...r }));
  for (const e of [...exiting]
    .filter((e) => !liveKeys.has(e.key))
    .sort((a, b) => a.index - b.index)) {
    rendered.splice(Math.min(e.index, rendered.length), 0, {
      kind: "exit",
      key: `exit:${e.id}`,
      row: e,
    });
  }

  let body: ReactNode;
  if (items == null) {
    body = loadingState ?? null;
  } else if (rendered.length === 0) {
    body = emptyState ?? null;
  } else {
    body = rendered.map((r) =>
      r.kind === "live" ? (
        <div key={r.key} className="rp5-row" data-rp5-added={r.added ? "true" : undefined}>
          <div>{renderRow(r.item)}</div>
        </div>
      ) : (
        // An exiting row (collapsing user-removal or ~2s eviction tombstone) is a
        // visual cue only — mark it `inert` so its rendered controls take no focus
        // and leave the a11y tree (`pointer-events-none` blocks the mouse but NOT
        // keyboard Tab, which would otherwise land on a dead row and fire its Exit /
        // navigate handlers on an already-removed item). The change announcement is
        // separate (the aria-live region above), so nothing is lost.
        <div
          key={r.key}
          className="rp5-row"
          data-rp5-collapsing={r.row.collapsing ? "true" : undefined}
          inert
        >
          <div
            className={cn(
              r.row.evicted && "pointer-events-none select-none opacity-50 line-through",
            )}
          >
            {renderRow(r.row.item)}
            {r.row.evicted ? (
              <span className="ml-2 align-middle text-[10px] uppercase tracking-wide no-underline">
                Removed
              </span>
            ) : null}
          </div>
        </div>
      ),
    );
  }

  return (
    <div className={cn("flex min-h-0 flex-col", className)} data-testid={testid}>
      <div
        aria-live="polite"
        aria-atomic="true"
        className="sr-only"
        data-testid="live-list-announcer"
      >
        {announce}
      </div>
      {degraded ? (
        <DegradedBanner
          degraded={degraded}
          onRetry={onRetry ?? (refetch ? doRefetch : undefined)}
        />
      ) : null}
      {body}
    </div>
  );
}

function DegradedBanner({
  degraded,
  onRetry,
}: {
  degraded: LiveListDegraded;
  onRetry?: () => void;
}) {
  const failed = degraded.status === "failed";
  return (
    <div
      role="status"
      className="flex items-center justify-between gap-2 border-b bg-muted/40 px-3 py-1 text-xs text-muted-foreground"
    >
      <span>{failed ? "Couldn't refresh." : "Live updates paused — reconnecting…"}</span>
      {failed && onRetry ? (
        <button
          type="button"
          onClick={onRetry}
          className="rounded px-1.5 py-0.5 font-medium text-foreground hover:bg-accent"
        >
          Retry
        </button>
      ) : null}
    </div>
  );
}

/**
 * The live count badge for the registry `badge` slot. Takes the SAME `items` the
 * rows render from (AC-6 — the count IS `items.length`, structurally: there is no
 * independent count source to drift). The number cross-fades on change; a burst
 * (net key-churn > {@link BURST_THRESHOLD}, the SAME signal the list uses to
 * suppress per-row tints) pulses the badge once instead. Both cues drop to
 * instant under reduced motion (CSS-guarded).
 */
export function LiveCountBadge<T>({
  items,
  getKey,
  className,
  "aria-label": ariaLabel,
  pulseToken,
}: {
  items: T[] | null;
  getKey: (item: T) => string;
  className?: string;
  /** Accessible name for the badge; the visible count is overridden by it, so a
   *  screen reader hears "3 agent sessions" rather than a context-free "3". */
  "aria-label"?: string;
  /**
   * SW-AM-10: bump this to pulse the badge on an EXTERNAL event even when the count
   * didn't change and no burst occurred (e.g. an agent moved to background — it was
   * already counted, so the burst path never fires). Reuses the same pulse cue.
   */
  pulseToken?: number | string;
}) {
  const reducedMotion = usePrefersReducedMotion();
  const count = items?.length ?? 0;
  const [transition, setTransition] = useState({
    items,
    keys: items == null ? null : new Set(items.map(getKey)),
    pulseToken,
    pulse: false,
    generation: 0,
  });
  if (items !== transition.items || pulseToken !== transition.pulseToken) {
    let shouldPulse = pulseToken !== transition.pulseToken;
    const currentKeys = items == null ? null : new Set(items.map(getKey));
    if (items !== transition.items && currentKeys != null && transition.keys != null) {
      let churn = 0;
      for (const key of currentKeys) if (!transition.keys.has(key)) churn++;
      for (const key of transition.keys) if (!currentKeys.has(key)) churn++;
      shouldPulse ||= churn > BURST_THRESHOLD;
    }
    const startPulse = shouldPulse && !reducedMotion;
    setTransition({
      items,
      keys: currentKeys,
      pulseToken,
      pulse: startPulse || transition.pulse,
      generation: startPulse ? transition.generation + 1 : transition.generation,
    });
  }

  // SW-AM-10: pulse on an external event token (an agent moved to background),
  // reusing the SAME pulse cue as the burst path. A nav-opened agent was already
  // counted, so `count` doesn't change and the burst path never fires — the token
  // is how the badge still flashes on the move. The ref inits to the mount-time
  // token, so the first run is a no-op (equal) — no pulse on initial render; only a
  // later change pulses. Reduced motion is skipped (also CSS-guarded).
  useEffect(() => {
    if (transition.generation === 0) return;
    const generation = transition.generation;
    const timer = setTimeout(
      () =>
        setTransition((current) =>
          current.generation === generation ? { ...current, pulse: false } : current,
        ),
      PULSE_CLEAR_MS,
    );
    return () => clearTimeout(timer);
  }, [transition.generation]);

  const pulse = transition.pulse;

  return (
    <span
      className={cn(
        "rounded bg-muted px-1.5 text-xs tabular-nums text-muted-foreground",
        className,
      )}
      aria-label={ariaLabel}
      data-rp5-pulse={pulse ? "true" : undefined}
      data-testid="live-count-badge"
    >
      <span key={count} data-rp5-count>
        {count}
      </span>
    </span>
  );
}

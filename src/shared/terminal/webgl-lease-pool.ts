// SPDX-License-Identifier: MIT
/**
 * The GPU renderer is borrowed. Browsers keep only about 16 live WebGL contexts
 * and drop the oldest past that (a blank pane), so terminal displays share at
 * most {@link WEBGL_LEASE_CAP} leases. Only shown displays hold one, in
 * most-recently-focused order; the rest render on xterm's DOM renderer with the
 * same content and look, and are promoted when a lease frees.
 *
 * - Focus wins: the most recently focused display always holds a lease, taking
 *   it from an idle (hidden) holder, and outranking the least recently focused.
 * - A hidden display keeps its lease for {@link WEBGL_RELEASE_DEBOUNCE_MS}, so a
 *   quick re-show keeps the context.
 * - A lost context (or a failed load) falls back to DOM and re-requests a lease
 *   on the next frame. {@link WEBGL_LOSS_PIN_COUNT} losses within
 *   {@link WEBGL_LOSS_WINDOW_MS} pin the display to DOM until its next attach.
 *
 * The pool only decides who holds a lease. Swapping renderers is the
 * {@link LeaseRenderer}'s job, and never touches geometry or content.
 */

export const WEBGL_LEASE_CAP = 10;
/** How long a hidden display keeps its lease. Re-showing inside it keeps the context. */
export const WEBGL_RELEASE_DEBOUNCE_MS = 800;
export const WEBGL_LOSS_WINDOW_MS = 10_000;
export const WEBGL_LOSS_PIN_COUNT = 3;

/** A display's renderer switch. */
export interface LeaseRenderer {
  /** Swap to WebGL. `false` if no context could be had (the display stays on
   *  DOM). `onLost` fires once if the context is lost later. */
  acquire(onLost: () => void): boolean;
  /** Swap back to DOM. */
  release(): void;
}

export interface WebglLeaseState {
  leased: boolean;
  /** Pinned to DOM after repeated context losses, until the next attach. */
  pinned: boolean;
}

/** A display's lease and the pool's fill, as the harness diagnostics read it. */
export interface WebglLeaseReading extends WebglLeaseState {
  held: number;
  cap: number;
}

export interface WebglLeasePool {
  add(id: string, renderer: LeaseRenderer): void;
  /** The display is gone: its lease (if any) is released and handed on. */
  remove(id: string): void;
  /** The display was attached to a slot: clears a thrash pin. */
  attached(id: string): void;
  /** The display is shown (attached and visible) or not. */
  setVisible(id: string, visible: boolean): void;
  focus(id: string): void;
  state(id: string): WebglLeaseState | null;
  readonly held: number;
  readonly cap: number;
  /** Ids holding a lease. */
  leased(): string[];
}

export interface WebglLeasePoolDeps {
  cap?: number;
  /** Run `task` on a coming frame. */
  schedule: (task: () => void) => void;
  now?: () => number;
}

interface Lessee {
  readonly id: string;
  readonly renderer: LeaseRenderer;
  visible: boolean;
  /** Order of the last focus; 0 = never focused. */
  focusedAt: number;
  /** Order in which it was last shown: ranks never-focused displays, first come first. */
  shownAt: number;
  leased: boolean;
  pinned: boolean;
  losses: number[];
  releaseTimer: ReturnType<typeof setTimeout> | null;
}

export function createWebglLeasePool(deps: WebglLeasePoolDeps): WebglLeasePool {
  const cap = deps.cap ?? WEBGL_LEASE_CAP;
  const now = deps.now ?? (() => performance.now());
  const lessees = new Map<string, Lessee>();
  let clock = 0;
  let held = 0;
  let reconcilePending = false;

  function requestReconcile(): void {
    if (reconcilePending) return;
    reconcilePending = true;
    deps.schedule(() => {
      reconcilePending = false;
      reconcile();
    });
  }

  function cancelRelease(l: Lessee): void {
    if (l.releaseTimer) clearTimeout(l.releaseTimer);
    l.releaseTimer = null;
  }

  function release(l: Lessee): void {
    cancelRelease(l);
    if (!l.leased) return;
    l.leased = false;
    held--;
    l.renderer.release();
  }

  function lost(l: Lessee): void {
    const t = now();
    l.losses = l.losses.filter((at) => t - at < WEBGL_LOSS_WINDOW_MS);
    l.losses.push(t);
    if (l.losses.length >= WEBGL_LOSS_PIN_COUNT) l.pinned = true;
    requestReconcile();
  }

  function grant(l: Lessee): void {
    let current = true;
    const ok = l.renderer.acquire(() => {
      if (!current || !l.leased || lessees.get(l.id) !== l) return;
      current = false;
      l.leased = false;
      held--;
      lost(l);
    });
    if (ok) {
      l.leased = true;
      held++;
    } else {
      current = false;
      lost(l);
    }
  }

  const byRank = (a: Lessee, b: Lessee) => b.focusedAt - a.focusedAt || a.shownAt - b.shownAt;

  function reconcile(): void {
    const wanted = [...lessees.values()]
      .filter((l) => l.visible && !l.pinned)
      .sort(byRank)
      .slice(0, cap);
    const want = new Set(wanted);
    // Shown holders outranked by a more recently focused display give way now.
    for (const l of lessees.values()) if (l.leased && l.visible && !want.has(l)) release(l);
    for (const [rank, l] of wanted.entries()) {
      if (l.leased) continue;
      if (held >= cap) {
        // Only the top display may cut short an idle holder's debounce.
        if (rank !== 0) continue;
        const idle = [...lessees.values()]
          .filter((o) => o.leased && !o.visible)
          .sort(byRank)
          .pop();
        if (!idle) continue;
        release(idle);
      }
      grant(l);
    }
  }

  return {
    add(id, renderer) {
      if (lessees.has(id)) return;
      lessees.set(id, {
        id,
        renderer,
        visible: false,
        focusedAt: 0,
        shownAt: 0,
        leased: false,
        pinned: false,
        losses: [],
        releaseTimer: null,
      });
    },

    remove(id) {
      const l = lessees.get(id);
      if (!l) return;
      release(l);
      lessees.delete(id);
      requestReconcile();
    },

    attached(id) {
      const l = lessees.get(id);
      if (!l) return;
      l.pinned = false;
      l.losses = [];
    },

    setVisible(id, visible) {
      const l = lessees.get(id);
      if (!l) return;
      if (visible) {
        cancelRelease(l);
        if (!l.visible) l.shownAt = ++clock;
        l.visible = true;
        requestReconcile();
        return;
      }
      l.visible = false;
      if (!l.leased || l.releaseTimer) return;
      l.releaseTimer = setTimeout(() => {
        l.releaseTimer = null;
        if (l.visible || lessees.get(id) !== l) return;
        release(l);
        requestReconcile();
      }, WEBGL_RELEASE_DEBOUNCE_MS);
    },

    focus(id) {
      const l = lessees.get(id);
      if (!l) return;
      l.focusedAt = ++clock;
      requestReconcile();
    },

    state(id) {
      const l = lessees.get(id);
      return l ? { leased: l.leased, pinned: l.pinned } : null;
    },

    get held() {
      return held;
    },

    cap,

    leased() {
      return [...lessees.values()].filter((l) => l.leased).map((l) => l.id);
    },
  };
}

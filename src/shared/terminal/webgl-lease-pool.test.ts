// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  WEBGL_LEASE_CAP,
  WEBGL_RELEASE_DEBOUNCE_MS,
  createWebglLeasePool,
  type LeaseRenderer,
  type WebglLeasePool,
} from "./webgl-lease-pool";

/** A renderer that counts its live WebGL addons and can lose its context. */
function fakeRenderer(opts: { fails?: () => boolean } = {}) {
  let onLost: (() => void) | null = null;
  const r = {
    live: 0,
    maxLive: 0,
    acquires: 0,
    acquire: vi.fn((lost: () => void) => {
      r.acquires++;
      if (opts.fails?.()) return false;
      r.live++;
      r.maxLive = Math.max(r.maxLive, r.live);
      onLost = lost;
      return true;
    }),
    release: vi.fn(() => {
      r.live--;
      onLost = null;
    }),
    loseContext() {
      if (!onLost) return;
      const lost = onLost;
      onLost = null;
      r.live--;
      lost();
    },
  };
  return r satisfies LeaseRenderer & Record<string, unknown>;
}

let now = 0;
function newPool(): WebglLeasePool {
  return createWebglLeasePool({
    schedule: (task) => requestAnimationFrame(task),
    now: () => now,
  });
}

/** One frame: the pool reconciles on the next rAF. */
const frame = () => vi.advanceTimersByTime(16);

beforeEach(() => {
  now = 0;
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "cancelAnimationFrame"],
  });
});

afterEach(() => {
  vi.useRealTimers();
});

function shownDisplays(pool: WebglLeasePool, n: number) {
  const renderers = Array.from({ length: n }, () => fakeRenderer());
  renderers.forEach((r, i) => {
    pool.add(`d${i}`, r);
    pool.attached(`d${i}`);
    pool.setVisible(`d${i}`, true);
  });
  frame();
  return renderers;
}

describe("WebGL lease pool (I7)", () => {
  it("cap_and_promotion", () => {
    const pool = newPool();
    const r = shownDisplays(pool, 14);
    expect(pool.held).toBe(WEBGL_LEASE_CAP);
    expect(r.filter((x) => x.live === 1)).toHaveLength(10);
    expect(r.filter((x) => x.live === 0)).toHaveLength(4);

    const leased = r.findIndex((x) => x.live === 1);
    const waiting = r.findIndex((x) => x.live === 0);
    pool.setVisible(`d${leased}`, false);
    frame();
    // Within the debounce the hidden display keeps its lease.
    expect(r[leased].live).toBe(1);
    expect(r[waiting].live).toBe(0);
    vi.advanceTimersByTime(WEBGL_RELEASE_DEBOUNCE_MS);
    expect(r[leased].live).toBe(0);
    frame();
    expect(r[waiting].live).toBe(1);
    expect(pool.held).toBe(10);
  });

  it("a re-show inside the debounce keeps the context", () => {
    const pool = newPool();
    const [r] = shownDisplays(pool, 1);
    pool.setVisible("d0", false);
    vi.advanceTimersByTime(WEBGL_RELEASE_DEBOUNCE_MS - 1);
    pool.setVisible("d0", true);
    vi.advanceTimersByTime(WEBGL_RELEASE_DEBOUNCE_MS * 2);
    expect(r.acquires).toBe(1);
    expect(r.release).not.toHaveBeenCalled();
  });

  it("focused_display_always_leased", () => {
    const pool = newPool();
    const r = shownDisplays(pool, 12);
    for (let i = 0; i < 10; i++) pool.focus(`d${i}`);
    frame();
    expect(r.slice(10).map((x) => x.live)).toEqual([0, 0]);

    pool.focus("d11");
    frame();
    expect(r[11].live).toBe(1);
    // d0 was focused least recently among the holders.
    expect(r[0].live).toBe(0);
    expect(pool.held).toBe(10);

    // With the cap held by an idle (hidden) display, focus still wins at once.
    pool.setVisible("d5", false);
    pool.focus("d10");
    frame();
    expect(r[10].live).toBe(1);
    expect(r[5].live).toBe(0);
    expect(pool.held).toBe(10);
  });

  it("context loss falls back and re-requests the lease on the next frame", () => {
    const pool = newPool();
    const [r] = shownDisplays(pool, 1);
    r.loseContext();
    expect(pool.state("d0")).toEqual({ leased: false, pinned: false });
    expect(pool.held).toBe(0);
    frame();
    expect(r.live).toBe(1);
    expect(pool.state("d0")?.leased).toBe(true);
  });

  it("repeated_loss_pins_dom", () => {
    const pool = newPool();
    const r = shownDisplays(pool, 11);
    expect(r[10].live).toBe(0);
    for (let i = 0; i < 3; i++) {
      now += 1000;
      r[0].loseContext();
      frame();
    }
    expect(pool.state("d0")).toEqual({ leased: false, pinned: true });
    // The lease went to the next eligible display.
    expect(r[10].live).toBe(1);
    expect(pool.held).toBe(10);
    vi.advanceTimersByTime(60_000);
    expect(r[0].acquires).toBe(3);

    // Losses spread wider than the window never pin.
    const spread = newPool();
    const [s] = shownDisplays(spread, 1);
    for (let i = 0; i < 5; i++) {
      now += 6000;
      s.loseContext();
      frame();
    }
    expect(spread.state("d0")).toEqual({ leased: true, pinned: false });

    // The next attach clears the pin.
    pool.attached("d0");
    pool.setVisible("d0", false);
    pool.setVisible("d0", true);
    pool.focus("d0");
    frame();
    expect(pool.state("d0")).toEqual({ leased: true, pinned: false });
  });

  it("a failed load counts as a loss: bounded retries, then DOM", () => {
    const pool = newPool();
    const r = fakeRenderer({ fails: () => true });
    pool.add("d0", r);
    pool.setVisible("d0", true);
    for (let i = 0; i < 20; i++) frame();
    expect(r.acquires).toBe(3);
    expect(pool.state("d0")).toEqual({ leased: false, pinned: true });
  });

  it("no_lease_leak_seeded", () => {
    const seed = Number(process.env.WEBGL_LEASE_SEED ?? Date.now() % 2 ** 31);
    let a = seed >>> 0;
    const random = () => {
      a = (a + 0x6d2b79f5) >>> 0;
      let t = a;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
    const pool = newPool();
    const renderers = new Map<string, ReturnType<typeof fakeRenderer>>();
    const visible = new Set<string>();
    const all: Array<ReturnType<typeof fakeRenderer>> = [];
    for (let i = 0; i < 20; i++) {
      const r = fakeRenderer();
      renderers.set(`d${i}`, r);
      all.push(r);
      pool.add(`d${i}`, r);
    }
    for (let cycle = 0; cycle < 500; cycle++) {
      const id = `d${Math.floor(random() * 20)}`;
      const op = random();
      if (op < 0.35) {
        pool.attached(id);
        pool.setVisible(id, true);
        visible.add(id);
      } else if (op < 0.6) {
        pool.setVisible(id, false);
        visible.delete(id);
      } else if (op < 0.8) {
        pool.focus(id);
      } else if (op < 0.9) {
        now += random() * 5000;
        renderers.get(id)!.loseContext();
      } else {
        // A rebuild: the display goes, a new one takes its id.
        pool.remove(id);
        const r = fakeRenderer();
        renderers.set(id, r);
        all.push(r);
        pool.add(id, r);
        visible.delete(id);
      }
      vi.advanceTimersByTime(random() < 0.2 ? WEBGL_RELEASE_DEBOUNCE_MS + 16 : 16);
      const where = `seed ${seed}, cycle ${cycle}`;
      expect(pool.held, where).toBeLessThanOrEqual(WEBGL_LEASE_CAP);
      expect(pool.leased().length, where).toBe(pool.held);
      const live = all.reduce((sum, r) => sum + r.live, 0);
      expect(live, where).toBe(pool.held);
      for (const r of all) expect(r.live, where).toBeGreaterThanOrEqual(0);
      for (const r of all) expect(r.maxLive, where).toBeLessThanOrEqual(1);
    }
    // Once everything settles, only shown displays hold leases.
    vi.advanceTimersByTime(WEBGL_RELEASE_DEBOUNCE_MS + 16);
    for (const id of pool.leased()) expect(visible.has(id), `seed ${seed}`).toBe(true);
  });
});

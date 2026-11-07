// SPDX-License-Identifier: MIT
import { testAnyListPaging } from "@/shared/lib/live-embed/paging-contract.test.fixtures";
/**
 * The dashboard kit every live doc gets (`window.codesense.kit`, served by the
 * viewer server from src-tauri/src/http/routes/dashboard_kit/). Loaded here the
 * way a doc loads it: as a plain script against a window.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { AGENT_STATUS_WORDS } from "@/shared/lib/agent-status-config";
import { STATUS_CONFIG } from "@/shared/lib/pipeline-graph/status-config";

import {
  loadIcons,
  renderBlock,
  // @ts-expect-error -- a plain Node ESM script without type declarations.
} from "../../../../scripts/gen-dashboard-kit-icons.mjs";

const KIT_DIR = join(process.cwd(), "src-tauri/src/http/routes/dashboard_kit");
const KIT_JS = readFileSync(join(KIT_DIR, "kit.js"), "utf8");
const KIT_CSS = readFileSync(join(KIT_DIR, "kit.css"), "utf8");
const TOKENS_CSS = readFileSync(join(KIT_DIR, "tokens.css"), "utf8");
const VIRTUAL_LIST_JS = readFileSync(join(KIT_DIR, "virtual-list.js"), "utf8");

interface LaidOutNode {
  id: string;
  x: number;
  y: number;
  cx: number;
  cy: number;
  width: number;
  height: number;
  layer: number;
  order: number;
}
interface LaidOutEdge {
  from: string;
  to: string;
  points: { x: number; y: number }[];
  reversed: boolean;
}
interface Layout {
  width: number;
  height: number;
  direction: "TB" | "LR";
  crossings: number;
  nodes: LaidOutNode[];
  edges: LaidOutEdge[];
  reversed: { from: string; to: string }[];
  skipped: unknown[];
}
type Kit = {
  live: (render: () => void, opts: Record<string, unknown>) => () => void;
  version: string;
  layoutDag: (
    nodes: Array<string | { id: string; width?: number; height?: number }>,
    edges: Array<{ from: string; to: string }>,
    opts?: Record<string, unknown>,
  ) => Layout;
  edgePath: (points: { x: number; y: number }[], direction?: string) => string;
  dagSvg: (layout: Layout, render?: (n: LaidOutNode) => Element) => SVGSVGElement;
  icon: (name: string, opts?: Record<string, unknown>) => SVGSVGElement;
  iconNames: () => string[];
  statusDot: (state: string) => HTMLElement;
  statusBadge: (state: string) => HTMLElement;
  agentState: (state: string) => { label: string; color: string; icon: string };
  agentStates: Record<string, { label: string; color: string; icon: string }>;
  storyStatuses: Record<string, { label: string; color: string; icon: string }>;
  avatar: (opts: Record<string, unknown> | string) => HTMLElement;
  h: (tag: string, props?: Record<string, unknown> | null, ...children: unknown[]) => HTMLElement;
  timeAgo: (t: number | string, now?: number) => string;
  call: (verb: string, args?: unknown) => Promise<unknown>;
  has: (name: string) => boolean;
  queryAll: (
    verb: string,
    args: Record<string, unknown>,
    opts?: Record<string, unknown>,
  ) => Promise<Result>;
  subscribeAll: (
    verb: string,
    args: Record<string, unknown>,
    everyS: number,
    cb: (r: Result) => void,
    opts?: Record<string, unknown>,
  ) => (() => void) & { loadMore: () => Promise<Result>; refresh: () => Promise<Result> };
  partialList: (opts: Record<string, unknown>) => {
    el: HTMLElement;
    stop: () => void;
    loadMore: () => Promise<Result>;
    refresh: () => Promise<Result>;
  };
};

type Result = {
  rows: unknown[];
  total: number | null;
  complete: boolean;
  truncated: boolean;
  returned: number;
  loading?: boolean;
  error?: Error | null;
  canLoadMore?: boolean;
  atCeiling?: boolean;
  waitingForHead?: boolean;
  source?: string;
  answered_at?: number;
  server_time?: number;
  totals?: unknown;
  days?: unknown;
};

function loadKit(codesense: Record<string, unknown> = {}): Kit {
  const win = window as unknown as { codesense?: Record<string, unknown> };
  win.codesense = codesense;
  // One core per page: a test that loads the kit again starts from a fresh budget.
  delete (window as unknown as Record<string, unknown>).__csKitCore;
  new Function(KIT_JS)();
  return win.codesense.kit as Kit;
}

let kit: Kit;
beforeAll(() => {
  kit = loadKit();
});

const byId = (layout: Layout) => new Map(layout.nodes.map((n) => [n.id, n]));

describe("kit.layoutDag", () => {
  it("layers a launch tree by longest path, parents above children", () => {
    const layout = kit.layoutDag(
      ["root", "a", "b", "a1", "a2", "b1"],
      [
        { from: "root", to: "a" },
        { from: "root", to: "b" },
        { from: "a", to: "a1" },
        { from: "a", to: "a2" },
        { from: "b", to: "b1" },
      ],
    );
    const n = byId(layout);
    expect(n.get("root")!.layer).toBe(0);
    expect([n.get("a")!.layer, n.get("b")!.layer]).toEqual([1, 1]);
    expect([n.get("a1")!.layer, n.get("a2")!.layer, n.get("b1")!.layer]).toEqual([2, 2, 2]);
    for (const e of layout.edges) expect(n.get(e.from)!.cy).toBeLessThan(n.get(e.to)!.cy);
    expect(layout.crossings).toBe(0);
    // A parent sits over its children.
    const a = n.get("a")!;
    expect(a.cx).toBeCloseTo((n.get("a1")!.cx + n.get("a2")!.cx) / 2, 5);
  });

  it("places a node by its LONGEST path from a source", () => {
    const layout = kit.layoutDag(
      ["a", "b", "c"],
      [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
        { from: "a", to: "c" },
      ],
    );
    expect(byId(layout).get("c")!.layer).toBe(2);
    // The a→c edge skips a layer: it is routed through one virtual point.
    const skip = layout.edges.find((e) => e.from === "a" && e.to === "c")!;
    expect(skip.points).toHaveLength(3);
  });

  it("orders layers to remove avoidable crossings", () => {
    // Input order puts x's child second and y's child first: a crossing to undo.
    const layout = kit.layoutDag(
      ["x", "y", "cy", "cx"],
      [
        { from: "y", to: "cy" },
        { from: "x", to: "cx" },
        { from: "x", to: "cy" },
      ],
    );
    expect(layout.crossings).toBe(0);
    const n = byId(layout);
    expect(n.get("x")!.order).toBeLessThan(n.get("y")!.order);
    expect(n.get("cx")!.order).toBeLessThan(n.get("cy")!.order);
  });

  it("keeps nodes in a layer apart by at least nodeSep", () => {
    const ids = ["r", ...Array.from({ length: 6 }, (_, i) => `c${i}`)];
    const layout = kit.layoutDag(
      ids,
      ids.slice(1).map((id) => ({ from: "r", to: id })),
      { nodeWidth: 100, nodeSep: 20 },
    );
    const row = layout.nodes.filter((n) => n.layer === 1).sort((a, b) => a.x - b.x);
    for (let i = 1; i < row.length; i++) {
      expect(row[i].x - (row[i - 1].x + row[i - 1].width)).toBeGreaterThanOrEqual(20 - 1e-9);
    }
    for (const n of layout.nodes) {
      expect(n.x).toBeGreaterThanOrEqual(0);
      expect(n.x + n.width).toBeLessThanOrEqual(layout.width + 1e-9);
      expect(n.y + n.height).toBeLessThanOrEqual(layout.height + 1e-9);
    }
  });

  it("tolerates a cycle by reversing a back edge, and says which", () => {
    const layout = kit.layoutDag(
      ["a", "b", "c"],
      [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
        { from: "c", to: "a" },
      ],
    );
    expect(layout.reversed).toEqual([{ from: "c", to: "a" }]);
    const n = byId(layout);
    expect([n.get("a")!.layer, n.get("b")!.layer, n.get("c")!.layer]).toEqual([0, 1, 2]);
    // The reversed edge still reads from → to.
    const back = layout.edges.find((e) => e.reversed)!;
    expect(back.from).toBe("c");
    const first = back.points[0];
    const last = back.points[back.points.length - 1];
    expect(first.y).toBeGreaterThan(last.y);
  });

  it("skips self-loops and edges to unknown nodes, and collapses duplicates", () => {
    const layout = kit.layoutDag(
      ["a", "b", "a"],
      [
        { from: "a", to: "a" },
        { from: "a", to: "ghost" },
        { from: "a", to: "b" },
        { from: "a", to: "b" },
      ],
    );
    expect(layout.nodes).toHaveLength(2);
    expect(layout.edges).toHaveLength(1);
    expect(layout.skipped).toHaveLength(2);
  });

  it("lays out left to right, and an empty graph", () => {
    const layout = kit.layoutDag(
      [{ id: "a", width: 120, height: 30 }, "b"],
      [{ from: "a", to: "b" }],
      { direction: "LR" },
    );
    const n = byId(layout);
    expect(n.get("a")!.cx).toBeLessThan(n.get("b")!.cx);
    expect(n.get("a")!.width).toBe(120);
    expect(kit.layoutDag([], []).nodes).toEqual([]);
  });

  it("draws a layout as an SVG of curved edges and node cards", () => {
    const layout = kit.layoutDag(["a", "b"], [{ from: "a", to: "b" }]);
    expect(kit.edgePath(layout.edges[0].points)).toMatch(/^M[\d.]+,[\d.]+ C/);
    const el = kit.dagSvg(layout, (n) => kit.h("div", { class: "cs-dag-card", text: n.id }));
    expect(el.querySelectorAll("svg path.cs-dag-edge")).toHaveLength(1);
    // Nodes are HTML boxes over the SVG, not foreignObject (WebKit misdraws those
    // under a transform).
    expect(el.querySelectorAll("foreignObject")).toHaveLength(0);
    const boxes = el.querySelectorAll<HTMLElement>(".cs-dag-node");
    expect(boxes).toHaveLength(2);
    expect(boxes[1].style.top).toBe(`${layout.nodes[1].y}px`);
    expect(el.textContent).toContain("b");
  });

  it("clusters groups into framed boxes and marks the edges between them", () => {
    const nodes = [
      { id: "a1", g: "A" },
      { id: "a2", g: "A" },
      { id: "b1", g: "B" },
      { id: "b2", g: "B" },
    ];
    const layout = kit.layoutDag(
      nodes,
      [
        { from: "a1", to: "a2" },
        { from: "b1", to: "b2" },
        { from: "a2", to: "b1" },
      ],
      { group: (n: { g: string }) => n.g, maxWidth: 2000 },
    ) as Layout & {
      groups: { key: string; x: number; y: number; width: number; height: number }[];
    };
    expect(layout.groups.map((g) => g.key)).toEqual(["A", "B"]);
    const [ga, gb] = layout.groups;
    expect(ga.x + ga.width <= gb.x || ga.y + ga.height <= gb.y).toBe(true);
    const n = byId(layout);
    for (const [id, g] of [
      ["a1", ga],
      ["a2", ga],
      ["b1", gb],
      ["b2", gb],
    ] as const) {
      const node = n.get(id)!;
      expect(node.x).toBeGreaterThanOrEqual(g.x);
      expect(node.x + node.width).toBeLessThanOrEqual(g.x + g.width);
      expect(node.y).toBeGreaterThanOrEqual(g.y);
    }
    const cross = layout.edges.filter((e) => (e as LaidOutEdge & { cross?: boolean }).cross);
    expect(cross.map((e) => `${e.from}>${e.to}`)).toEqual(["a2>b1"]);
  });

  it("lays out a large graph quickly", () => {
    const ids = Array.from({ length: 300 }, (_, i) => `n${i}`);
    const edges = ids
      .slice(1)
      .map((id, i) => ({ from: ids[Math.floor((i * 7919) % (i + 1))], to: id }));
    const t0 = performance.now();
    const layout = kit.layoutDag(ids, edges);
    expect(layout.nodes).toHaveLength(300);
    expect(performance.now() - t0).toBeLessThan(1500);
  });
});

describe("kit.dagView", () => {
  type View = {
    el: HTMLElement;
    update: (n: unknown[], e: unknown[]) => void;
    select: (id: string | null) => void;
    layout: () => Layout;
  };
  const dagView = () =>
    (kit as unknown as { dagView: (c: HTMLElement, o: Record<string, unknown>) => View }).dagView;

  it("re-renders only the nodes whose data changed, and keeps the rest", () => {
    const host = document.createElement("div");
    document.body.append(host);
    const renders: string[] = [];
    const view = dagView()(host, {
      renderNode: (n: { id: string; data: { label: string } }) => {
        renders.push(n.id);
        return kit.h("div", { class: "cs-dag-card", text: n.data.label });
      },
    });
    const edges = [{ from: "a", to: "b" }];
    view.update(
      [
        { id: "a", label: "A" },
        { id: "b", label: "B" },
      ],
      edges,
    );
    const first = host.querySelector('[data-node="a"]');
    expect(renders).toEqual(["a", "b"]);
    view.update(
      [
        { id: "a", label: "A" },
        { id: "b", label: "B2" },
      ],
      edges,
    );
    expect(renders).toEqual(["a", "b", "b"]);
    expect(host.querySelector('[data-node="a"]')).toBe(first);
    expect(host.querySelector('[data-node="b"]')!.textContent).toBe("B2");
    view.update([{ id: "a", label: "A" }], []);
    expect(host.querySelector('[data-node="b"]')).toBeNull();
    host.remove();
  });

  it("selecting a node lights its upstream and downstream and dims the rest", () => {
    const host = document.createElement("div");
    document.body.append(host);
    const view = dagView()(host, {});
    view.update(
      ["a", "b", "c", "x"],
      [
        { from: "a", to: "b" },
        { from: "b", to: "c" },
      ],
    );
    view.select("b");
    const cls = (id: string) => host.querySelector(`[data-node="${id}"]`)!.className;
    expect(cls("b")).toContain("is-selected");
    expect(cls("a")).toContain("is-upstream");
    expect(cls("c")).toContain("is-downstream");
    expect(cls("x")).toContain("is-dim");
    expect(host.querySelectorAll("path.cs-dag-edge.is-active")).toHaveLength(2);
    view.select(null);
    expect(cls("x")).not.toContain("is-dim");
    host.remove();
  });
});

describe("kit.keyed and kit.skeleton", () => {
  const keyed = () =>
    (
      kit as unknown as {
        keyed: (c: HTMLElement, items: unknown[], o: Record<string, unknown>) => void;
      }
    ).keyed;

  it("reuses an unchanged item's element and reorders by key", () => {
    const box = document.createElement("div");
    let made = 0;
    const opts = {
      key: (x: { id: string }) => x.id,
      render: (x: { id: string; v: number }) => {
        made++;
        return kit.h("div", { text: `${x.id}${x.v}` });
      },
    };
    keyed()(
      box,
      [
        { id: "a", v: 1 },
        { id: "b", v: 1 },
      ],
      opts,
    );
    const a = box.firstChild;
    keyed()(
      box,
      [
        { id: "b", v: 2 },
        { id: "a", v: 1 },
      ],
      opts,
    );
    expect(made).toBe(3);
    expect(box.lastChild).toBe(a);
    expect(box.textContent).toBe("b2a1");
    keyed()(box, [], opts);
    expect(box.childNodes).toHaveLength(0);
  });

  it("draws loading rows", () => {
    const sk = (kit as unknown as { skeleton: (o: unknown) => HTMLElement }).skeleton({
      rows: 3,
      avatar: true,
    });
    expect(sk.getAttribute("aria-busy")).toBe("true");
    expect(sk.querySelectorAll(".cs-skeleton-row")).toHaveLength(3);
  });
});

describe("kit palettes match the app", () => {
  it("agent states use the app's status-mark colours and words", () => {
    for (const [state, token] of [
      ["running", "--cs-state-running"],
      ["idle", "--cs-state-idle"],
      ["needs-you", "--cs-state-needs-you"],
      ["exited", "--cs-state-exited"],
    ] as const) {
      expect(kit.agentStates[state].color).toBe(`var(${token})`);
    }
    // The app's status tokens, light and dark (viewer-themes.css).
    expect(TOKENS_CSS).toContain("--cs-state-running: light-dark(#216d84, #4aebeb);");
    expect(TOKENS_CSS).toContain("--cs-state-needs-you: light-dark(#b36b10, #fcaa2b);");
    expect(TOKENS_CSS).toContain("--cs-state-idle: light-dark(#7d8287, #aab0b7);");
    expect(kit.agentStates.running.label).toBe(AGENT_STATUS_WORDS.running);
    expect(kit.agentStates.idle.label).toBe(AGENT_STATUS_WORDS.idle);
    expect(kit.agentStates.exited.label).toBe(AGENT_STATUS_WORDS.done);
    expect(KIT_CSS).not.toMatch(/cs-pulse|opacity: 0\.6/);
    expect(Object.keys(kit.agentStates).sort()).toEqual(
      ["exited", "idle", "needs-you", "offline", "running", "unknown"].sort(),
    );
  });

  it("story statuses use the pipeline's colours", () => {
    for (const [id, cfg] of Object.entries(STATUS_CONFIG)) {
      expect(kit.storyStatuses[id]).toMatchObject({ color: cfg.color, label: cfg.label });
    }
  });

  it("the stylesheet's tokens follow the theme-aware status contract", () => {
    for (const [state, cfg] of Object.entries(kit.agentStates)) {
      expect(cfg.color).toBe(`var(--cs-state-${state})`);
      expect(TOKENS_CSS).toMatch(
        new RegExp(`--cs-state-${state}: light-dark\\(#[0-9a-f]{6}, #[0-9a-f]{6}\\);`),
      );
    }
    for (const id of Object.keys(kit.storyStatuses)) {
      expect(TOKENS_CSS).toMatch(
        new RegExp(`--cs-story-${id}: light-dark\\(#[0-9a-f]{6}, #[0-9a-f]{6}\\);`),
      );
      expect(TOKENS_CSS).not.toContain(`--cs-status-${id}`);
    }
  });
});

describe("kit icons, status and avatars", () => {
  it("ships the app's Lucide glyphs, regenerated from lucide-react", async () => {
    const icons = await loadIcons();
    expect(KIT_JS).toContain(renderBlock(icons));
    expect(kit.iconNames()).toEqual(Object.keys(icons).sort());
  });

  it("draws an icon as an inline svg; an unknown name is marked", () => {
    const svg = kit.icon("bot", { size: 20 });
    expect(svg.tagName.toLowerCase()).toBe("svg");
    expect(svg.getAttribute("width")).toBe("20");
    expect(svg.querySelector("path, rect")).not.toBeNull();
    expect(kit.icon("no-such-icon").getAttribute("data-missing")).toBe("no-such-icon");
  });

  it("status dot and badge carry the state and its label", () => {
    const dot = kit.statusDot("needs-you");
    expect(dot.dataset.state).toBe("needs-you");
    expect(dot.getAttribute("aria-label")).toBe("Needs you");
    const badge = kit.statusBadge("running");
    expect(badge.textContent).toBe("Working");
    expect(badge.querySelector("svg")).not.toBeNull();
    expect(kit.agentState("weird").label).toBe("weird");
  });

  it("an avatar draws a shipped face, else initials", () => {
    const face = kit.avatar({ face: "builtin-funny:coffee-bot", name: "Coffee" });
    expect(face.querySelector("img")!.getAttribute("src")).toBe(
      "/kit/v1/avatars/builtin-funny/coffee-bot.svg",
    );
    expect(kit.avatar({ name: "release captain" }).textContent).toBe("RC");
    expect(kit.avatar({ face: "team-pack:x", name: "solo" }).textContent).toBe("SO");
  });

  it("h() builds elements from text, never parsing HTML", () => {
    const el = kit.h("div", { class: "cs-card", text: "<b>x</b>" });
    expect(el.innerHTML).toBe("&lt;b&gt;x&lt;/b&gt;");
    expect(kit.h("p", null, "a", null, ["b", kit.h("i", { text: "c" })]).textContent).toBe("abc");
  });

  it("timeAgo reads epoch ms and ISO strings", () => {
    const now = Date.parse("2026-10-04T12:00:00Z");
    expect(kit.timeAgo(now - 30_000, now)).toBe("30s ago");
    expect(kit.timeAgo("2026-10-04T10:00:00Z", now)).toBe("2h ago");
    expect(kit.timeAgo("nope", now)).toBe("");
  });
});

describe("kit.virtualList", () => {
  it("passes a records collection to subscribe and unsubscribe", () => {
    const subscribe = vi.fn(async () => ({ topic: "records", snapshot: [], total: 0 }));
    const unsubscribe = vi.fn();
    loadKit({ subscribe, unsubscribe });
    new Function(VIRTUAL_LIST_JS)();
    const kit = (window as unknown as { codesense: { kit: Record<string, any> } }).codesense.kit;
    const container = document.createElement("div");
    const list = kit.virtualList({
      container,
      topic: "records",
      collection: "notes",
      rowHeight: 20,
      render: () => document.createElement("div"),
    });
    expect(subscribe).toHaveBeenCalledWith(
      "records",
      expect.objectContaining({ collection: "notes", window: [0, expect.any(Number)] }),
    );
    list.destroy();
    expect(unsubscribe).toHaveBeenCalledWith("records", expect.any(Function), {
      collection: "notes",
    });
  });
});

describe("kit.has", () => {
  it("delegates to codesense.has, not to which helpers exist", () => {
    const k = loadKit({ has: (name: string) => name === "sessions", stopSession: async () => {} });
    expect(k.has("sessions")).toBe(true);
    expect(k.has("stopSession")).toBe(false);
    expect(loadKit({ routines: async () => [] }).has("routines")).toBe(false);
  });
});

describe("kit.call", () => {
  it("uses a named helper when the app has one", async () => {
    const k = loadKit({ routines: async () => ["r1"] });
    await expect(k.call("routines")).resolves.toEqual(["r1"]);
  });

  it("falls back to invoke, and marks a call this app lacks as unavailable", async () => {
    const k = loadKit({
      invoke: async (verb: string) => {
        throw new Error(`${verb} is not allowlisted`);
      },
    });
    const err = await k.call("machines").catch((e: Error & { unavailable?: boolean }) => e);
    expect((err as Error & { unavailable?: boolean }).unavailable).toBe(true);
  });

  it("does not mark an ordinary failure as unavailable", async () => {
    const k = loadKit({
      runs: async () => {
        throw new Error("busy");
      },
    });
    const err = await k.call("runs").catch((e: Error & { unavailable?: boolean }) => e);
    expect((err as Error & { unavailable?: boolean }).unavailable).toBeUndefined();
  });
});

describe("kit.live", () => {
  it("gates polls and keeps saved-agent notifications alongside sessions", async () => {
    vi.useFakeTimers();
    const visibility = new Set<(shown: boolean) => void>();
    const render = vi.fn();
    const subscribe = vi.fn(async () => ({}));
    const unsubscribe = vi.fn();
    const liveKit = loadKit({
      subscribe,
      unsubscribe,
      onVisibility: (cb: (shown: boolean) => void) => {
        visibility.add(cb);
        return () => visibility.delete(cb);
      },
    });
    const stop = liveKit.live(render, {
      events: ["sessions-changed", "agents-changed"],
      pollMs: 100,
    });
    try {
      await vi.advanceTimersByTimeAsync(300);
      expect(render).not.toHaveBeenCalled();
      visibility.forEach((cb) => cb(true));
      await vi.advanceTimersByTimeAsync(1);
      expect(subscribe).toHaveBeenCalledWith("sessions", expect.anything());
      render.mockClear();
      window.dispatchEvent(
        new MessageEvent("message", {
          source: window.parent,
          data: { type: "codesense:agents-changed" },
        }),
      );
      await vi.advanceTimersByTimeAsync(1);
      expect(render).toHaveBeenCalledOnce();
      visibility.forEach((cb) => cb(false));
      render.mockClear();
      await vi.advanceTimersByTimeAsync(300);
      expect(render).not.toHaveBeenCalled();
    } finally {
      stop();
      vi.useRealTimers();
    }
  });
});

// ── SV-ENG-63 paging helpers ─────────────────────────────────────────────

function ks(id: string, lastAtMs?: number) {
  return { sessionId: id, lastAtMs: lastAtMs ?? 1, tokens: { input: 1, output: 0 } };
}
function kc(id: string) {
  return { id, path: `/x/${id}.md` };
}
function kTotals() {
  return { costUsd: 1.5, tokens: { input: 10, output: 5 } };
}
function kitIds(rows: unknown[]) {
  return rows.map((r: any) => r.sessionId || r.id || r.path).sort();
}

function mockKitQueries() {
  const calls: { verb: string; args: any }[] = [];
  const subs: { args: any; cb: (result: any, error?: any) => void; off: () => void }[] = [];
  const pending: { resolve: (value: any) => void; reject: (err: any) => void }[] = [];
  const cs = {
    sessionUsage: (args: any) => {
      calls.push({ verb: "sessionUsage", args });
      return new Promise((res, rej) => pending.push({ resolve: res, reject: rej }));
    },
    queryContext: (args: any) => {
      calls.push({ verb: "queryContext", args });
      return new Promise((res, rej) => pending.push({ resolve: res, reject: rej }));
    },
    subscribeContext: (args: any, cb: (result: any, error?: any) => void) => {
      const off = vi.fn();
      subs.push({ args, cb, off });
      return off;
    },
  };
  function resolve(v: any) {
    const p = pending.shift();
    if (p) p.resolve(v);
  }
  function reject(e: any) {
    const p = pending.shift();
    if (p) p.reject(e);
  }
  function tick(result: any, error?: any) {
    subs.forEach((s) => s.cb(result, error));
  }
  function lastCall() {
    return calls[calls.length - 1];
  }
  return { cs, calls, subs, resolve, reject, tick, lastCall, pending };
}

describe("kit.queryAll", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("walks sessionUsage pages and preserves totals/days", async () => {
    const { cs, resolve, lastCall } = mockKitQueries();
    const k = loadKit(cs);
    const totals = kTotals();
    const days = [{ day: "2026-10-25" }];
    const p = k.queryAll("sessionUsage", { sinceMs: 0 }, { limit: 150 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({
      sessions: Array.from({ length: 150 }, (_, i) => ks(`s${i}`, 1000 - i)),
      total: 201,
      nextCursor: "c1",
      totals,
      days,
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(lastCall().args.cursor).toBe("c1");
    resolve({
      sessions: Array.from({ length: 51 }, (_, i) => ks(`s${150 + i}`, 500 - i)),
      total: 201,
    });
    const r = await p;
    expect(r.rows).toHaveLength(201);
    expect(r.complete).toBe(true);
    expect(r.totals).toEqual(totals);
    expect(r.days).toEqual(days);
  });

  it("walks queryContext rows", async () => {
    const { cs, resolve } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("queryContext", { text: "x" }, { limit: 2 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("a"), kc("b")], total: 4, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("c"), kc("d")], total: 4 });
    const r = await p;
    expect(kitIds(r.rows)).toEqual(["a", "b", "c", "d"]);
    expect(r.complete).toBe(true);
  });

  it("forwards original args and effective limit on every page", async () => {
    const { cs, resolve, calls } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("queryContext", { text: "x", where: "y" }, { limit: 3 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("a")], total: 2, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("b")], total: 2 });
    await p;
    expect(calls[0].args).toEqual(expect.objectContaining({ text: "x", where: "y", limit: 3 }));
    expect(calls[1].args).toEqual(
      expect.objectContaining({ text: "x", where: "y", limit: 3, cursor: "c1" }),
    );
  });

  it("rejects on ordinary errors", async () => {
    const { cs, reject } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {});
    await vi.advanceTimersByTimeAsync(0);
    reject(new Error("not_allowed"));
    await expect(p).rejects.toThrow("not_allowed");
  });

  it("backs off on rate_limited", async () => {
    const { cs, resolve, reject } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {});
    await vi.advanceTimersByTimeAsync(0);
    reject(new Error("rate_limited"));
    await vi.advanceTimersByTimeAsync(5000);
    resolve({ sessions: [ks("x")], total: 1 });
    const r = await p;
    expect(r.rows).toHaveLength(1);
  });

  it("gives up after six rate_limited retries", async () => {
    const { cs, reject, calls } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {}, { limit: 1 });
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 7; i++) {
      reject(new Error("rate_limited"));
      if (i < 6) await vi.advanceTimersByTimeAsync(5000 * (i + 1));
    }
    await expect(p).rejects.toThrow("rate_limited");
    expect(calls).toHaveLength(7);
  });

  it("restarts once on stale continuation then rejects on repeat", async () => {
    const { cs, resolve, reject, calls } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {}, { limit: 1 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("a")], total: 2, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    reject(new Error("invalid cursor"));
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(3);
    reject(new Error("invalid cursor"));
    await expect(p).rejects.toThrow("invalid cursor");
  });

  it("SV-PBF-287 H2: kit.storyTotals asks the bridge 500 ids at a time and merges one answer", async () => {
    const asked: string[][] = [];
    const cs = {
      storyTotals: (args: { storyIds: string[] }) => {
        asked.push(args.storyIds);
        const n = asked.length;
        return Promise.resolve({
          stories: Object.fromEntries(
            args.storyIds.map((id) => [
              id,
              { totals: { sessions: 1 }, unavailable: { costUsd: 0 } },
            ]),
          ),
          reasons: [`R${n}`],
          pending: [],
          indexing: { done: 600, total: 600 },
        });
      },
    };
    const k = loadKit(cs);
    const ids = Array.from({ length: 600 }, (_, i) => `SV-${i}`);
    type Merged = {
      stories: Record<string, { unavailable?: Record<string, number> }>;
      reasons: string[];
      indexing: unknown;
    };
    const merged = await (
      k as unknown as { storyTotals: (ids: string[]) => Promise<Merged> }
    ).storyTotals(ids);
    expect(asked.map((part) => part.length)).toEqual([500, 100]);
    expect(Object.keys(merged.stories)).toHaveLength(600);
    expect(merged.reasons).toEqual(["R1", "R2"]);
    expect(merged.stories["SV-0"].unavailable).toEqual({ costUsd: 0 });
    expect(merged.stories["SV-599"].unavailable).toEqual({ costUsd: 1 });
    expect(merged.indexing).toEqual({ done: 600, total: 600 });
  });

  it("SV-PBF-287: a busy read is asked again after 1 s, then 2 s, and the walk goes on", async () => {
    const { cs, resolve, reject, calls } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("queryContext", {}, { limit: 2 });
    await vi.advanceTimersByTimeAsync(0);
    reject(new Error("busy"));
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1250);
    expect(calls).toHaveLength(2);
    reject(new Error("mac_busy"));
    await vi.advanceTimersByTimeAsync(2250);
    expect(calls).toHaveLength(3);
    resolve({ rows: [kc("a")], total: 1 });
    const r = await p;
    expect(kitIds(r.rows)).toEqual(["a"]);
    expect(r.error).toBeNull();
  });

  it("SV-PBF-287: walks again from the head on the bridge's stale_cursor, silently, keeping only the new walk's rows", async () => {
    const { cs, resolve, reject, calls } = mockKitQueries();
    const k = loadKit(cs);
    const stale = () =>
      new Error(
        "stale_cursor: results changed since this page was fetched; restart paging without a cursor",
      );
    const p = k.queryAll("queryContext", {}, { limit: 2 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("gone"), kc("a")], total: 3, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 3; i++) {
      reject(stale());
      await vi.advanceTimersByTimeAsync(0);
      expect(calls.at(-1)?.args.cursor).toBeUndefined();
      resolve({ rows: [kc("a"), kc("b")], total: 3, nextCursor: `r${i}` });
      await vi.advanceTimersByTimeAsync(0);
      expect(calls.at(-1)?.args).toMatchObject({ cursor: `r${i}`, limit: 2 });
    }
    resolve({ rows: [kc("c")], total: 3 });
    const r = await p;
    expect(kitIds(r.rows)).toEqual(["a", "b", "c"]);
    expect(r.complete).toBe(true);
    expect(r.error).toBeNull();

    const again = k.queryAll("queryContext", {}, { limit: 1 });
    const rejected = expect(again).rejects.toThrow(/^stale_cursor/);
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("a")], total: 2, nextCursor: "c0" });
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 3; i++) {
      reject(stale());
      await vi.advanceTimersByTimeAsync(0);
      resolve({ rows: [kc("a")], total: 2, nextCursor: `s${i}` });
      await vi.advanceTimersByTimeAsync(0);
    }
    reject(stale());
    await rejected;
  });

  it("rejects a second invalid continuation after the cursorless restart succeeds", async () => {
    const { cs, resolve, reject, calls } = mockKitQueries();
    const k = loadKit(cs);
    const pending = k.queryAll("sessionUsage", {}, { limit: 1 });
    const rejected = expect(pending).rejects.toThrow("query_invalid");
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("a", 10)], total: 3, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    expect(calls.at(-1)?.args).toMatchObject({ cursor: "c1", limit: 1 });
    reject(new Error("query_invalid"));
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(3);
    expect(calls.at(-1)?.args.cursor).toBeUndefined();
    resolve({ sessions: [ks("a", 11)], total: 3, nextCursor: "c2" });
    await vi.advanceTimersByTimeAsync(0);
    expect(calls.at(-1)?.args).toMatchObject({ cursor: "c2", limit: 1 });
    reject(new Error("query_invalid"));
    await rejected;
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(4);
  });

  it("recovers moved rows by union across recovery rewalks", async () => {
    const { cs, resolve } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {}, { limit: 1 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("A", 30)], total: 3, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("B", 20)], total: 3, nextCursor: "c2" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [], total: 3 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("A", 30)], total: 3, nextCursor: "c1b" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("C", 25)], total: 3, nextCursor: "c2b" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("B", 20)], total: 3 });
    const r = await p;
    expect(kitIds(r.rows)).toEqual(["A", "B", "C"]);
    expect(r.complete).toBe(true);
  });

  it("stops at maxRows", async () => {
    const { cs, resolve } = mockKitQueries();
    const k = loadKit(cs);
    const p = k.queryAll("sessionUsage", {}, { limit: 1, maxRows: 2 });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("a")], total: 5, nextCursor: "1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("b")], total: 5, nextCursor: "2" });
    const r = await p;
    expect(r.rows).toHaveLength(2);
    expect(r.truncated).toBe(true);
  });
});

describe("kit.subscribeAll (queryContext)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("does an initial full walk via subscribeContext + queryContext continuation", async () => {
    const { cs, resolve, tick, subs } = mockKitQueries();
    const k = loadKit(cs);
    const results: Result[] = [];
    k.subscribeAll("queryContext", { text: "x" }, 15, (r) => results.push(r));
    await vi.advanceTimersByTimeAsync(0);
    expect(subs).toHaveLength(1);
    tick({ rows: [kc("a")], total: 2, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("b")], total: 2 });
    await vi.advanceTimersByTimeAsync(0);
    const last = results[results.length - 1];
    expect(kitIds(last.rows)).toEqual(["a", "b"]);
    expect(last.complete).toBe(true);
    // Original args merged with page args.
    expect(subs[0].off).not.toHaveBeenCalled();
  });

  it("coalesces the newest tick while a continuation is pending", async () => {
    const { cs, resolve, tick, pending } = mockKitQueries();
    const k = loadKit(cs);
    const results: Result[] = [];
    k.subscribeAll("queryContext", {}, 15, (r) => results.push(r));
    await vi.advanceTimersByTimeAsync(0);
    tick({ rows: [kc("a")], total: 3, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    // First continuation answers with progress; a second request is now pending.
    resolve({ rows: [kc("b")], total: 3, nextCursor: "c2" });
    await vi.advanceTimersByTimeAsync(0);
    expect(pending).toHaveLength(1);
    // New tick arrives while the second continuation is pending.
    tick({ rows: [kc("a2")], total: 3, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    // A head cannot prove deletion. Keep a until a permitted fresh traversal.
    resolve({ rows: [kc("c")], total: 3 });
    await vi.advanceTimersByTimeAsync(0);
    expect(kitIds(results[results.length - 1].rows)).toContain("a");
    await vi.advanceTimersByTimeAsync(60000);
    tick({ rows: [kc("a2")], total: 3, nextCursor: "new-c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("a2")], total: 3, nextCursor: "new-c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("b")], total: 3, nextCursor: "new-c2" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("c")], total: 3 });
    await vi.advanceTimersByTimeAsync(0);
    const last = results[results.length - 1];
    expect(kitIds(last.rows).sort()).toEqual(["a2", "b", "c"]);
    expect(last.complete).toBe(true);
  });

  it("reconciles membership replacement on a full refresh", async () => {
    const { cs, resolve, tick } = mockKitQueries();
    const k = loadKit(cs);
    const results: Result[] = [];
    k.subscribeAll("queryContext", {}, 15, (r) => results.push(r), { limit: 2, restEveryS: 60 });
    await vi.advanceTimersByTimeAsync(0);
    tick({ rows: [kc("a"), kc("b")], total: 4, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("c"), kc("d")], total: 4 });
    await vi.advanceTimersByTimeAsync(0);
    expect(kitIds(results[results.length - 1].rows).sort()).toEqual(["a", "b", "c", "d"]);

    await vi.advanceTimersByTimeAsync(60000);
    tick({ rows: [kc("e"), kc("f")], total: 4, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("g"), kc("h")], total: 4 });
    await vi.advanceTimersByTimeAsync(0);
    const last = results[results.length - 1];
    expect(kitIds(last.rows).sort()).toEqual(["e", "f", "g", "h"]);
  });

  it("reconciles terminal shrink when page 1 loses its cursor", async () => {
    const { cs, resolve, tick } = mockKitQueries();
    const k = loadKit(cs);
    const results: Result[] = [];
    k.subscribeAll("queryContext", {}, 15, (r) => results.push(r), { limit: 2, restEveryS: 60 });
    await vi.advanceTimersByTimeAsync(0);
    tick({ rows: [kc("a"), kc("b")], total: 4, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ rows: [kc("c"), kc("d")], total: 4 });
    await vi.advanceTimersByTimeAsync(0);

    await vi.advanceTimersByTimeAsync(60000);
    tick({ rows: [kc("e"), kc("f")], total: 2 });
    await vi.advanceTimersByTimeAsync(0);
    const last = results[results.length - 1];
    expect(kitIds(last.rows).sort()).toEqual(["e", "f"]);
    expect(last.complete).toBe(true);
  });

  it("stop calls subscribeContext off and suppresses late results", async () => {
    const { cs, resolve, tick, subs } = mockKitQueries();
    const k = loadKit(cs);
    const results: Result[] = [];
    const stop = k.subscribeAll("queryContext", {}, 15, (r) => results.push(r), { limit: 1 });
    await vi.advanceTimersByTimeAsync(0);
    tick({ rows: [kc("a")], total: 2, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    const callsBeforeStop = results.length;
    stop();
    expect(subs[0].off).toHaveBeenCalled();
    // Late answers suppressed by stop flag and cleared timers.
    resolve({ rows: [kc("b")], total: 2 });
    await vi.advanceTimersByTimeAsync(0);
    expect(results.length).toBe(callsBeforeStop);
  });
});

describe("kit.subscribeAll (sessionUsage)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("uses kit.live visibility/poll lifecycle and real sessionUsage calls", async () => {
    const visibility = new Set<(shown: boolean) => void>();
    const { cs, resolve, calls } = mockKitQueries();
    const liveKit = loadKit({
      ...cs,
      subscribe: vi.fn(async () => ({})),
      unsubscribe: vi.fn(),
      onVisibility: (cb: (shown: boolean) => void) => {
        visibility.add(cb);
        return () => visibility.delete(cb);
      },
    });
    const results: Result[] = [];
    liveKit.subscribeAll("sessionUsage", { sinceMs: 0 }, 15, (r) => results.push(r), {
      limit: 1,
      restEveryS: 60,
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(0);
    visibility.forEach((cb) => cb(true));
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    expect(calls[0].args).toEqual(expect.objectContaining({ sinceMs: 0, limit: 1 }));
    resolve({ sessions: [ks("a")], total: 2, nextCursor: "c1" });
    await vi.advanceTimersByTimeAsync(0);
    resolve({ sessions: [ks("b")], total: 2 });
    await vi.advanceTimersByTimeAsync(0);
    expect(kitIds(results[results.length - 1].rows)).toEqual(["a", "b"]);

    // 60s poll fires a new refresh while visible.
    await vi.advanceTimersByTimeAsync(60000);
    expect(calls.length).toBeGreaterThanOrEqual(3);
  });
});

describe("kit.partialList", () => {
  it("renders approved status states", () => {
    const k = loadKit();
    const container = document.createElement("div");
    let cb: (r: Result) => void = () => {};
    const list = k.partialList({
      container,
      subscribe: (fn: (r: Result) => void) => {
        cb = fn;
        return () => {};
      },
      render: (row: any) => {
        const el = document.createElement("div");
        el.textContent = row.id;
        return el;
      },
    });
    cb({
      rows: [{ id: "a" }, { id: "b" }],
      total: 5,
      complete: false,
      truncated: true,
      returned: 2,
      loading: false,
    });
    expect(container.querySelector(".cs-partial-list-text")?.textContent).toBe("Showing 2 of 5");
    cb({
      rows: [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }, { id: "e" }],
      total: 5,
      complete: true,
      truncated: false,
      returned: 5,
      loading: false,
    });
    expect(container.querySelector(".cs-partial-list-text")?.textContent).toBe("Showing all 5");
    list.stop();
  });
});

// Exercise the copied lifecycle through the existing bridge shim, including
// the usage head's actual live visibility/session notifications.
describe.each(["sessionUsage", "queryContext"])("kit paging parity: %s", (name) => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  const drain = () => vi.advanceTimersByTimeAsync(0);
  const row = (id: string, value = 1) =>
    name === "sessionUsage"
      ? { cli: "codex", sessionId: id, machineId: "mac", value }
      : { path: id, value };
  const page = (rows: unknown[], total?: number, nextCursor?: string, cost = 1) => ({
    [name === "sessionUsage" ? "sessions" : "rows"]: rows,
    ...(total === undefined ? {} : { total }),
    nextCursor,
    totals: { cost },
    days: [{ cost }],
  });
  function fixture(opts: Record<string, unknown> = {}, args: Record<string, unknown> = {}) {
    const h = mockKitQueries();
    const visibility = new Set<(shown: boolean) => void>();
    let changed = () => {};
    let shown = false;
    const unsubscribe = vi.fn();
    const k = loadKit({
      ...h.cs,
      subscribe: vi.fn(async (_kind: string, handlers: { onChange: () => void }) => {
        changed = handlers.onChange;
      }),
      unsubscribe,
      onVisibility: (cb: (shown: boolean) => void) => {
        visibility.add(cb);
        return () => visibility.delete(cb);
      },
    });
    const results: Result[] = [];
    const stop = k.subscribeAll(name, args, 15, (r) => results.push(r), opts);
    async function head(value: unknown, error?: Error) {
      if (name === "queryContext") h.tick(value, error);
      else {
        if (!shown) {
          shown = true;
          visibility.forEach((cb) => cb(true));
        } else changed();
        await drain();
        // A head may arrive while an explicit continuation is pending. Resolve
        // the head transport itself, leaving the older page pending.
        const request = h.pending.pop();
        expect(request).toBeDefined();
        if (error) request!.reject(error);
        else request!.resolve(value);
      }
      await drain();
    }
    return { ...h, k, head, results, stop, visibility, unsubscribe };
  }

  function reviewFixture(opts: Record<string, unknown>) {
    const h = fixture(opts);
    let headCalls = 0;
    return {
      ...h,
      hosted: false,
      cleanup: () => {},
      head: async (answer: unknown, error?: Error, _outer?: Record<string, unknown>) => {
        if (name === "sessionUsage") headCalls++;
        await h.head(answer, error);
      },
      reply: h.resolve,
      fail: (error: string) => h.reject(new Error(error)),
      count: () => h.calls.length - headCalls,
      args: () => h.lastCall().args as Record<string, unknown>,
    };
  }

  it("prepares queued reconciliation before an internal stale restart and skips an already complete head", async () => {
    for (const terminal of [false, true]) {
      const h = reviewFixture({ limit: 1 });
      try {
        await h.head(page([row("a")], 2, "old", 1));
        expect(h.count()).toBe(1);
        await h.head(terminal ? page([], 0, undefined, 0) : page([row("a", 9)], 3, "queued", 9));
        h.fail("query_invalid");
        await drain();
        if (terminal) {
          expect(h.results.at(-1)).toMatchObject({
            rows: [],
            returned: 0,
            total: 0,
            complete: true,
            canLoadMore: false,
            loading: false,
            totals: { cost: 0 },
            days: [{ cost: 0 }],
          });
          expect(h.count()).toBe(1);
          continue;
        }
        expect(h.count()).toBe(2);
        expect(h.args()).toMatchObject({ limit: 1, cursor: "queued" });
        h.reply(page([row("a", 11), row("b"), row("c")], 4, "new-tail", 11));
        await drain();
        expect(h.results.at(-1)).toMatchObject({
          returned: 3,
          total: 4,
          totals: { cost: 11 },
          days: [{ cost: 11 }],
        });
        expect(h.results.at(-1)?.rows).toContainEqual(row("a", 11));
        expect(h.count()).toBe(3);
        expect(h.args()).toMatchObject({ limit: 1, cursor: "new-tail" });
        h.reply(page([row("d")], 4, undefined, 11));
        await drain();
        expect(h.results.at(-1)).toMatchObject({
          returned: 4,
          total: 4,
          complete: true,
          canLoadMore: false,
          loading: false,
          totals: { cost: 11 },
          days: [{ cost: 11 }],
        });
        expect(h.results.at(-1)?.rows).toContainEqual(row("a", 11));
        await drain();
        expect(h.count()).toBe(3);
      } finally {
        h.stop();
        h.cleanup();
      }
    }
  });

  it("reconciles a queued head before an ordinary failure retry accepts newer data", async () => {
    const h = reviewFixture({ limit: 1 });
    try {
      await h.head(page([row("a")], 2, "old", 1));
      await h.head(page([row("a", 9)], 3, "queued", 9));
      h.fail("offline");
      await drain();
      expect(h.results.at(-1)?.error?.message).toBe("offline");
      const retry = h.stop.loadMore();
      expect(h.args().cursor).toBe("queued");
      h.reply(page([row("a", 11), row("b"), row("c")], 4, "new-tail", 11));
      const result = await retry;
      expect(result).toMatchObject({
        returned: 3,
        total: 4,
        complete: false,
        canLoadMore: true,
        totals: { cost: 11 },
        days: [{ cost: 11 }],
        error: null,
      });
      expect(result.rows).toContainEqual(row("a", 11));
      const count = h.count();
      await drain();
      expect(h.count()).toBe(count);
      const last = h.stop.loadMore();
      expect(h.args().cursor).toBe("new-tail");
      h.reply(page([row("d")], 4, undefined, 11));
      expect(await last).toMatchObject({
        returned: 4,
        total: 4,
        complete: true,
        canLoadMore: false,
        totals: { cost: 11 },
        days: [{ cost: 11 }],
      });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("retains a head warning through page success, next page failure and retry until an identical successful head", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      const initial = page([row("a")], 4, "c1", 1);
      await h.head(initial, undefined, { source: "live", answered_at: 100, server_time: 101 });
      const next = h.stop.loadMore();
      await h.head(null, new Error("rate_limited"));
      expect(h.results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      h.reply(page([row("b")], 4, "c2", 2));
      expect(await next).toMatchObject({
        returned: 2,
        totals: { cost: 2 },
        error: { message: "rate_limited" },
      });
      const failed = h.stop.loadMore();
      expect(h.results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      const rejection = expect(failed).rejects.toThrow("offline");
      h.fail("offline");
      await rejection;
      expect(h.results.at(-1)?.error?.message).toBe("offline");
      const retry = h.stop.loadMore();
      expect(h.args().cursor).toBe("c2");
      expect(h.results.at(-1)?.error?.message).toBe("rate_limited");
      h.reply(page([row("c"), row("d")], 4, undefined, 4));
      expect(await retry).toMatchObject({
        complete: true,
        returned: 4,
        totals: { cost: 4 },
        days: [{ cost: 4 }],
        error: { message: "rate_limited" },
      });
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "live",
          answered_at: 100,
          server_time: 101,
        });
      const count = h.count();
      await h.head(initial, undefined, { source: "cache", answered_at: 900, server_time: 901 });
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 4,
        totals: { cost: 4 },
        error: null,
      });
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "cache",
          answered_at: 900,
          server_time: 901,
        });
      expect(h.count()).toBe(count);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("enforces the public hard 5000-row ceiling with a larger requested cap and ordinary pages", async () => {
    const h = reviewFixture({ manual: true, limit: 100, maxRows: 9000 });
    const rows = Array.from({ length: 5101 }, (_, i) => row(String(i)));
    try {
      await h.head(page(rows.slice(0, 100), rows.length, "100"));
      for (let offset = 100; offset < 5000; offset += 100) {
        const next = h.stop.loadMore();
        expect(h.args()).toMatchObject({ limit: 100, cursor: String(offset) });
        h.reply(page(rows.slice(offset, offset + 100), rows.length, String(offset + 100)));
        await next;
      }
      const result = h.results.at(-1)!;
      expect(result).toMatchObject({
        returned: 5000,
        total: 5101,
        atCeiling: true,
        complete: false,
        truncated: true,
        canLoadMore: false,
      });
      expect(result.rows).toEqual(rows.slice(0, 5000));
      expect(new Set(result.rows.map((r) => JSON.stringify(r))).size).toBe(5000);
      expect(h.count()).toBe(49);
      await h.stop.loadMore();
      await drain();
      expect(h.count()).toBe(49);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("completes a fenced final manual page with the newest rows and full summary", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      await h.head(page([row("a")], 2, "c1"));
      const final = h.stop.loadMore();
      await h.head(page([row("a", 9)], 2, "head", 9), undefined, {
        source: "cache",
        answered_at: 900,
        server_time: 901,
      });
      const count = h.count();
      h.reply(page([row("a", 2), row("b")], 99, undefined, 2));
      const result = await final;
      expect(result).toMatchObject({
        complete: true,
        returned: 2,
        total: 2,
        canLoadMore: false,
        totals: { cost: 9 },
        days: [{ cost: 9 }],
      });
      expect(result.rows).toContainEqual(row("a", 9));
      if (h.hosted)
        expect(result).toMatchObject({ source: "cache", answered_at: 900, server_time: 901 });
      await drain();
      expect(h.count()).toBe(count);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("keeps completed manual depth after rest and fully head-supplied growth", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      await h.head(page([row("a")], 2, "c1"));
      const final = h.stop.loadMore();
      h.reply(page([row("b")], 2));
      await final;
      vi.setSystemTime(Date.now() + 60000);
      const count = h.count();
      await h.head(page([row("a", 9)], 2, "head", 9));
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 2,
        canLoadMore: false,
        totals: { cost: 9 },
      });
      await h.head(page([row("a", 10), row("c")], 3, "growth", 10));
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        total: 3,
        canLoadMore: false,
        totals: { cost: 10 },
      });
      expect(h.count()).toBe(count);
      await h.head(page([row("a", 11)], 4, "missing", 11));
      expect(h.results.at(-1)).toMatchObject({ complete: false, returned: 3, canLoadMore: true });
      expect(h.count()).toBe(count);
      const next = h.stop.loadMore();
      expect(h.args().cursor).toBe("missing");
      h.reply(page([row("d")], 4, undefined, 11));
      expect(await next).toMatchObject({ complete: true, returned: 4 });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("coalesces the newest automatic head during a rate-limited pending continuation", async () => {
    const h = reviewFixture({ limit: 1 });
    try {
      await h.head(page([row("a")], 2, "old"));
      h.fail("rate_limited");
      await drain();
      await h.head(page([row("a", 5)], 3, "h1", 5));
      await h.head(page([row("a", 9)], 3, "h2", 9), undefined, {
        source: "cache",
        answered_at: 900,
        server_time: 901,
      });
      await vi.advanceTimersByTimeAsync(5000);
      expect(h.args().cursor).toBe("old");
      h.reply(page([row("a", 2), row("b")], 2, "old-tail", 2));
      await drain();
      expect(h.args().cursor).toBe("h2");
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "cache",
          answered_at: 900,
          server_time: 901,
        });
      expect(h.results.at(-1)).toMatchObject({
        total: 3,
        totals: { cost: 9 },
        days: [{ cost: 9 }],
      });
      expect(h.results.at(-1)?.rows).toContainEqual(row("a", 9));
      h.reply(page([row("b"), row("c")], 3, undefined, 9));
      await drain();
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        total: 3,
        returned: 3,
        totals: { cost: 9 },
      });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("waits for a successful head after each invalid manual cursor before explicit continuation", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      const old = page([row("a")], 2, "c1", 9);
      await h.head(old);
      for (let attempt = 0; attempt < 2; attempt++) {
        const next = h.stop.loadMore();
        const failed = expect(next).rejects.toThrow("query_invalid");
        h.fail("query_invalid");
        await failed;
        const count = h.count();
        expect(h.results.at(-1)).toMatchObject({
          returned: 1,
          total: 2,
          canLoadMore: false,
          waitingForHead: true,
          totals: { cost: 9 },
        });
        await h.stop.loadMore();
        await h.stop.refresh();
        expect(h.count()).toBe(count);
        await h.head(null, new Error("mac_offline"));
        await h.stop.refresh();
        expect(h.results.at(-1)).toMatchObject({ waitingForHead: true, canLoadMore: false });
        expect(h.count()).toBe(count);
        await h.head(old);
        expect(h.results.at(-1)).toMatchObject({
          waitingForHead: false,
          canLoadMore: true,
          error: null,
        });
        expect(h.count()).toBe(count);
      }
      const final = h.stop.loadMore();
      expect(h.args().cursor).toBe("c1");
      h.reply(page([row("b")], 2, undefined, 9));
      expect(await final).toMatchObject({ complete: true, returned: 2 });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("keeps newer continuation rows/summary on identical old heads and rearms only a known deficit", async () => {
    const h = fixture({ manual: true, limit: 1 });
    try {
      const old = page([row("a")], 300, "c1", 1);
      await h.head(old);
      const p = h.stop.loadMore();
      h.resolve(page([row("a", 9), row("b")], 301, undefined, 88.88));
      await p;
      await h.head(undefined, new Error("rate_limited"));
      expect(h.results.at(-1)?.error?.message).toBe("rate_limited");
      await h.head(old);
      expect(h.results.at(-1)?.error).toBeNull();
      expect(h.results.at(-1)).toMatchObject({
        total: 301,
        returned: 2,
        totals: { cost: 88.88 },
        days: [{ cost: 88.88 }],
        canLoadMore: true,
      });
      expect(h.results.at(-1)?.rows).toContainEqual(row("a", 9));
      const next = h.stop.loadMore();
      expect(h.lastCall().args.cursor).toBe("c1");
      h.resolve(page([row("c")], 301, "c2", 88.88));
      await next;
    } finally {
      h.stop();
    }
    for (const total of [undefined, 1]) {
      const h = fixture({ manual: true, limit: 1 });
      try {
        const old = page([row("a")], total, "c1");
        await h.head(old);
        const p = h.stop.loadMore();
        h.resolve(page([row("b")], total));
        await p;
        const before = h.calls.length;
        await h.head(old);
        expect(h.results.at(-1)).toMatchObject({ complete: false, canLoadMore: false });
        expect(h.calls.length).toBe(before + (name === "sessionUsage" ? 1 : 0));
      } finally {
        h.stop();
      }
    }
  });

  it("matches unknown, explicit zero, overshoot and latest summary semantics", async () => {
    const h = mockKitQueries();
    const k = loadKit(h.cs);
    const unknown = k.queryAll(name, {});
    h.resolve(page([row("a")]));
    expect(await unknown).toMatchObject({ total: null, complete: false, returned: 1 });
    const zero = k.queryAll(name, {});
    h.resolve(page([], 0));
    expect(await zero).toMatchObject({ complete: true, returned: 0, total: 0 });
    const p = k.queryAll(name, {}, { maxRows: 2 });
    h.resolve(page([row("a")], 3, "c", 1));
    await drain();
    h.resolve(page([row("b")], 1, undefined, 9));
    expect(await p).toMatchObject({
      returned: 2,
      total: 1,
      complete: false,
      totals: { cost: 9 },
      days: [{ cost: 9 }],
    });
  });

  it("captures args and effective initial limit; manual clicks each fetch one page", async () => {
    const args = { sinceMs: 123, limit: 2, filters: [{ value: "original" }] };
    const h = fixture({ manual: true, limit: 1 }, args);
    try {
      args.filters[0].value = "mutated";
      await h.head(page([row("a")], 4, "c1"));
      const initialArgs = name === "queryContext" ? h.subs[0].args : h.calls[0].args;
      expect(initialArgs).toEqual({ sinceMs: 123, limit: 1, filters: [{ value: "original" }] });
      const before = h.calls.length;
      expect(h.pending).toHaveLength(0);
      expect(h.results.at(-1)).toMatchObject({ returned: 1, loading: false });
      const one = h.stop.loadMore();
      expect(h.stop.loadMore()).toBe(one);
      expect(h.calls).toHaveLength(before + 1);
      h.resolve(page([row("b")], 4, "c2"));
      await one;
      await h.head(page([row("a", 9)], 5, "head"));
      expect(h.results.at(-1)).toMatchObject({ returned: 2, total: 5 });
      const next = h.stop.loadMore();
      expect(h.lastCall().args).toEqual({
        sinceMs: 123,
        limit: 1,
        filters: [{ value: "original" }],
        cursor: "c2",
      });
      const count = h.calls.length;
      h.resolve(page([row("c")], 4, "c3"));
      expect(await next).toMatchObject({ returned: 3 });
      expect(h.calls).toHaveLength(count);
    } finally {
      h.stop();
    }
  });

  it("fences a pending old page without losing unseen rows or deepest cursor", async () => {
    const h = fixture({ manual: true, limit: 1 });
    try {
      await h.head(page([row("a")], 4, "c1"));
      const p = h.stop.loadMore();
      await h.head(page([row("a", 9)], 4, "head", 9));
      h.resolve(page([row("a", 2), row("b")], 99, "c2", 2));
      const r = await p;
      expect(r).toMatchObject({ returned: 2, total: 4, totals: { cost: 9 }, days: [{ cost: 9 }] });
      expect(r.rows).toContainEqual(row("a", 9));
      const next = h.stop.loadMore();
      expect(h.lastCall().args.cursor).toBe("c2");
      h.resolve(page([row("c")], 4, "c3"));
      await next;
    } finally {
      h.stop();
    }
  });

  it("delivers head and page errors, retries the cursor, and settles on stop", async () => {
    const h = fixture({ manual: true, limit: 1 });
    try {
      await h.head(null, new Error("not_allowed"));
      expect(h.results.at(-1)?.error?.message).toBe("not_allowed");
      await h.head(page([row("a")], 3, "c1"));
      const failed = h.stop.loadMore();
      const rejected = expect(failed).rejects.toThrow("offline");
      h.reject(new Error("offline"));
      await rejected;
      expect(h.results.at(-1)).toMatchObject({ returned: 1, loading: false });
      expect(h.results.at(-1)?.error?.message).toBe("offline");
      const retry = h.stop.loadMore();
      expect(h.lastCall().args).toMatchObject({ cursor: "c1", limit: 1 });
      const cancelled = expect(retry).rejects.toThrow("stopped");
      const count = h.results.length;
      h.stop();
      await cancelled;
      h.resolve(page([row("b")], 3, "c2"));
      await drain();
      expect(h.results).toHaveLength(count);
      if (name === "queryContext") expect(h.subs[0].off).toHaveBeenCalledOnce();
      else expect(h.unsubscribe).toHaveBeenCalledWith("sessions", expect.any(Function));
    } finally {
      h.stop();
    }
  });

  it("retains capped membership until a fresh smaller traversal completes", async () => {
    const h = fixture({ limit: 1, maxRows: 3 });
    try {
      await h.head(page([row("a"), row("b"), row("c")], 8, "old"));
      expect(h.results.at(-1)).toMatchObject({ returned: 3, complete: false });
      await h.head(page([row("x")], 2, "new"));
      expect(h.results.at(-1)?.rows).toEqual([row("a"), row("b"), row("c")]);
      h.resolve(page([row("y")], 2));
      await drain();
      expect(h.results.at(-1)).toMatchObject({
        rows: [row("x"), row("y")],
        returned: 2,
        complete: true,
      });
    } finally {
      h.stop();
    }
  });

  it("polls heads every 15s, rewalks at 60s, and stops visibility polls", async () => {
    const h = fixture({ limit: 1, restEveryS: 60 });
    try {
      await h.head(page([row("a")], 2, "c1"));
      h.resolve(page([row("b")], 2));
      await drain();
      if (name === "queryContext") h.visibility.forEach((cb) => cb(true));
      const before = h.calls.length;
      for (let i = 0; i < 3; i++) {
        await vi.advanceTimersByTimeAsync(15000);
        expect(h.calls).toHaveLength(before + i + 1);
        expect(h.lastCall().args.cursor).toBeUndefined();
        h.resolve(page([row("a", i)], 2, "head"));
        await drain();
      }
      await vi.advanceTimersByTimeAsync(15000);
      h.resolve(page([row("a")], 2, "rest"));
      await drain();
      expect(h.lastCall().args.cursor).toBe("rest");
      h.resolve(page([row("b")], 2));
      await drain();
      expect(h.results.at(-1)?.complete).toBe(true);
      const refreshed = h.stop.refresh();
      expect(h.lastCall().args).toEqual({ limit: 1 });
      h.resolve(page([], 0));
      expect(await refreshed).toMatchObject({ returned: 0, complete: true });
      h.visibility.forEach((cb) => cb(false));
      const count = h.calls.length;
      await vi.advanceTimersByTimeAsync(60000);
      expect(h.calls).toHaveLength(count);
    } finally {
      h.stop();
    }
  });
});

describe("kit usage identity parity", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  it("uses exact CLI/session pairs and treats machineId as metadata", async () => {
    const h = mockKitQueries();
    const k = loadKit(h.cs);
    const p = k.queryAll("sessionUsage", {}, { limit: 2 });
    h.resolve({
      sessions: [
        { cli: "codex", sessionId: "same", machineId: "one" },
        { cli: "claude", sessionId: "same", machineId: "one" },
      ],
      total: 2,
      nextCursor: "c",
    });
    await vi.advanceTimersByTimeAsync(0);
    h.resolve({ sessions: [{ cli: "codex", sessionId: "same", machineId: "two" }], total: 2 });
    expect(await p).toMatchObject({
      returned: 2,
      complete: true,
      rows: [
        { cli: "codex", sessionId: "same", machineId: "two" },
        { cli: "claude", sessionId: "same", machineId: "one" },
      ],
    });
  });
});

function listFixture() {
  const h = mockKitQueries();
  return {
    sdk: loadKit(h.cs),
    head: h.tick,
    reply: h.resolve,
    fail: (error: string) => h.reject(new Error(error)),
    count: () => h.calls.length,
    args: () => h.lastCall().args as Record<string, unknown>,
    cleanup: () => {},
  };
}

describe("shared partialList integration", () => {
  it("installs one synchronous default-only inline baseline, independent of terminal and identical across ports", async () => {
    document.getElementById("cs-partial-list-style")?.remove();
    const authored = document.createElement("style");
    authored.textContent = ".authored { color: red; }";
    document.head.append(authored);
    const h = listFixture();
    const container = document.createElement("div");
    let receive: (r: Result) => void = () => {};
    const loadMore = vi.fn(async () => ({
      rows: [],
      total: 0,
      returned: 0,
      complete: true,
      truncated: false,
    }));
    const beforeStyles = document.head.querySelectorAll("style").length;
    const view = h.sdk.partialList({
      container,
      render: () => document.createElement("div"),
      loadMore,
      subscribe: (cb: (r: Result) => void) => {
        expect(document.getElementById("cs-partial-list-style")).not.toBeNull();
        receive = cb;
        return () => {};
      },
    });
    const other = h.sdk.partialList({ container, render: () => document.createElement("div") });
    try {
      const resource = document.getElementById("cs-partial-list-style") as HTMLStyleElement;
      expect(document.head.firstChild).toBe(resource);
      expect(document.head.querySelectorAll("#cs-partial-list-style")).toHaveLength(1);
      expect(document.head.querySelectorAll("style")).toHaveLength(beforeStyles + 1);
      const rules = Array.from(resource.sheet!.cssRules) as CSSStyleRule[];
      expect(rules.length).toBeGreaterThan(0);
      expect(rules.every((rule) => rule.selectorText.includes(".cs-partial-list"))).toBe(true);
      const selectors = rules.map((rule) => rule.selectorText);
      for (const selector of [
        ":where(.cs-partial-list)",
        ":where(.cs-partial-list > .cs-partial-list-status)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:hover)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:focus-visible)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:disabled)",
        ":where(.cs-partial-list > .cs-partial-list-status) > .cs-partial-list-more[hidden]",
      ])
        expect(selectors).toContain(selector);
      const css = resource.textContent!;
      // Every variable use has a fallback, including nested hover foreground.
      for (const match of css.matchAll(/var\((--cs-[\w-]+)([^,)]*)/g)) expect(match[2]).toBe("");
      expect(css.match(/var\(--cs-[\w-]+,/g)?.length).toBe(css.match(/var\(/g)?.length);
      expect(css).not.toMatch(/--cs-[\w-]+\s*:/);
      const kitDir = join(process.cwd(), "src-tauri/src/http/routes/dashboard_kit");
      const tokens = readFileSync(join(kitDir, "tokens.css"), "utf8");
      for (const colors of css.matchAll(/light-dark\([^)]+\)/g))
        expect(tokens).toContain(colors[0]);
      const sources = [
        readFileSync(
          join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
          "utf8",
        ),
        readFileSync(join(kitDir, "kit.js"), "utf8"),
      ];
      const resourceBlock = (s: string) =>
        s.slice(s.indexOf("  var PARTIAL_LIST_STYLE ="), s.indexOf("  function partialList("));
      const component = (s: string) => {
        const start = s.indexOf("  function partialList(");
        return s.slice(start, s.indexOf("\n  }\n", start) + 5);
      };
      expect(resourceBlock(sources[0])).toBe(resourceBlock(sources[1]));
      expect(component(sources[0])).toBe(component(sources[1]));
      const button = view.el.querySelector("button")!;
      receive({
        rows: [],
        returned: 0,
        total: 1,
        complete: false,
        truncated: true,
        canLoadMore: true,
        loading: true,
      });
      expect(button.disabled).toBe(true);
      button.click();
      expect(loadMore).not.toHaveBeenCalled();
      receive({
        rows: [],
        returned: 0,
        total: 1,
        complete: false,
        truncated: true,
        canLoadMore: true,
        loading: false,
      });
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(loadMore).toHaveBeenCalledOnce();
      expect(button.hidden).toBe(true);
      view.stop();
      other.stop();
      expect(document.getElementById("cs-partial-list-style")).toBe(resource);
    } finally {
      view.stop();
      other.stop();
      h.cleanup();
      authored.remove();
      document.getElementById("cs-partial-list-style")?.remove();
    }
  });

  it("leaves bound controls authored and outside every default baseline selector", () => {
    document.getElementById("cs-partial-list-style")?.remove();
    const h = listFixture();
    const layout = document.createElement("section");
    const status = document.createElement("p");
    const button = document.createElement("button");
    status.className = "authored-status";
    button.className = "authored-button";
    status.style.color = "red";
    button.style.margin = "3px";
    layout.append(status, button);
    const attributes = (el: Element) => [el.className, el.getAttribute("style")];
    const before = [attributes(status), attributes(button)];
    const bound = h.sdk.partialList({ status, button });
    let generated: ReturnType<typeof h.sdk.partialList> | undefined;
    try {
      expect(document.getElementById("cs-partial-list-style")).toBeNull();
      expect(Array.from(layout.children)).toEqual([status, button]);
      expect([attributes(status), attributes(button)]).toEqual(before);
      generated = h.sdk.partialList({
        container: document.createElement("div"),
        render: () => document.createElement("div"),
      });
      const resource = document.getElementById("cs-partial-list-style") as HTMLStyleElement;
      const rules = Array.from(resource.sheet!.cssRules) as CSSStyleRule[];
      for (const rule of rules)
        for (const el of [layout, status, button])
          expect(el.matches(rule.selectorText)).toBe(false);
      expect([attributes(status), attributes(button)]).toEqual(before);
      expect(Array.from(layout.children)).toEqual([status, button]);
    } finally {
      bound.stop();
      generated?.stop();
      h.cleanup();
      document.getElementById("cs-partial-list-style")?.remove();
    }
  });

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("keeps an unresolved head warning in bound controls and actual onResult callbacks through page actions", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const results: Result[] = [];
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
      onResult: (result: Result) => results.push(result),
    });
    try {
      const initial = { rows: [{ id: "a" }], total: 3, nextCursor: "c1", totals: { cost: 1 } };
      h.head(initial);
      const next = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      h.head(null, new Error("rate_limited"));
      const warned = results.length - 1;
      expect(results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      expect(status.textContent).toContain("rate_limited");
      h.reply({ rows: [{ id: "b" }], total: 3, nextCursor: "c2", totals: { cost: 2 } });
      await next;
      expect(results.at(-1)).toMatchObject({
        returned: 2,
        totals: { cost: 2 },
        error: { message: "rate_limited" },
      });
      expect(status.textContent).toContain("rate_limited");
      const failed = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toContain("rate_limited");
      h.fail("offline");
      await failed;
      expect(status.textContent).toContain("offline");
      expect(button.textContent).toBe("Retry");
      const retry = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.args().cursor).toBe("c2");
      expect(status.textContent).toContain("rate_limited");
      h.reply({ rows: [{ id: "c" }], total: 3, totals: { cost: 3 } });
      await retry;
      expect(results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        totals: { cost: 3 },
        error: { message: "rate_limited" },
      });
      expect(status.textContent).toContain("rate_limited");
      expect(results.slice(warned).every((r) => !!r.error)).toBe(true);
      const count = h.count();
      h.head(initial);
      await vi.advanceTimersByTimeAsync(0);
      expect(results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        totals: { cost: 3 },
        error: null,
      });
      expect(status.textContent).toBe("Showing all 3");
      expect(h.count()).toBe(count);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("keeps stale manual component controls waiting until a head, then needs a click", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
    });
    try {
      const old = { rows: [{ id: "a" }], total: 2, nextCursor: "c1" };
      h.head(old);
      const next = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      h.fail("query_invalid");
      await next;
      const count = h.count();
      expect(status.textContent).toContain("Waiting for fresh data");
      expect(button.textContent).toBe("Waiting…");
      expect(button.disabled).toBe(true);
      expect(button.hidden).toBe(false);
      button.click();
      await view.refresh();
      await view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count);
      h.head(old);
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count);
      expect(button.disabled).toBe(false);
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count + 1);
      expect(h.args().cursor).toBe("c1");
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toBe("Showing all 2");
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("settles its public query action on stop while the dispatched query remains pending", async () => {
    const h = listFixture();
    const container = document.createElement("div");
    let reply!: (result: Result) => void;
    const query = vi.fn(
      () =>
        new Promise<Result>((resolve) => {
          reply = resolve;
        }),
    );
    const render = vi.fn(() => document.createElement("div"));
    const view = h.sdk.partialList({ container, query, render });
    const action = view.refresh();
    let settled = false;
    action.then(() => {
      settled = true;
    });
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(query).toHaveBeenCalledOnce();
      expect(settled).toBe(false);
      const before = container.textContent;
      view.stop();
      await vi.advanceTimersByTimeAsync(0);
      expect(settled).toBe(true);
      reply({ rows: [{ id: "late" }], total: 1, returned: 1, complete: true, truncated: false });
      await vi.advanceTimersByTimeAsync(0);
      expect(render).not.toHaveBeenCalled();
      expect(container.textContent).toBe(before);
      expect(container.querySelector("button")!.disabled).toBe(true);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("binds actual paging controls, handles retry/double click, refreshes zero and stops", async () => {
    const h = listFixture();
    const container = document.createElement("div");
    const view = h.sdk.partialList({
      container,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true, limit: 1 }),
      render: (row: any) => {
        const el = document.createElement("div");
        el.textContent = row.id;
        return el;
      },
    });
    try {
      h.head({ rows: [{ id: "a" }], nextCursor: "c1" });
      await vi.advanceTimersByTimeAsync(0);
      const text = container.querySelector(".cs-partial-list-text")!;
      const button = container.querySelector("button")!;
      expect(text.textContent).toBe("Loaded 1 · total unknown");
      button.click();
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(1);
      expect(h.args().cursor).toBe("c1");
      h.fail("offline");
      await vi.advanceTimersByTimeAsync(0);
      expect(text.textContent).toContain("offline");
      expect(button.textContent).toBe("Retry");
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(2);
      expect(h.args().cursor).toBe("c1");
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(text.textContent).toBe("Showing all 2");
      expect(button.hidden).toBe(true);
      const refresh = view.refresh();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.args().cursor).toBeUndefined();
      h.reply({ rows: [], total: 0 });
      await refresh;
      expect(text.textContent).toBe("Showing all 0");
      view.stop();
      h.head({ rows: [{ id: "late" }], total: 1 });
      expect(text.textContent).toBe("Showing all 0");
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("binds existing coverage elements without a duplicate list and reports the ceiling", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const layout = document.createElement("section");
    layout.append(status, button);
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true, maxRows: 1 }),
    });
    try {
      h.head({ rows: [{ id: "a" }], total: 3, nextCursor: "c" });
      await vi.advanceTimersByTimeAsync(0);
      expect(layout.children).toHaveLength(2);
      expect(status.textContent).toContain("Showing 1 of 3");
      expect(status.textContent).toContain("row limit reached");
      expect(button.hidden).toBe(true);
      expect(h.count()).toBe(0);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("settles an in-flight bound component action when stopped and suppresses late replies", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
    });
    try {
      h.head({ rows: [{ id: "a" }], total: 2, nextCursor: "c" });
      const loading = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(1);
      view.stop();
      await loading;
      const text = status.textContent;
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toBe(text);
      expect(button.disabled).toBe(true);
    } finally {
      view.stop();
      h.cleanup();
    }
  });
});

function anyListFixture() {
  const calls: {
    args: any;
    resolve: (a: any) => void;
    reject: (e: Error) => void;
    done: boolean;
  }[] = [];
  const sdk = loadKit({
    invoke: (_verb: string, args: any) =>
      new Promise((resolve, reject) => calls.push({ args, resolve, reject, done: false })),
    onVisibility: (cb: (shown: boolean) => void) => {
      cb(true);
      return () => {};
    },
  });
  function reply(a: any) {
    const c = calls.find((c) => !c.done)!;
    c.done = true;
    c.resolve(a);
  }
  return {
    sdk,
    reply,
    fail: (e: string) => {
      const c = calls.find((c) => !c.done)!;
      c.done = true;
      c.reject(new Error(e));
    },
    head: async (a: any) => {
      await vi.advanceTimersByTimeAsync(0);
      let c = calls.find((c) => !c.done && !c.args.cursor);
      if (!c) {
        await vi.advanceTimersByTimeAsync(15000);
        c = calls.find((c) => !c.done && !c.args.cursor);
      }
      expect(c).toBeDefined();
      c!.done = true;
      c!.resolve(a);
      await vi.advanceTimersByTimeAsync(0);
    },
    args: () => calls.at(-1)!.args,
    count: () => calls.length,
    cleanup: () => {},
  };
}

testAnyListPaging(anyListFixture);

describe("app-only paging shapes", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("pages app inbox messages through caller options and before/hasMore", async () => {
    const h = anyListFixture();
    const p = h.sdk.queryAll(
      "inbox",
      { limit: 1 },
      {
        rows: "messages",
        id: "messageId",
        cursor: {
          arg: "before",
          next: (a: any) => (a.hasMore ? (a.messages.at(-1)?.messageId ?? null) : null),
        },
      },
    );
    await vi.advanceTimersByTimeAsync(0);
    h.reply({ messages: [{ messageId: 9 }], humanUnread: 2, conversations: [], hasMore: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(h.args()).toMatchObject({ before: 9, limit: 1 });
    h.reply({ messages: [{ messageId: 8 }], humanUnread: 2, conversations: [], hasMore: false });
    expect(await p).toMatchObject({
      rows: [{ messageId: 9 }, { messageId: 8 }],
      canLoadMore: false,
      humanUnread: 2,
    });
    expect(h.count()).toBe(2);
  });

  it("pages app routineRuns through caller options and next_before", async () => {
    const h = anyListFixture();
    const p = h.sdk.queryAll(
      "routineRuns",
      { key: "org/team/routine", limit: 1 },
      {
        rows: "runs",
        id: "run_id",
        cursor: { arg: "before", next: (a: any) => a.next_before },
      },
    );
    await vi.advanceTimersByTimeAsync(0);
    h.reply({ runs: [{ run_id: "run9" }], next_before: "run9" });
    await vi.advanceTimersByTimeAsync(0);
    expect(h.args()).toMatchObject({ before: "run9", key: "org/team/routine", limit: 1 });
    h.reply({ runs: [{ run_id: "run8" }], next_before: null });
    expect(await p).toMatchObject({
      rows: [{ run_id: "run9" }, { run_id: "run8" }],
      canLoadMore: false,
    });
    expect(h.count()).toBe(2);
  });
});

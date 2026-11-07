// SPDX-License-Identifier: MIT
/**
 * WebDriver-only WebGL renderer diagnostics (registered by the shell terminal panel
 * under `MODE === "webdriver"`; tree-shaken from release builds).
 *
 * Reads xterm WebGL-addon internals per terminal so the harness records, at every
 * capture, renderer facts instead of inferring them from pixels:
 * - `atlasId`: a stable id per TextureAtlas OBJECT. Two panes reporting the same id
 *   share one atlas — the invariant the addon patch exists to hold.
 * - `pages` / `uploadedVersions`: CPU atlas page versions vs the versions this pane's
 *   GL context last uploaded (skew = a pane drawing stale texture content).
 * - `contextLost`, `canvas` vs `viewport`: presentation-level failures (blank/frozen).
 * - `renderPaused`: xterm's IntersectionObserver pause (frozen canvas, no renders).
 * - `clearCount`: how many times THIS pane's atlas was cleared.
 * - `timeline` / `frameClock`: recent render-service events, and whether the page is
 *   visible and its requestAnimationFrame loop is ticking.
 * - `lease`: the display's WebGL lease (held, pinned to DOM) and the pool's fill
 *   (`held` of `cap`), from the terminal manager.
 * Everything is optional-chained: a renamed internal reads as null, never throws.
 */
import type { Terminal } from "@xterm/xterm";
import type { WebglLeaseReading } from "@/shared/terminal/webgl-lease-pool";

const atlasIds = new WeakMap<object, number>();
let nextAtlasId = 1;
const clearCounts = new WeakMap<Terminal, number>();
const contextEvents = { lost: 0, restored: 0, lastLostAt: 0 };
// Page-wide requestAnimationFrame liveness + visibility: xterm renders on rAF, so a
// page the webview considers hidden (no rAF) shows a frozen or blank canvas.
const frameClock = { ticks: 0, lastTickAt: 0, visibilityChanges: [] as string[] };
let listening = false;

function idFor(atlas: object | undefined | null): number | null {
  if (!atlas) return null;
  let id = atlasIds.get(atlas);
  if (id === undefined) {
    id = nextAtlasId++;
    atlasIds.set(atlas, id);
  }
  return id;
}

/** Count WebGL context loss/restore page-wide (non-bubbling events → capture phase). */
export function installWebglContextEventCounters(): void {
  if (listening) return;
  listening = true;
  const tick = () => {
    frameClock.ticks++;
    frameClock.lastTickAt = Math.round(performance.now());
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  document.addEventListener("visibilitychange", () => {
    frameClock.visibilityChanges.push(
      `${Math.round(performance.now())} ${document.visibilityState}`,
    );
  });
  window.addEventListener(
    "webglcontextlost",
    () => {
      contextEvents.lost++;
      contextEvents.lastLostAt = performance.now();
    },
    true,
  );
  window.addEventListener("webglcontextrestored", () => contextEvents.restored++, true);
}

/** Count every `clearTextureAtlas` per pane and record a render-service event timeline. */
export function instrumentClearTextureAtlas(term: Terminal): void {
  const t = term as Terminal & { __diagWrapped?: boolean };
  if (t.__diagWrapped) return;
  t.__diagWrapped = true;
  const original = term.clearTextureAtlas.bind(term);
  term.clearTextureAtlas = () => {
    clearCounts.set(term, (clearCounts.get(term) ?? 0) + 1);
    original();
  };
  // A bounded timeline of render-service events (renderer swaps, resizes, renders,
  // atlas clears) so a frozen or blank capture shows what last happened.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const rs = (term as any)._core?._renderService;
  if (!rs) return;
  const log: string[] = [];
  timelines.set(term, log);
  const push = (e: string) => {
    log.push(`${Math.round(performance.now())} ${e}`);
    if (log.length > 60) log.shift();
  };
  const wrap = (obj: Record<string, unknown>, name: string, fmt?: (...a: unknown[]) => string) => {
    const orig = obj[name] as ((...a: unknown[]) => unknown) | undefined;
    if (typeof orig !== "function") return;
    obj[name] = function (this: unknown, ...a: unknown[]) {
      push(name + (fmt ? " " + fmt(...a) : ""));
      return orig.apply(this, a);
    };
  };
  wrap(rs, "setRenderer", (r) =>
    String((r as { constructor?: { name?: string } })?.constructor?.name),
  );
  wrap(rs, "handleResize", (c, r) => `${c}x${r} paused=${rs._isPaused}`);
  wrap(rs, "handleCharSizeChanged");
  wrap(
    rs,
    "_handleIntersectionChange",
    (e) => `intersecting=${(e as IntersectionObserverEntry).isIntersecting}`,
  );
  wrap(rs, "_renderRows", (a, b) => {
    const r = rs._renderer?.value;
    return `${a}-${b} attached=${r?._isAttached} glyph=${!!r?._glyphRenderer?.value} char=${r?.dimensions?.device?.char?.width}`;
  });
  wrap(rs, "clearTextureAtlas");
}
const timelines = new WeakMap<Terminal, string[]>();

export interface XtermWebglDiag {
  atlasId: number | null;
  pages: Array<{ size: number; version: number }> | null;
  uploadedVersions: number[] | null;
  contextLost: boolean | null;
  canvas: { width: number; height: number; cssWidth: string; cssHeight: string } | null;
  viewport: number[] | null;
  renderPaused: boolean | null;
  rendererAttached: boolean | null;
  clearCount: number;
  contextEvents: typeof contextEvents;
  timeline: string[];
  lease: WebglLeaseReading | null;
  frameClock: typeof frameClock & {
    now: number;
    visibility: DocumentVisibilityState;
    hasFocus: boolean;
  };
}

export function readWebglDiag(
  term: Terminal,
  lease: WebglLeaseReading | null = null,
): XtermWebglDiag {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const core = (term as any)._core;
  const renderService = core?._renderService;
  const renderer = renderService?._renderer?.value;
  const atlas = renderer?._charAtlas;
  const glyph = renderer?._glyphRenderer?.value;
  const gl: WebGL2RenderingContext | undefined = renderer?._gl;
  const canvas: HTMLCanvasElement | undefined = renderer?._canvas;
  let viewport: number[] | null = null;
  try {
    viewport = gl ? Array.from(gl.getParameter(gl.VIEWPORT) as Int32Array) : null;
  } catch {
    viewport = null;
  }
  return {
    atlasId: idFor(atlas),
    pages:
      atlas?.pages?.map((p: { canvas: HTMLCanvasElement; version: number }) => ({
        size: p.canvas.width,
        version: p.version,
      })) ?? null,
    uploadedVersions:
      glyph?._atlasTextures
        ?.slice(0, atlas?.pages?.length ?? 0)
        .map((t: { version: number }) => t.version) ?? null,
    contextLost: gl ? gl.isContextLost() : null,
    canvas: canvas
      ? {
          width: canvas.width,
          height: canvas.height,
          cssWidth: canvas.style.width,
          cssHeight: canvas.style.height,
        }
      : null,
    viewport,
    renderPaused: typeof renderService?._isPaused === "boolean" ? renderService._isPaused : null,
    rendererAttached: typeof renderer?._isAttached === "boolean" ? renderer._isAttached : null,
    clearCount: clearCounts.get(term) ?? 0,
    contextEvents: { ...contextEvents },
    timeline: timelines.get(term) ?? [],
    lease,
    frameClock: {
      ...frameClock,
      now: Math.round(performance.now()),
      visibility: document.visibilityState,
      hasFocus: document.hasFocus(),
    },
  };
}

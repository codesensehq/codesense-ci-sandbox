// SPDX-License-Identifier: MIT
/**
 * Test harness for driving the REAL published `@xterm/addon-webgl` code (both package
 * entries) under happy-dom, which has neither WebGL nor a 2D canvas. A stubbed GL / 2D
 * context just lets the renderer construct and resize; assertions read the renderer's
 * own state.
 *
 * Both entry points matter: a resolver may pick `module` (the ESM `.mjs`, which Vite
 * bundles) or `main` (the CJS `.js`). Our pnpm patch edits both, so tests guarding it
 * run against both.
 */
import { createRequire } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Terminal } from "@xterm/xterm";

export type WebglAddonCtor = new () => { dispose(): void };

const CELL_W = 8;
const CELL_H = 16;

function fakeWebgl2(canvas: HTMLCanvasElement): WebGL2RenderingContext {
  const params: Record<number, unknown> = {};
  const target = {
    canvas,
    MAX_TEXTURE_SIZE: 0x0d33,
    MAX_TEXTURE_IMAGE_UNITS: 0x8872,
    VIEWPORT: 0x0ba2,
    getParameter: (p: number) =>
      p === 0x0d33 ? 4096 : p === 0x8872 ? 16 : p === 0x0ba2 ? new Int32Array(4) : (params[p] ?? 0),
    getShaderParameter: () => true,
    getProgramParameter: () => true,
    getExtension: () => null,
    isContextLost: () => false,
    getContextAttributes: () => ({}),
    getUniformLocation: () => ({}),
    getAttribLocation: () => 0,
  } as Record<string, unknown>;
  return new Proxy(target, {
    get(t, prop: string) {
      if (prop in t) return t[prop];
      // GL enum constants are UPPER_CASE; everything else is a call we can no-op.
      if (/^[A-Z0-9_]+$/.test(prop)) return 1;
      return () => ({});
    },
  }) as unknown as WebGL2RenderingContext;
}

function fake2d(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const target = {
    canvas,
    measureText: () => ({
      width: CELL_W,
      actualBoundingBoxAscent: CELL_H - 4,
      actualBoundingBoxDescent: 4,
      fontBoundingBoxAscent: CELL_H - 4,
      fontBoundingBoxDescent: 4,
    }),
    getImageData: (_x: number, _y: number, w: number, h: number) => ({
      width: w,
      height: h,
      data: new Uint8ClampedArray(Math.max(1, w * h * 4)),
    }),
    createImageData: (w: number, h: number) => ({
      width: w,
      height: h,
      data: new Uint8ClampedArray(Math.max(1, w * h * 4)),
    }),
    createLinearGradient: () => ({ addColorStop: () => {} }),
  } as Record<string, unknown>;
  return new Proxy(target, {
    get(t, prop: string) {
      if (prop in t) return t[prop];
      return () => {};
    },
    set(t, prop: string, value) {
      t[prop] = value;
      return true;
    },
  }) as unknown as CanvasRenderingContext2D;
}

const originalGetContext = HTMLCanvasElement.prototype.getContext;
const originalOffsetWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetWidth");
const originalOffsetHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");

/** Install the fake GL / 2D contexts and the char-measure geometry. Call in `beforeEach`. */
export function installFakeCanvasContexts(): void {
  HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string) {
    if (kind === "webgl2") return fakeWebgl2(this);
    if (kind === "2d") return fake2d(this);
    return null;
  } as typeof HTMLCanvasElement.prototype.getContext;
  // xterm's DOM char measurer reads offsetWidth/Height of a 32-char probe span.
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get: () => CELL_W * 32,
  });
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get: () => CELL_H,
  });
}

/** Undo `installFakeCanvasContexts` and clear the DOM. Call in `afterEach`. */
export function restoreCanvasContexts(): void {
  HTMLCanvasElement.prototype.getContext = originalGetContext;
  if (originalOffsetWidth)
    Object.defineProperty(HTMLElement.prototype, "offsetWidth", originalOffsetWidth);
  if (originalOffsetHeight)
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", originalOffsetHeight);
  document.body.innerHTML = "";
}

/** The WebGL renderer instance the terminal is drawing with. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function webglRendererOf(term: Terminal): any {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (term as any)._core._renderService._renderer.value;
}

export function openWithWebgl(Addon: WebglAddonCtor, cols = 20, rows = 5): Terminal {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const term = new Terminal({ cols, rows, fontFamily: "monospace", fontSize: 13 });
  term.open(el);
  term.loadAddon(new Addon() as never);
  return term;
}

const addonDir = join(process.cwd(), "node_modules/@xterm/addon-webgl/lib");

export const webglAddonEntryPoints: Array<[string, () => Promise<WebglAddonCtor>]> = [
  [
    "ESM entry (package `module`, what Vite bundles)",
    async () =>
      (await import(/* @vite-ignore */ pathToFileURL(join(addonDir, "addon-webgl.mjs")).href))
        .WebglAddon as WebglAddonCtor,
  ],
  [
    "CJS entry (package `main`)",
    async () =>
      createRequire(import.meta.url)(join(addonDir, "addon-webgl.js")).WebglAddon as WebglAddonCtor,
  ],
];

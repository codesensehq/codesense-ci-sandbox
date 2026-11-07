// SPDX-License-Identifier: MIT
/**
 * Runtime guard: every terminal gets its OWN WebGL glyph atlas.
 *
 * xterm's addon-webgl shares one TextureAtlas across all same-config terminals; our
 * pnpm patch removes that. A shared atlas lets one pane's `clearTextureAtlas()` rewrite
 * the pages its siblings still hold cached glyph coordinates into (wrong glyphs / blank
 * cells). This drives the REAL addon code with ≥2 real xterm terminals and reads the
 * atlas object each renderer acquired.
 *
 * Both published entry points are exercised (see the harness). The built bundle itself
 * is guarded separately by the `scripts/vite-plugin-webgl-atlas-guard.ts` build plugin.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Terminal } from "@xterm/xterm";
import {
  installFakeCanvasContexts,
  openWithWebgl,
  restoreCanvasContexts,
  webglAddonEntryPoints,
  webglRendererOf,
} from "./__test-utils__/webgl-addon-harness";

beforeEach(installFakeCanvasContexts);
afterEach(restoreCanvasContexts);

function atlasOf(term: Terminal): unknown {
  return webglRendererOf(term)._charAtlas;
}

describe("WebGL glyph atlas is per-terminal (never shared)", () => {
  it.each(webglAddonEntryPoints)(
    "%s: two same-config terminals acquire distinct atlases",
    async (_label, load) => {
      const Addon = await load();
      const a = openWithWebgl(Addon);
      const b = openWithWebgl(Addon);
      try {
        expect(atlasOf(a)).toBeTruthy();
        expect(atlasOf(b)).toBeTruthy();
        expect(atlasOf(a)).not.toBe(atlasOf(b));
      } finally {
        a.dispose();
        b.dispose();
      }
    },
  );
});

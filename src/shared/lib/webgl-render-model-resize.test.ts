// SPDX-License-Identifier: MIT
/**
 * Runtime guard: after ANY resize, the WebGL render model tracks one line length per
 * terminal row.
 *
 * Upstream `RenderModel.resize` reallocates `lineLengths` only when the cell COUNT
 * changes. A transposed resize (36x17 → 17x36: same cols*rows) keeps the old 17-entry
 * array, and the glyph renderer copies vertices only for `lineLengths.length` rows — so
 * every row past the old row count draws blank while the buffer (and the cursor, drawn
 * by the rectangle renderer) is intact. A mosaic preset switch that turns a wide pane
 * into a tall column hits exactly this. Our pnpm patch reallocates `lineLengths`
 * whenever the row count changes.
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

function writeSync(term: Terminal, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

describe("WebGL render model survives a same-cell-count resize", () => {
  it.each(webglAddonEntryPoints)(
    "%s: 36x17 → 17x36 keeps a line length for every row and draws the bottom rows",
    async (_label, load) => {
      const Addon = await load();
      // A different size first, so the 36x17 resize is the one that allocates the model.
      const term = openWithWebgl(Addon, 80, 36);
      try {
        term.resize(36, 17);
        term.resize(17, 36);
        const renderer = webglRendererOf(term);
        expect(renderer._model.lineLengths.length).toBe(term.rows);

        const lines = Array.from({ length: term.rows }, (_, i) => `row${i}`);
        await writeSync(term, lines.join("\r\n"));
        renderer.renderRows(0, term.rows - 1);
        const lastRow = term.rows - 1;
        expect(renderer._model.lineLengths[lastRow]).toBe(`row${lastRow}`.length);
      } finally {
        term.dispose();
      }
    },
  );
});

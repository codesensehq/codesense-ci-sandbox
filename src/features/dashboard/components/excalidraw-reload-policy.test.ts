// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { decideCanvasReload } from "./excalidraw-reload-policy";

// SW-SC-3 AC-2/3/4: the three-way policy for an external write that lands
// while the canvas is open. Pure, so it's tested without mounting Excalidraw.
describe("decideCanvasReload", () => {
  it("is a no-op when the incoming scene matches what we believe is on disk (AC-4 echo)", () => {
    expect(decideCanvasReload({ externalSig: "sig-a", diskSig: "sig-a", dirty: false })).toBe(
      "noop",
    );
    // Even if the canvas is dirty, an identical incoming scene changes nothing.
    expect(decideCanvasReload({ externalSig: "sig-a", diskSig: "sig-a", dirty: true })).toBe(
      "noop",
    );
  });

  it("applies in place when content differs and there are no local edits (AC-2)", () => {
    expect(decideCanvasReload({ externalSig: "sig-b", diskSig: "sig-a", dirty: false })).toBe(
      "apply",
    );
  });

  it("nudges instead of clobbering when the user has unsaved local edits (AC-3)", () => {
    expect(decideCanvasReload({ externalSig: "sig-b", diskSig: "sig-a", dirty: true })).toBe(
      "nudge",
    );
  });
});

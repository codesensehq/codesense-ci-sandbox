// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { activationIsCoherent, supersedes, type ViewportReadiness } from "./activation";
import { ForegroundTileId, RendererId, StoryWorkspaceId } from "./identities";

/** OC-4 AC2/AC4 — activation intent primitives. */
describe("OC-4 activation intent primitives (AC2/AC4)", () => {
  it("supersedes accepts only a strictly newer generation (NFR14 stale-callback rule)", () => {
    expect(supersedes(3, 4)).toBe(true);
    expect(supersedes(3, 3)).toBe(false); // equal is not newer
    expect(supersedes(3, 2)).toBe(false); // older is stale
  });

  it("flags the one contradictory activation: background + focus-terminal (UX-DR2)", () => {
    const base = {
      workspace: StoryWorkspaceId("ws-1"),
      tile: ForegroundTileId("tile-1"),
      generation: 1,
    };
    expect(activationIsCoherent({ ...base, presence: "background", focus: "focus-terminal" })).toBe(
      false,
    );
    expect(activationIsCoherent({ ...base, presence: "foreground", focus: "focus-terminal" })).toBe(
      true,
    );
    expect(activationIsCoherent({ ...base, presence: "background", focus: "preserve-focus" })).toBe(
      true,
    );
  });

  it("readiness is a discriminated union: the renderer exists only when ready", () => {
    const ready: ViewportReadiness = { readiness: "ready", renderer: RendererId("r-1") };
    const pending: ViewportReadiness = { readiness: "geometry-pending" };
    expect(ready.readiness).toBe("ready");
    expect(pending.readiness).toBe("geometry-pending");
    if (ready.readiness === "ready") expect(ready.renderer).toBe(RendererId("r-1"));

    const renderer = RendererId("r-1");
    // @ts-expect-error a pending readiness cannot carry a renderer
    const illegal: ViewportReadiness = { readiness: "geometry-pending", renderer };
    void illegal;
  });
});

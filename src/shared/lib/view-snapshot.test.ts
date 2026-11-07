// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  VIEW_KEY_SEP,
  navViewKey,
  fileViewKey,
  pruneViewStateForTeam,
  type ViewSnapshot,
} from "./view-snapshot";

describe("view-snapshot keys (RF-perf-12)", () => {
  it("composes distinct nav + file keys under a team", () => {
    expect(navViewKey("acme:web")).toBe(`acme:web${VIEW_KEY_SEP}nav`);
    expect(fileViewKey("acme:web", "/a/b.md")).toBe(`acme:web${VIEW_KEY_SEP}/a/b.md`);
    // Real file keys are absolute paths (start with "/"), so no file key ever
    // equals the bare "nav" suffix — nav and file keys can't collide.
    expect(navViewKey("acme:web")).not.toBe(fileViewKey("acme:web", "/nav"));
  });
});

describe("pruneViewStateForTeam (RF-perf-12 GC — AC-5)", () => {
  const snap = (n: number): ViewSnapshot => ({ scrollTop: n });

  it("drops only the target team's nav + file keys", () => {
    const map: Record<string, ViewSnapshot> = {
      [navViewKey("acme:web")]: snap(10),
      [fileViewKey("acme:web", "/x.md")]: snap(20),
      [navViewKey("acme:api")]: snap(30),
      [fileViewKey("acme:api", "/y.md")]: snap(40),
    };
    const next = pruneViewStateForTeam(map, "acme:web");
    expect(next).toEqual({
      [navViewKey("acme:api")]: snap(30),
      [fileViewKey("acme:api", "/y.md")]: snap(40),
    });
  });

  it("returns the SAME reference when the team has no snapshots (referential stability)", () => {
    const map: Record<string, ViewSnapshot> = { [navViewKey("acme:api")]: snap(1) };
    expect(pruneViewStateForTeam(map, "acme:web")).toBe(map);
  });

  it("does not collide a team key that is a prefix of another team key", () => {
    // `acme:b::` must NOT prune `acme:bc::…` — the doubled separator guards it.
    const map: Record<string, ViewSnapshot> = {
      [navViewKey("acme:b")]: snap(1),
      [navViewKey("acme:bc")]: snap(2),
      [fileViewKey("acme:bc", "/z.md")]: snap(3),
    };
    const next = pruneViewStateForTeam(map, "acme:b");
    expect(next).toEqual({
      [navViewKey("acme:bc")]: snap(2),
      [fileViewKey("acme:bc", "/z.md")]: snap(3),
    });
  });
});

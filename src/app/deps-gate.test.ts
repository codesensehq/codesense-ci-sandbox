// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { AgentBinaryStatus, DependencyStatus } from "@/tauri/commands";
import { computeDepsReady, UNAVAILABLE_DEP_STATUS } from "./deps-gate";

// The status carries per-agent entries keyed by basename. These helpers build a
// single-agent (bundled `claude`) status; the gate reads only `available` and
// `resolving`, so `version`/`source`/`shadowed` are left empty.
function agent(): AgentBinaryStatus {
  return {
    available: false,
    path: null,
    resolving: false,
    version: null,
    source: null,
    shadowed: [],
    qualified: null,
    supported: null,
  };
}

function status(
  git: Partial<DependencyStatus["git"]>,
  claude: Partial<AgentBinaryStatus>,
  basename = "claude",
): DependencyStatus {
  return {
    git: { available: false, path: null, version: null, ...git },
    agents: {
      [basename]: { ...agent(), ...claude },
    },
  };
}

describe("computeDepsReady (SW-AN-3 / SW-AN-44)", () => {
  // AC-1 + AC-4: with git ready, the app boots whether Claude is absent OR
  // below-floor — both resolve to `available:false` upstream in deps.rs (whose
  // floor tests carry the distinct AC-4 proof), so at the gate layer they are the
  // same input and must both unlock the app.
  it("boots on git-ready when the active agent is absent or below-floor (available:false)", () => {
    expect(computeDepsReady(status({ available: true }, { available: false }), "claude")).toBe(
      true,
    );
  });

  // AC-5 regression: agent present + git ready still boots.
  it("boots on git-ready when the active agent is present", () => {
    expect(
      computeDepsReady(
        status({ available: true }, { available: true, path: "/usr/bin/claude" }),
        "claude",
      ),
    ).toBe(true);
  });

  // Git is the one genuinely-required dependency — its absence gates the app.
  it("shows the install view (false) when git is missing, regardless of the agent", () => {
    expect(computeDepsReady(status({ available: false }, { available: true }), "claude")).toBe(
      false,
    );
    expect(computeDepsReady(status({ available: false }, { available: false }), "claude")).toBe(
      false,
    );
  });

  // SW-OB-12 anti-flash splash preserved: while the ACTIVE agent's detection is in
  // flight we stay on the splash (null) so a Claude-present machine settles first.
  it("stays on the splash (null) while the active agent is resolving", () => {
    expect(computeDepsReady(status({ available: true }, { resolving: true }), "claude")).toBeNull();
    // Even with git missing, resolving takes precedence (matches prior behavior).
    expect(
      computeDepsReady(status({ available: false }, { resolving: true }), "claude"),
    ).toBeNull();
  });

  // SW-AN-44: before the agent list resolves, activeBasename is "" — the gate falls
  // back to the sole seeded (bundled) entry so the splash still gates on it.
  it("falls back to the sole seeded entry when the active basename is unknown", () => {
    expect(computeDepsReady(status({ available: true }, { resolving: true }), "")).toBeNull();
    expect(computeDepsReady(status({ available: true }, { available: true }), "")).toBe(true);
  });

  // SW-AN-44 bounded-resolving safeguard: an ACTIVE agent with no entry yet (a
  // not-yet-warmed drop-in) is treated as not-resolving, so git-readiness unlocks
  // — the splash never wedges on a missing entry.
  it("unlocks when the active agent has no entry (bounded-resolving safeguard)", () => {
    const s = status({ available: true }, { resolving: true }, "claude");
    expect(computeDepsReady(s, "codex")).toBe(true);
  });

  // SW-AN-44 (review): with the basename unknown ("") and MULTIPLE seeded agents,
  // the fallback must gate on ANY resolving agent — independent of key order (a
  // BTreeMap serializes key-sorted, which is not seed order). A `[0]`-index would
  // pick the alphabetically-first ("aider") and miss the resolving "claude".
  it("holds the splash on any resolving agent when the basename is unknown (order-independent)", () => {
    const s: DependencyStatus = {
      git: { available: true, path: "/usr/bin/git", version: "2.44" },
      agents: {
        aider: { ...agent(), available: true, path: "/usr/bin/aider" },
        claude: { ...agent(), resolving: true },
      },
    };
    expect(computeDepsReady(s, "")).toBeNull();
    // None resolving → git-ready unlocks regardless of order.
    s.agents.claude = { ...agent(), available: true, path: "/usr/bin/claude" };
    expect(computeDepsReady(s, "")).toBe(true);
  });
});

describe("UNAVAILABLE_DEP_STATUS", () => {
  it("gates to the install view (git unavailable, no agents)", () => {
    expect(computeDepsReady(UNAVAILABLE_DEP_STATUS, "claude")).toBe(false);
    expect(UNAVAILABLE_DEP_STATUS.agents).toEqual({});
  });
});

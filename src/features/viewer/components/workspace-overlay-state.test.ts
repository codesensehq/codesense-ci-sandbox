// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { EpicBatchStatus } from "@/features/shell/stores/epic-assembly-store";
import type { SurfaceSyncStatus } from "@/features/shell/stores/surface-sync-store";

import { deriveWorkspaceOverlay } from "./workspace-overlay-state";

const story = (over: Partial<Parameters<typeof deriveWorkspaceOverlay>[0]> = {}) => ({
  kind: "story" as const,
  status: "ready" as const,
  needsSyncOverlay: true,
  setupState: undefined,
  ...over,
});
const epic = (over: Partial<Parameters<typeof deriveWorkspaceOverlay>[0]> = {}) => ({
  kind: "epic" as const,
  status: "ready" as const,
  needsSyncOverlay: true,
  setupState: undefined,
  ...over,
});

const batch = (over: Partial<EpicBatchStatus> = {}): EpicBatchStatus => ({
  seeded: true,
  total: 2,
  ready: 0,
  installingRepos: [],
  failures: [],
  allInstallsTerminal: false,

  settled: false,
  totalFailure: false,
  partialFailure: false,
  ...over,
});
const wave = (
  s: SurfaceSyncStatus["state"],
  extra: Partial<SurfaceSyncStatus> = {},
): SurfaceSyncStatus => ({
  state: s,
  waveId: 1,
  ...extra,
});

describe("deriveWorkspaceOverlay — story", () => {
  it("keeps the overlay up in the pre-wave gap (undefined surface)", () => {
    const o = deriveWorkspaceOverlay(story(), undefined, undefined);
    expect(o.visible).toBe(true);
    expect(o.failed).toBe(false);
    // SW-XS-10: no "Preparing context…" fallback; the typed setup line speaks.
    expect(o.subLabel).toBeUndefined();
  });

  it("drops the overlay on synced", () => {
    expect(deriveWorkspaceOverlay(story(), wave("synced"), undefined).visible).toBe(false);
  });

  it("hands off to the in-pane error on failed", () => {
    const o = deriveWorkspaceOverlay(
      story(),
      wave("failed", { errorMessage: "disk full" }),
      undefined,
    );
    expect(o.failed).toBe(true);
    expect(o.failureReason).toBe("disk full");
  });

  it("no overlay when needsSyncOverlay is unset (already-materialized re-open)", () => {
    expect(
      deriveWorkspaceOverlay(story({ needsSyncOverlay: undefined }), undefined, undefined).visible,
    ).toBe(false);
  });

  it("creating status is always visible", () => {
    expect(
      deriveWorkspaceOverlay(
        story({ status: "creating", needsSyncOverlay: undefined }),
        undefined,
        undefined,
      ).visible,
    ).toBe(true);
  });
});

describe("deriveWorkspaceOverlay — epic", () => {
  it("pending while the batch hasn't begun (undefined) or isn't settled", () => {
    expect(deriveWorkspaceOverlay(epic(), undefined, undefined).visible).toBe(true);
    const o = deriveWorkspaceOverlay(
      epic(),
      undefined,
      batch({ ready: 1, installingRepos: ["a/web"] }),
    );
    expect(o.visible).toBe(true);
    expect(o.subLabel).toBe("Assembling 2 repos — 1 of 2 ready · installing a/web…");
  });

  it("clears once the batch settles clean", () => {
    const o = deriveWorkspaceOverlay(
      epic(),
      wave("synced"),
      batch({ ready: 2, allInstallsTerminal: true, settled: true }),
    );
    expect(o.visible).toBe(false);
    expect(o.failed).toBe(false);
    expect(o.partialFailure).toBe(false);
  });

  it("total failure → in-pane error, reason from assemblyError", () => {
    const o = deriveWorkspaceOverlay(
      epic(),
      undefined,
      batch({ settled: true, totalFailure: true, assemblyError: "backend blew up" }),
    );
    expect(o.failed).toBe(true);
    expect(o.failureReason).toBe("backend blew up");
  });

  // SLW-FX-27 (Part B, AC-6/AC-7): the early-armed placeholder — a born-"creating"
  // epic row with no resolved dir yet — is visible IMMEDIATELY (via status alone,
  // before any batch or wave), so the epic shows its setup affordance the instant
  // it opens, like a story. It then stays visible after the dir resolves (status
  // flips ready, `needsSyncOverlay` persists, batch not yet settled) — no flicker.
  it("SLW-FX-27: born-creating placeholder is visible immediately (pre-dir, pre-batch)", () => {
    const o = deriveWorkspaceOverlay(epic({ status: "creating" }), undefined, undefined);
    expect(o.visible).toBe(true);
  });

  it("SLW-FX-27: stays visible through the creating→ready flip (batch begun, unsettled)", () => {
    // After `openIntegration` stamps the resolved dir: status "ready",
    // needsSyncOverlay persists, batch begun but not settled.
    const o = deriveWorkspaceOverlay(epic(), undefined, batch({ settled: false }));
    expect(o.visible).toBe(true);
  });

  it("partial failure → usable epic (not visible) + in-pane flag label", () => {
    const o = deriveWorkspaceOverlay(
      epic(),
      wave("synced"),
      batch({
        ready: 1,
        allInstallsTerminal: true,
        settled: true,
        partialFailure: true,
        failures: [{ owner: "acme", repo: "web" }],
      }),
    );
    expect(o.visible).toBe(false);
    expect(o.failed).toBe(false);
    expect(o.partialFailure).toBe(true);
    expect(o.partialFailureLabel).toBe("1 repo ready · acme/web couldn't set up — reopen to retry");
  });
});

describe("deriveWorkspaceOverlay — contextLanding (terminal input hold)", () => {
  it("is landing from the pre-wave gap through syncing", () => {
    expect(deriveWorkspaceOverlay(story(), undefined, undefined).contextLanding).toBe(true);
    expect(deriveWorkspaceOverlay(story(), wave("syncing"), undefined).contextLanding).toBe(true);
  });

  it("stops landing once the wave settles, fails, or the latch is cleared", () => {
    expect(deriveWorkspaceOverlay(story(), wave("synced"), undefined).contextLanding).toBe(false);
    expect(deriveWorkspaceOverlay(story(), wave("failed"), undefined).contextLanding).toBe(false);
    expect(
      deriveWorkspaceOverlay(story({ needsSyncOverlay: false }), undefined, undefined)
        .contextLanding,
    ).toBe(false);
  });

  it("follows the epic batch for an epic workspace", () => {
    expect(deriveWorkspaceOverlay(epic(), undefined, batch()).contextLanding).toBe(false);
    expect(deriveWorkspaceOverlay(epic(), undefined, batch({ settled: true })).contextLanding).toBe(
      false,
    );
  });
});

describe("deriveWorkspaceOverlay — contextFailed", () => {
  it("is failed (not landing) once the first wave fails", () => {
    const o = deriveWorkspaceOverlay(story(), wave("failed"), undefined);
    expect(o.contextLanding).toBe(false);
    expect(o.contextFailed).toBe(true);
  });

  it("reports no failure for a later wave on a worktree whose latch has cleared", () => {
    const o = deriveWorkspaceOverlay(story({ needsSyncOverlay: false }), wave("failed"), undefined);
    expect(o.contextFailed).toBe(false);
  });
});

describe("deriveWorkspaceOverlay — contextLanded (did context really arrive)", () => {
  it("is true only once the wave settled usable", () => {
    expect(deriveWorkspaceOverlay(story(), wave("synced"), undefined).contextLanded).toBe(true);
    expect(deriveWorkspaceOverlay(story(), wave("syncing"), undefined).contextLanded).toBe(false);
  });

  it("stays false when the watchdog drops the latch with no wave ever seen", () => {
    const o = deriveWorkspaceOverlay(story({ needsSyncOverlay: false }), undefined, undefined);
    expect(o.contextLanding).toBe(false);
    expect(o.contextLanded).toBe(false);
  });
});

it("product assembly does not advertise context landing or readiness", () => {
  for (const settled of [false, true]) {
    const o = deriveWorkspaceOverlay(epic(), wave("synced"), batch({ settled }));
    expect(o.contextLanding).toBe(false);
    expect(o.contextLanded).toBe(false);
    expect(o.contextFailed).toBe(false);
  }
});

// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const open = vi.hoisted(() => vi.fn().mockResolvedValue({ status: "opened" }));
vi.mock("@/features/git-sync/lib/open-diff", () => ({ openDiff: open }));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
import { pageSdk } from "../lib/page-sdk";
it("discovers openDiff through the existing repo:read grant", () => {
  expect(pageSdk({ effectiveGrant: ["repo:read"] }).verbs).toContain("openDiff");
  expect(pageSdk({ effectiveGrant: [] }).verbs).not.toContain("openDiff");
});
it("forwards slug, SHA, file and after-range with the caller's authenticated team", async () => {
  const params = { repo: "acme/api", rev: "abcdef1", path: "a.ts", range: { start: 4, end: 6 } };
  const frame = { frameId: "frame", grant: { team: { org: "acme", team: "dev" } } };
  expect(await bridge.openDiff(params, { frame, docPath: "/pane.html" } as any)).toEqual({
    status: "opened",
  });
  expect(open).toHaveBeenCalledWith(params, {
    frameId: "frame",
    page: "/pane.html",
    team: frame.grant.team,
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({ read: vi.fn(), open: vi.fn(), register: vi.fn() }));
vi.mock("./source-redline-source", () => ({ readOpenDiff: mocks.read }));
vi.mock("@/shared/lib/open-file-in-team", () => ({ openFileInTeam: mocks.open }));
vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: mocks.open }));
vi.mock("@/shared/lib/virtual-viewer-registry", () => ({ registerVirtualTab: mocks.register }));
vi.mock("../components/open-diff-tab", () => ({ OpenDiffTab: () => null }));
import { openDiff } from "./open-diff";
const params = { repo: "acme/app", rev: "abcdef1", path: "src/a.ts", range: { start: 3, end: 5 } };
beforeEach(() => vi.clearAllMocks());
it("opens a descriptor tab in the caller team after a revalidated local read, without storing blobs", async () => {
  mocks.read.mockResolvedValue({
    status: "opened",
    redline: { before: "old secret", after: "new secret" },
    files: [],
    resolvedRev: "abcdef1",
  });
  expect(
    await openDiff(params, {
      frameId: "frame",
      page: "/pane.html",
      team: { org: "acme", team: "dev" },
    }),
  ).toEqual({ status: "opened" });
  expect(mocks.read).toHaveBeenCalledWith(params, "frame", "/pane.html");
  const [target] = mocks.open.mock.calls[0];
  expect(target.org).toBe("acme");
  expect(target.team).toBe("dev");
  expect(JSON.parse(decodeURIComponent(target.path.split("/").pop()))).toEqual({
    ...params,
    team: { org: "acme", team: "dev" },
  });
  expect(target.path).not.toContain("secret");
});
it.each(["commitNotLocal", "worktreeRemoved", "refused", "unavailable"])(
  "returns %s without opening a tab",
  async (status) => {
    const result = { status, reason: "not local", fetchCommand: "git fetch origin abcdef1" };
    mocks.read.mockResolvedValue(result);
    expect(await openDiff(params)).toEqual(result);
    expect(mocks.open).not.toHaveBeenCalled();
  },
);

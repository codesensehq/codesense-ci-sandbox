// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
const calls = vi.hoisted(() => ({ resolve: vi.fn(), surface: vi.fn(), open: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  bridgeResolveOpenTarget: calls.resolve,
  fileSurfaceCall: calls.surface,
}));
vi.mock("@/shared/lib/open-file-in-team", () => ({ openFileInTeam: calls.open }));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
beforeEach(() => {
  vi.resetAllMocks();
  calls.surface.mockRejectedValue(new Error("refused"));
  calls.resolve.mockRejectedValue(new Error("refused"));
});
const path = "/h/users/42/builtin-panes/panes/story-trail.html";
const frame = frameFixture({ documentPath: path, contentRoot: "/ctx/a", slot: "centre" });
frame.kind = "builtin";
frame.grant.dashboard = "dashboard:built-in:acme/api/story-trail";
frame.grant.team = { org: "acme/api", team: "dev" };
const ctx = { frame, frameGrant: frame.grant, docPath: path };
it("opens an exact builtin self-file with its story fragment and preserves team", async () => {
  await bridge.openFile({ path: "story-trail.html#story/SV-X", newTab: true }, ctx);
  expect(calls.surface).toHaveBeenCalledWith(
    "resolve-open",
    { path: "story-trail.html#story/SV-X" },
    frame.frameId,
  );
  expect(calls.resolve).not.toHaveBeenCalled();
  expect(calls.open).toHaveBeenCalledWith(
    { path, org: "acme/api", team: "dev" },
    expect.objectContaining({ newTab: true, hash: "#story/SV-X" }),
  );
});
it.each(["other.html", "../story-trail.html", "/tmp/story-trail.html"])(
  "keeps normal validation for builtin %s",
  async (path) => {
    calls.surface.mockRejectedValueOnce(new Error("refused"));
    await expect(bridge.openFile({ path: path + "#story/SV-X" }, ctx)).rejects.toThrow("refused");
    expect(calls.surface).toHaveBeenCalledWith("resolve-open", { path }, frame.frameId);
  },
);
it.each(["board.html#story/SV-X", "board.html"])(
  "uses admitted path resolution before interpreting a fragment in %s",
  async (input) => {
    calls.resolve.mockImplementation(async (_page, candidate) => {
      if (candidate !== "board.html") throw new Error("File not found");
      return { path: "/ctx/a/board.html", org: "acme/api", team: "dev" };
    });
    await bridge.openFile({ path: input }, { docPath: "/ctx/a/index.html" });
    expect(calls.resolve).toHaveBeenCalledWith(
      "/ctx/a/index.html",
      "board.html",
      undefined,
      undefined,
    );
    expect(calls.open).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining(input.includes("#") ? { hash: "#story/SV-X" } : { keepLayout: true }),
    );
  },
);
it("does not bypass validation for a non-builtin with the same basename or a traversal plus hash", async () => {
  const normal = { ...frame, kind: "private" as const };
  calls.resolve.mockRejectedValue(new Error("refused"));
  await expect(
    bridge.openFile({ path: "story-trail.html#session/lead" }, { ...ctx, frame: normal }),
  ).rejects.toThrow("refused");
  await expect(
    bridge.openFile({ path: "../secret.html#story/SV-X" }, { docPath: "/ctx/a/index.html" }),
  ).rejects.toThrow("refused");
  expect(calls.resolve).toHaveBeenLastCalledWith(
    "/ctx/a/index.html",
    "../secret.html",
    undefined,
    undefined,
  );
});

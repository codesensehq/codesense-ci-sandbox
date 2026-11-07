// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";
const calls = vi.hoisted(() => ({ surface: vi.fn(), open: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  fileSurfaceCall: calls.surface,
  loadAppState: vi.fn(async () => "{}"),
  saveAppState: vi.fn(async () => {}),
}));
vi.mock("@/shared/lib/open-file-in-team", () => ({ openFileInTeam: calls.open }));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
import { useViewerStore } from "@/shared/stores/viewer-store";
const literal = "/ctx/a/docs/Issue #123.md";
const team = { org: "acme/api", team: "dev" };
const frame = frameFixture({
  documentPath: "/pane.html",
  contentRoot: "/ctx/a",
  slot: "file-nav",
  builtinTeam: team,
});
beforeEach(() => {
  vi.resetAllMocks();
  calls.surface.mockImplementation(async (_operation, args) => {
    if (args.path === literal) return { ...team, path: literal };
    throw new Error("refused: File not found: " + args.path);
  });
});
it("opens a listed Issue #123.md literally without a fragment", async () => {
  expect(await bridge.openFile({ path: literal }, { frame })).toEqual({ ...team, path: literal });
  expect(calls.surface).toHaveBeenCalledWith("resolve-open", { path: literal }, frame.frameId);
  expect(calls.open).toHaveBeenCalledWith(
    { ...team, path: literal },
    expect.not.objectContaining({ hash: expect.anything() }),
  );
});
it("still opens the exact bundled self-file with #story/X as its fragment", async () => {
  const path = "/h/users/42/builtin-panes/panes/story-trail.html";
  const self = {
    ...frame,
    documentPath: path,
    kind: "builtin" as const,
    grant: { ...frame.grant, dashboard: "dashboard:built-in:acme/api/story-trail" },
  };
  await bridge.openFile(
    { path: "story-trail.html#story/X", newTab: true },
    { frame: self, docPath: path },
  );
  expect(calls.open).toHaveBeenCalledWith(
    { ...team, path },
    expect.objectContaining({ newTab: true, hash: "#story/X" }),
  );
});
it("reuses an existing literal-# tab through the real viewer opener", async () => {
  const { openFileInTeam } = await vi.importActual<typeof import("@/shared/lib/open-file-in-team")>(
    "@/shared/lib/open-file-in-team",
  );
  calls.open.mockImplementation(openFileInTeam);
  useViewerStore.setState({ openTabs: {}, activeTab: {}, fileHashRequests: {} });
  useViewerStore.getState().openFile("acme/api:dev", "", literal, { keepLayout: true });
  const before = useViewerStore.getState().openTabs["acme/api:dev"];
  await bridge.openFile({ path: literal, newTab: true }, { frame });
  expect(useViewerStore.getState().openTabs["acme/api:dev"]).toBe(before);
  expect(useViewerStore.getState().activeTab["acme/api:dev"]).toBe(literal);
  expect(before).toHaveLength(1);
});
it.each(["../secret.md#story/X", "/tmp/story-trail.html#story/X"])(
  "keeps native admission for refused %s",
  async (path) => {
    await expect(bridge.openFile({ path }, { frame })).rejects.toThrow("refused:");
    expect(calls.open).not.toHaveBeenCalled();
  },
);

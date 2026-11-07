// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
import { useViewerStore } from "./viewer-store";
import { useOrgStore } from "./org-store";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
beforeEach(() => {
  useViewerStore.setState({
    fileHashRequests: {},
    openTabs: {},
    activeTab: {},
    navHistory: {},
    navIndex: {},
  });
  useOrgStore.setState({ activeOrgSlug: "acme/api", activeTeamName: "a" });
  vi.spyOn(useViewerStore.getState(), "persistState").mockImplementation(() => {});
});
it.each([true, false])("preserves a hash in cross-team fallback newTab=%s", (newTab) => {
  openFileInTeam(
    { org: "acme/api", team: "b", path: "/ctx/b/story-trail.html" },
    { newTab, hash: "#session/lead/turn/37", keepLayout: true },
  );
  expect(useViewerStore.getState().fileHashRequests["acme/api:b"]).toEqual({
    path: "/ctx/b/story-trail.html",
    hash: "#session/lead/turn/37",
  });
});
it("preserves hashes without a host callback and re-fires an already-open tab request", () => {
  const file = { org: "acme/api", team: "a", path: "/ctx/a/story-trail.html" };
  openFileInTeam(file, { newTab: true, hash: "#story/SV-X", keepLayout: true });
  const first = useViewerStore.getState().fileHashRequests["acme/api:a"];
  openFileInTeam(file, { newTab: true, hash: "#story/SV-X", keepLayout: true });
  const second = useViewerStore.getState().fileHashRequests["acme/api:a"];
  expect(second).toEqual(first);
  expect(second).not.toBe(first);
  useViewerStore.getState().consumeFileHash("acme/api:a", first);
  expect(useViewerStore.getState().fileHashRequests["acme/api:a"]).toBe(second);
  useViewerStore.getState().consumeFileHash("acme/api:a", second);
  expect(useViewerStore.getState().fileHashRequests["acme/api:a"]).toBeUndefined();
});

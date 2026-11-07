// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { prefsFromView } from "./prefs-client";
import trace from "./prefs-writer-trace.json";
import type { HubSnapshot } from "./live-hub";
import { applyOwnedViewerPrefs, useViewerStore } from "@/shared/stores/viewer-store";

it("forgets cleared owned fields before the same committed workspace is restored", () => {
  const owned = prefsFromView(trace.final[1] as HubSnapshot).viewer!;
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, workspaces: {}, activeWorkspace: {} });
  applyOwnedViewerPrefs(owned);
  expect(useViewerStore.getState().activeWorkspace["acme:dev"]).toBe("second");
  applyOwnedViewerPrefs({});
  expect(useViewerStore.getState().activeWorkspace).toEqual({});
  applyOwnedViewerPrefs(owned);
  expect(useViewerStore.getState().activeWorkspace["acme:dev"]).toBe("second");
  expect(
    useViewerStore.getState().workspaces["acme:dev"].map((workspace) => workspace.id),
  ).toContain("second");
  applyOwnedViewerPrefs({}, true);
});

it("merges pending workspace choices by team without replacing an unrelated newer choice", () => {
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, activeWorkspace: {} });
  applyOwnedViewerPrefs({ activeWorkspace: { A: "a0", B: "b0" } });
  useViewerStore.setState({ activeWorkspace: { A: "a1", B: "b0" } });
  applyOwnedViewerPrefs({ activeWorkspace: { A: "a0", B: "b1" } });
  expect(useViewerStore.getState().activeWorkspace).toEqual({ A: "a1", B: "b1" });
  applyOwnedViewerPrefs({}, true);
});
it("retains a pending first workspace choice while applying another team's committed choice", () => {
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, activeWorkspace: {} });
  applyOwnedViewerPrefs({});
  useViewerStore.setState({ activeWorkspace: { A: "a1" } });
  applyOwnedViewerPrefs({ activeWorkspace: { B: "b1" } });
  expect(useViewerStore.getState().activeWorkspace).toEqual({ A: "a1", B: "b1" });
  applyOwnedViewerPrefs({}, true);
});
it("keeps a creating story visible while committed workspace records update", () => {
  const owned = prefsFromView(trace.final[1] as HubSnapshot).viewer!;
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, workspaces: {}, activeWorkspace: {} });
  const initial = prefsFromView(trace.initial[1] as HubSnapshot).viewer!;
  applyOwnedViewerPrefs(initial);
  const creating = {
    ...useViewerStore.getState().workspaces["acme:dev"][0],
    id: "creating",
    status: "creating" as const,
  };
  useViewerStore.setState({
    workspaces: { "acme:dev": [...useViewerStore.getState().workspaces["acme:dev"], creating] },
  });
  applyOwnedViewerPrefs(owned);
  expect(useViewerStore.getState().workspaces["acme:dev"].map((w) => w.id)).toEqual([
    "second",
    "creating",
  ]);
  applyOwnedViewerPrefs({}, true);
});
it("keeps live virtual tabs and reconciles an outside active document with pending tabs", () => {
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, openTabs: {}, activeTab: {} });
  applyOwnedViewerPrefs({
    openTabs: { A: [{ path: "/a.md", name: "/a.md" }] },
    activeTab: { A: "/a.md" },
  });
  useViewerStore.setState({
    openTabs: {
      A: [
        { path: "/a.md", name: "/a.md" },
        { path: "/local.md", name: "/local.md" },
        { path: "virtual://settings", name: "virtual://settings" },
      ],
    },
  });
  applyOwnedViewerPrefs({
    openTabs: { A: [{ path: "/outside.md", name: "/outside.md" }] },
    activeTab: { A: "/outside.md" },
  });
  const paths = useViewerStore.getState().openTabs.A.map((t) => t.path);
  expect(paths).toContain("virtual://settings");
  expect(paths).toContain("/local.md");
  expect(paths).toContain(useViewerStore.getState().activeTab.A);
  applyOwnedViewerPrefs({}, true);
});

it("keeps a creating story when an outside writer removes its persisted team bucket", () => {
  const initial = prefsFromView(trace.initial[1] as HubSnapshot).viewer!;
  applyOwnedViewerPrefs({}, true);
  useViewerStore.setState({ stateLoaded: true, workspaces: {}, activeWorkspace: {} });
  applyOwnedViewerPrefs(initial);
  const first = useViewerStore.getState().workspaces["acme:dev"][0];
  useViewerStore.setState({
    workspaces: { "acme:dev": [first, { ...first, id: "creating", status: "creating" }] },
  });
  applyOwnedViewerPrefs({ ...initial, workspaces: {} });
  expect(useViewerStore.getState().workspaces["acme:dev"]?.map((w) => w.id)).toEqual(["creating"]);
  applyOwnedViewerPrefs({}, true);
});

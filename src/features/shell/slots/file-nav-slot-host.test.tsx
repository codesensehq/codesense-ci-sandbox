// SPDX-License-Identifier: MIT
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { frameFixture, FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
import type { FrameRecord, MintFrameArgs } from "@/tauri/commands";
const state = vi.hoisted(() => ({
  mint: vi.fn(),
  rebound: vi.fn(),
  record: null as FrameRecord | null,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  mintFrame: state.mint,
  rebindFrameTeam: state.rebound,
  releaseFrame: vi.fn(async () => {}),
  getHttpApiPort: async () => 9123,
  resolveFrameGrant: async () => state.record!.grant,
  liveHubViewKey: async () => `team:acme/${state.record!.grant.team!.team}`,
  liveHubSubscribe: async (topic: string, viewKey: string) => ({
    topic,
    viewKey,
    rev: 0,
    snapshot: [],
  }),
  readDashboardPolicy: async () => ({ preset: "open" }),
  loadAppState: async () => "{}",
  saveAppState: async () => {},
  commandsSync: async () => {},
}));

import { FileNavSlotHost, FileNavSlotMount } from "./file-nav-slot-host";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
const ref = { scope: "team" as const, org: "acme", team: "a", id: "tree" };
const entry = {
  id: "tree",
  title: "Files",
  scope: "team" as const,
  team: "a",
  path: "/ctx/a/panes/tree.html",
  panesDir: "/ctx/a/panes",
  panelId: "pane:acme:a/tree",
  dashboardRef: ref,
  declaration: { slots: ["file-nav"], capabilities: ["files:read"] },
};
beforeEach(() => {
  state.mint.mockReset();
  state.rebound.mockReset();
  state.mint.mockImplementation(async (args: MintFrameArgs) => {
    const record = frameFixture(args);
    record.grant.team = { org: "acme", team: "a" };
    record.grant.scope = "open";
    state.record = record;
    return record;
  });
  state.rebound.mockImplementation(
    async (_id: string, _token: string, org: string, team: string) => {
      const record = {
        ...state.record!,
        token: `token-${team}`,
        grant: { ...state.record!.grant, team: { org, team } },
      };
      state.record = record;
      return record;
    },
  );
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: 240,
    bottom: 400,
    width: 240,
    height: 400,
    toJSON() {},
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ status: 200, ok: true })),
  );
  const id = dashboardRefKey(ref);
  usePaneStore.setState({
    sourceRows: {
      [id]: {
        id,
        dashboardRef: ref,
        entry,
        contentRoot: "/ctx/a",
        sourceRevision: 0,
        listed: true,
      },
    },
  });
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" });
  useViewerStore.setState({ navCollapsed: {}, statusNavExpandedPaths: {} });
  useDashboardProfileStore.setState({
    org: "acme",
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      chrome: {},
      cliLayout: "default",
      slots: { "file-nav": ref },
    },
  });
});
afterEach(() => {
  cleanup();
  liveHub.dispose();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function view() {
  return (
    <>
      <FileNavSlotMount teamKey="acme:a">
        <div>Native A</div>
      </FileNavSlotMount>
      <FileNavSlotMount teamKey="acme:b">
        <div>Native B</div>
      </FileNavSlotMount>
      <FileNavSlotHost />
    </>
  );
}
it("keeps one real iframe through A→B→A and posts each bound team's slot context", async () => {
  render(view());
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("f-test.localhost"));
  const frame = document.querySelector("iframe")!;
  const post = vi.fn((message: { type: string }) => {
    if (message.type === "codesense:rebind-pending")
      queueMicrotask(() => {
        window.dispatchEvent(
          new FrameMessageEvent("message", {
            source: frame.contentWindow,
            data: { type: "codesense:rebind-ack", token: state.record!.token },
          }),
        );
      });
  });
  Object.defineProperty(frame, "contentWindow", {
    value: { postMessage: post },
    configurable: true,
  });
  act(() =>
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        source: frame.contentWindow,
        data: { type: "codesense:ready", token: state.record!.token },
      }),
    ),
  );
  act(() =>
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        source: frame.contentWindow,
        data: { type: "codesense:live", token: state.record!.token },
      }),
    ),
  );
  const contexts = () =>
    post.mock.calls
      .map(([message]) => message as any)
      .filter((m) => m.type === "codesense:slot-context")
      .map((m) => m.context.team?.team);
  await waitFor(() => expect(contexts()).toContain("a"));
  act(() => useOrgStore.setState({ activeTeamName: "b" }));
  await waitFor(() => expect(contexts()).toContain("b"));
  expect(document.querySelector("iframe")).toBe(frame);
  act(() => useOrgStore.setState({ activeTeamName: "a" }));
  await waitFor(() => expect(contexts().at(-1)).toBe("a"));
  expect(state.mint).toHaveBeenCalledTimes(1);
  expect(document.querySelectorAll("iframe")).toHaveLength(1);
});
it("attaches the measured file-nav frame to LiveHub as visible", async () => {
  const attach = vi.spyOn(liveHub, "attach");
  render(view());
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("f-test.localhost"));
  const frame = document.querySelector("iframe")!;
  Object.defineProperty(frame, "contentWindow", {
    value: { postMessage: vi.fn() },
    configurable: true,
  });
  act(() =>
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        source: frame.contentWindow,
        data: { type: "codesense:live", token: state.record!.token },
      }),
    ),
  );
  await waitFor(() => expect(attach).toHaveBeenCalled());
  expect(attach.mock.calls[0][3]).toBe(true);
});
it("renders native content with no iframe when the profile has no choice", () => {
  useDashboardProfileStore.setState({ profile: null });
  const { getByText } = render(view());
  expect(getByText("Native A")).toBeTruthy();
  expect(document.querySelector("iframe")).toBeNull();
});

it("portals a usable native fallback below an overlay that does not intercept clicks", async () => {
  const id = dashboardRefKey(ref);
  usePaneStore.setState((s) => ({
    sourceRows: {
      ...s.sourceRows,
      [id]: { ...s.sourceRows[id], entry: null },
    },
  }));
  const click = vi.fn();
  const { getByText } = render(
    <>
      <FileNavSlotMount teamKey="acme:a">
        <button onClick={click}>Native action</button>
      </FileNavSlotMount>
      <FileNavSlotHost />
    </>,
  );
  await waitFor(() => expect(getByText("Native action")).toBeTruthy());
  expect(document.querySelector<HTMLElement>('[data-surface="sidebar"]')!.style.pointerEvents).toBe(
    "none",
  );
  expect(
    document.querySelector<HTMLElement>('[data-slot-id="file-nav"]')!.style.pointerEvents,
  ).toBe("none");
  getByText("Native action").click();
  expect(click).toHaveBeenCalledOnce();
});
it("clears shared selection when the last file closes", async () => {
  useDashboardProfileStore.setState({ profile: null });
  const write = vi.spyOn(liveHub, "writeUi");
  const { rerender } = render(
    <FileNavSlotMount teamKey="acme:a" selected="/a/file.md">
      Native
    </FileNavSlotMount>,
  );
  await waitFor(() =>
    expect(write).toHaveBeenCalledWith("ui:file-nav", "team:acme/a", { selected: "/a/file.md" }),
  );
  rerender(<FileNavSlotMount teamKey="acme:a">Native</FileNavSlotMount>);
  await waitFor(() =>
    expect(write).toHaveBeenCalledWith("ui:file-nav", "team:acme/a", { selected: null }),
  );
});

it("delivers the existing Focus Nav Search action to the dashboard slot", async () => {
  const lifecycle = await import("@/features/dashboard/lib/frame-lifecycle");
  const { useUiStore } = await import("@/shared/stores/ui-store");
  const post = vi.spyOn(lifecycle, "postDashboardCommand").mockImplementation(() => {});
  render(view());
  await waitFor(() => expect(document.querySelector("iframe")).toBeTruthy());
  act(() => useUiStore.getState().requestFocusLeftNavSearch());
  await waitFor(() =>
    expect(post).toHaveBeenCalledWith(
      expect.any(String),
      "file-nav",
      expect.objectContaining({ id: "nav.focus-left-search" }),
    ),
  );
  post.mockRestore();
});

it("delivers native tab reveal even when the selected path has not changed", async () => {
  const lifecycle = await import("@/features/dashboard/lib/frame-lifecycle");
  const post = vi.spyOn(lifecycle, "postDashboardCommand").mockImplementation(() => {});
  render(
    <>
      <FileNavSlotMount teamKey="acme:a" selected="/a/image.png">
        Native
      </FileNavSlotMount>
      <FileNavSlotHost />
    </>,
  );
  await waitFor(() => expect(document.querySelector("iframe")).toBeTruthy());
  post.mockClear();
  act(() =>
    useViewerStore.setState((s) => ({
      revealCounter: { ...s.revealCounter, "acme:a": (s.revealCounter["acme:a"] ?? 0) + 1 },
    })),
  );
  await waitFor(() =>
    expect(post).toHaveBeenCalledWith(
      expect.any(String),
      "file-nav",
      expect.objectContaining({ id: "nav.reveal-file", args: { path: "/a/image.png" } }),
    ),
  );
});

import { PipelineStatusNav } from "@/features/dashboard/components/pipeline-status-nav";
import { openNativeMenu } from "@/features/shell/native-menu-openers";
import { buildStoryRef, buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";

function selectedPaneNav(onStatusChange = vi.fn()) {
  const story = buildStoryRef("S-1-story.md", {
    absolutePath: "/ctx/a/S-1-story.md",
    status: "todo",
  });
  const tree = buildTree("", { absoluteDirPath: "/ctx/a", stories: [story] });
  return (
    <>
      <FileNavSlotMount teamKey="acme:a">
        <PipelineStatusNav
          tree={tree}
          repoPath="/ctx/a"
          selectedPath={null}
          onFileSelect={vi.fn()}
          onStatusChange={onStatusChange}
        />
      </FileNavSlotMount>
      <FileNavSlotHost />
    </>
  );
}
const request = (surface: string, row: Record<string, unknown>) => ({
  surface,
  teamKey: "acme:a",
  contentRoot: "/ctx/a",
  row,
  anchorRect: { x: 10, y: 30, width: 20, height: 20 },
  owner: {},
  onClose: vi.fn(),
});

it("opens the native filter menu while the Files dashboard replaces the native body", async () => {
  const { findByTestId } = render(selectedPaneNav());
  await waitFor(() => expect(document.querySelector("iframe")).toBeTruthy());
  expect(document.querySelector('[data-testid="left-nav-filter-field"]')).toBeNull();
  const req = request("nav-filter", { kind: "nav-filter" });
  let close: (() => void) | null = null;
  act(() => {
    close = openNativeMenu(req);
  });
  expect(close).toBeTypeOf("function");
  expect(await findByTestId("left-nav-lens-menu")).toBeTruthy();
  act(() => close!());
  await waitFor(() =>
    expect(document.querySelector('[data-testid="left-nav-lens-menu"]')).toBeNull(),
  );
});

it("opens the native status picker for a story while the Files dashboard replaces the native body", async () => {
  const onStatusChange = vi.fn();
  useStatusTaxonomyStore.setState({
    states: [
      { id: "todo", label: "To do", role: "not_started", order: 0 },
      { id: "done", label: "Done", role: "done", order: 1 },
    ] as never,
  });
  const { findByText } = render(selectedPaneNav(onStatusChange));
  await waitFor(() => expect(document.querySelector("iframe")).toBeTruthy());
  const req = request("nav-status", { kind: "story", path: "/ctx/a/S-1-story.md" });
  let close: (() => void) | null = null;
  act(() => {
    close = openNativeMenu(req);
  });
  expect(close).toBeTypeOf("function");
  (await findByText("Done")).click();
  expect(onStatusChange).toHaveBeenCalledWith("/ctx/a/S-1-story.md", "done");
  expect(req.onClose).toHaveBeenCalled();
});

it("keeps the outgoing file-nav frame mounted until its flush finishes when the profile switches to native", async () => {
  const { warmScopes } = await import("@/shared/lib/warm-scopes");
  warmScopes.activate("acme:a");
  render(view());
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("f-test.localhost"));
  const original = document.querySelector("iframe")!;
  const post = vi.fn();
  Object.defineProperty(original, "contentWindow", {
    value: { postMessage: post },
    configurable: true,
  });
  for (const type of ["codesense:ready", "codesense:live"])
    act(() =>
      window.dispatchEvent(
        new FrameMessageEvent("message", {
          source: original.contentWindow,
          data: { type, token: state.record!.token },
        }),
      ),
    );
  await act(async () =>
    useDashboardProfileStore.setState((s) => ({
      profile: { ...s.profile!, slots: { "file-nav": "native" } },
    })),
  );
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 40));
  });
  expect(post.mock.calls.some(([m]) => m.type === "codesense:flush")).toBe(true);
  expect(original.isConnected).toBe(true);
  expect(post.mock.calls.some(([m]) => m.type === "codesense:teardown")).toBe(false);
});

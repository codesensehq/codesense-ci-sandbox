// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
const { scan } = vi.hoisted(() => ({ scan: vi.fn() }));
vi.mock("@/tauri/commands", async () => ({
  ...(await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands")),
  scanPipelineTree: scan,
}));
import { usePipelineTree } from "./use-pipeline-tree";
import { useConfigStore } from "@/shared/stores/config-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { buildTree } from "./test-fixtures/derived-tree";
const flush = async () => {
  for (let i = 0; i < 25; i++) await Promise.resolve();
};
const pending: Array<(tree: ReturnType<typeof buildTree>) => void> = [];
beforeEach(() => {
  vi.useFakeTimers();
  scan.mockReset();
  seedFileRefresh();
  useConfigStore.setState({ initialized: true, repos: [] });
  useViewerStore.setState({ stateLoaded: true, scopeRecoveryComplete: true });
  useOrgStore.setState({
    stateLoaded: true,
    workspaceRepos: {},
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        org_root: "/ctx",
        created_at: "",
        remote_url: null,
        config_flags: {},
        people: [],
        teams: [
          { name: "A", source_repos: [] },
          { name: "B", source_repos: [] },
        ],
      },
    ],
  });
});
afterEach(async () => {
  cleanup();
  pending.splice(0).forEach((finish) => finish(buildTree("")));
  await flush();
  await act(async () => {
    await vi.runAllTimersAsync();
  });
  vi.useRealTimers();
});
describe("content scan lifecycle", () => {
  it("exits waiting after 10 seconds and bounds retries while native work remains outstanding", async () => {
    scan.mockImplementationOnce(() => new Promise((finish) => pending.push(finish)));
    const { result } = renderHook(() => usePipelineTree("team:acme:A", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    await act(async () => {
      vi.advanceTimersByTime(9_850);
      await flush();
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toContain("timed out");
    for (let i = 0; i < 30; i++) {
      await act(async () => {
        useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
        vi.advanceTimersByTime(33);
        await flush();
      });
    }
    expect(result.current.error).toContain("timed out");
    expect(scan).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 3; i++) {
      await act(async () => {
        result.current.refetch();
        vi.advanceTimersByTime(150);
        await flush();
      });
    }
    expect(scan).toHaveBeenCalledTimes(1);
    await act(async () => {
      pending.splice(0).forEach((finish) => finish(buildTree("")));
      await flush();
    });
    expect(result.current.tree).toBeNull();
    scan.mockResolvedValue(buildTree(""));
    await act(async () => {
      result.current.refetch();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeNull();
  });
  it("bounds hydration, ignores late completion and restarts waiting only on retry", async () => {
    useConfigStore.setState({ initialized: false });
    scan.mockResolvedValue(buildTree(""));
    const { result } = renderHook(() => usePipelineTree("team:acme:A", ""));
    await act(async () => {
      vi.advanceTimersByTime(10000);
      await flush();
    });
    expect(result.current.loading).toBe(false);
    expect(result.current.error).toContain("timed out");
    await act(async () => {
      useConfigStore.setState({ initialized: true });
      await flush();
    });
    expect(result.current.error).toContain("timed out");
    expect(scan).not.toHaveBeenCalled();
    await act(async () => {
      result.current.refetch();
      await flush();
    });
    expect(result.current.loading).toBe(true);
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(result.current.tree).not.toBeNull();
  });
  it("publishes a healthy delayed read while watcher events continue", async () => {
    scan.mockImplementation(
      () => new Promise((finish) => setTimeout(() => finish(buildTree("healthy")), 500)),
    );
    const { result } = renderHook(() => usePipelineTree("team:acme:A", ""));
    for (let i = 0; i < 100; i++)
      await act(async () => {
        useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
        vi.advanceTimersByTime(33);
        await flush();
      });
    expect(result.current.tree?.dirName).toBe("healthy");
    expect(result.current.error).toBeNull();
  });
  it("pauses hidden consumers and coalesces their refreshes when visible again", async () => {
    scan.mockResolvedValue(buildTree("fresh"));
    const { result, rerender } = renderHook(
      ({ enabled }) => usePipelineTree("team:acme:A", "", undefined, enabled),
      { initialProps: { enabled: false } },
    );
    for (let i = 0; i < 30; i++)
      await act(async () => {
        useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
        vi.advanceTimersByTime(100);
        await flush();
      });
    expect(scan).not.toHaveBeenCalled();
    rerender({ enabled: true });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(scan).toHaveBeenCalledTimes(1);
    expect(result.current.tree?.dirName).toBe("fresh");
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await flush();
    });
    await act(async () => {
      useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
      vi.advanceTimersByTime(500);
      await flush();
    });
    expect(scan).toHaveBeenCalledTimes(1);
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await flush();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(scan).toHaveBeenCalledTimes(2);
  });
  it("recovers superseded consumers after three scopes contend for one root", async () => {
    scan.mockImplementationOnce(() => new Promise((finish) => pending.push(finish)));
    scan.mockImplementation(async (_root, _pipeline, scope) => buildTree(scope || "root"));
    const first = renderHook(() => usePipelineTree("team:acme:A", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    const second = renderHook(() => usePipelineTree("team:acme:A", "docs"));
    const third = renderHook(() => usePipelineTree("team:acme:A", "plans"));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(scan).toHaveBeenCalledTimes(1);
    await act(async () => {
      pending.splice(0).forEach((finish) => finish(buildTree("root")));
      await flush();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(first.result.current.tree?.dirName).toBe("root");
    expect(second.result.current.tree?.dirName).toBe("docs");
    expect(third.result.current.tree?.dirName).toBe("plans");
    expect(second.result.current.error).toBeNull();
    expect(scan).toHaveBeenCalledTimes(3);
  });
  it("keeps watcher refreshes live after a failed running read and successful queued recovery", async () => {
    let fail!: (error: Error) => void;
    scan.mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          fail = reject;
        }),
    );
    scan.mockResolvedValue(buildTree("recovered"));
    const { result } = renderHook(() => usePipelineTree("team:acme:A", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    await act(async () => {
      useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
      await flush();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    await act(async () => {
      fail(new Error("temporary read failure"));
      await flush();
    });
    expect(result.current.error).toBeNull();
    expect(result.current.tree?.dirName).toBe("recovered");
    expect(scan).toHaveBeenCalledTimes(2);
    await act(async () => {
      useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
      await flush();
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(scan).toHaveBeenCalledTimes(3);
  });
  it("rejects partial directory-read failures", async () => {
    scan.mockResolvedValue(
      buildTree("", {
        subEpics: [
          buildTree("unreadable", { orderError: "Failed to read directory: permission denied" }),
        ],
      }),
    );
    const { result } = renderHook(() => usePipelineTree("team:acme:A", ""));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(result.current.error).toContain("permission denied");
    expect(result.current.tree).toBeNull();
  });
  it("keeps continuous events from starving an idle reader", async () => {
    scan.mockResolvedValue(buildTree(""));
    renderHook(() => usePipelineTree("team:acme:A", ""));
    for (let i = 0; i < 62; i++)
      await act(async () => {
        useFileRefreshStore.getState().bumpFileRefresh("team:acme:A", undefined, true);
        vi.advanceTimersByTime(33);
        await flush();
      });
    expect(scan).toHaveBeenCalledTimes(1);
  });
  it("never publishes a previous team's delayed tree in the newly selected team", async () => {
    scan.mockImplementationOnce(() => new Promise((finish) => pending.push(finish)));
    scan.mockResolvedValue(buildTree("B"));
    const { result, rerender } = renderHook(({ identity }) => usePipelineTree(identity, ""), {
      initialProps: { identity: "team:acme:A" },
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    rerender({ identity: "team:acme:B" });
    expect(result.current.tree).toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flush();
    });
    expect(result.current.tree?.dirName).toBe("B");
    await act(async () => {
      pending.splice(0).forEach((finish) => finish(buildTree("A")));
      await flush();
    });
    expect(result.current.tree?.dirName).toBe("B");
  });
});

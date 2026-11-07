// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { act } from "react";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn(async () => {}),
  pushViewerState: vi.fn(async () => {}),
}));

import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { shouldDropArrowFilter, useStaleArrowFilterCleanup } from "./use-stale-arrow-filter";

type State = Parameters<typeof shouldDropArrowFilter>[0];

function state(over: Partial<State> = {}): State {
  return {
    agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws" },
    agentsPaneHiddenTeams: [],
    workspaces: { t: [] },
    softClosedWorkspaces: {},
    persistedStateRestored: true,
    terminalLayout: { hydrated: true, members: {}, admission: {} } as never,
    ...over,
  } as State;
}
const NO_MACHINES = { byTeam: {}, failedTeams: [] };

describe("the app's own cleanup of a stale arrow filter (the pane not mounted)", () => {
  it("drops a filter on a deleted local workspace once This Mac's state is known", () => {
    expect(shouldDropArrowFilter(state({ persistedStateRestored: false }), NO_MACHINES)).toBe(
      false,
    );
    expect(shouldDropArrowFilter(state(), NO_MACHINES)).toBe(true);
  });

  it("keeps a remote workspace's filter until its machine's layout is held without it", () => {
    const remote = { agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws", machineId: "box" } };
    expect(shouldDropArrowFilter(state(remote), NO_MACHINES)).toBe(false);
    expect(
      shouldDropArrowFilter(
        state({
          ...remote,
          terminalLayout: { hydrated: true, members: { "box/remote": {} }, admission: {} } as never,
        }),
        NO_MACHINES,
      ),
    ).toBe(true);
  });

  it("never drops a filter for holding no agent: it reads no agents", () => {
    expect(
      shouldDropArrowFilter(
        state({ workspaces: { t: [{ id: "ws", kind: "generic", cwd: "/x" } as never] } }),
        NO_MACHINES,
      ),
    ).toBe(false);
  });

  it("drops a filter of a hidden team", () => {
    expect(
      shouldDropArrowFilter(
        state({
          agentsPaneHiddenTeams: ["t"],
          workspaces: { t: [{ id: "ws", kind: "generic", cwd: "/x" } as never] },
        }),
        NO_MACHINES,
      ),
    ).toBe(true);
  });
});

describe("useStaleArrowFilterCleanup, mounted at the app's root", () => {
  it("drops a stale filter once the stores say so, and stops watching once unmounted", () => {
    const layout = useViewerStore.getState().terminalLayout;
    useViewerStore.setState({
      agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws", machineId: "box" },
      agentsPaneHiddenTeams: [],
      workspaces: { t: [] },
      softClosedWorkspaces: {},
      persistedStateRestored: true,
      terminalLayout: { ...layout, hydrated: true, members: {}, admission: {} },
    });
    act(() => useMachinesStore.setState({ byTeam: {}, failedTeams: [] }));
    const { unmount } = renderHook(() => useStaleArrowFilterCleanup());
    // Its machine can still bring the workspace: kept.
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    // The team's machine list loads without the machine: gone, so the filter goes.
    act(() => useMachinesStore.setState({ byTeam: { t: [] } }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
    act(() => useMachinesStore.setState({ byTeam: {}, failedTeams: [] }));

    // Its machine's layout arrives without the workspace: the filter goes.
    act(() =>
      useViewerStore.setState({
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws", machineId: "box" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() =>
      useViewerStore.setState((s) => ({
        terminalLayout: { ...s.terminalLayout, members: { "box/remote": {} as never } },
      })),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    // A deleted local workspace's filter goes once This Mac's saved state is restored.
    act(() =>
      useViewerStore.setState({
        persistedStateRestored: false,
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "gone-here" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() => useViewerStore.setState({ persistedStateRestored: true }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    // ...and once This Mac's own layout is hydrated, when that comes last.
    act(() =>
      useViewerStore.setState((s) => ({
        terminalLayout: { ...s.terminalLayout, hydrated: false },
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "gone-here" },
      })),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() =>
      useViewerStore.setState((s) => ({ terminalLayout: { ...s.terminalLayout, hydrated: true } })),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    // A machine removed from the layout's admission takes a remote filter with no record.
    act(() =>
      useViewerStore.setState({
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws", machineId: "gone" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() =>
      useViewerStore.setState((s) => ({
        terminalLayout: {
          ...s.terminalLayout,
          admission: {
            "gone/remote": {
              state: "removed",
              member: { machineId: "gone", slot: "remote" },
            } as never,
          },
        },
      })),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    // A team hidden from the pane takes its filter with it.
    act(() =>
      useViewerStore.setState({
        workspaces: { t: [{ id: "ws", kind: "generic", cwd: "/x" } as never] },
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() => useViewerStore.setState({ agentsPaneHiddenTeams: ["t"] }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
    act(() => useViewerStore.setState({ agentsPaneHiddenTeams: [] }));

    // The filtered local workspace is deleted, open or minimized: the filter goes.
    const record = { id: "ws", kind: "generic", cwd: "/x" } as never;
    act(() =>
      useViewerStore.setState({
        workspaces: { t: [record] },
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() => useViewerStore.setState({ workspaces: { t: [] } }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
    act(() =>
      useViewerStore.setState({
        softClosedWorkspaces: { t: { ws: { workspace: record } as never } },
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws" },
      }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() => useViewerStore.setState({ softClosedWorkspaces: {} }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    // The team's machine list read fails, then loads without the machine: the filter goes.
    act(() => {
      useMachinesStore.setState({ byTeam: { t: [] }, failedTeams: ["t"] });
      useViewerStore.setState({
        agentsWorkspaceFilter: { teamKey: "t", workspaceId: "ws", machineId: "far" },
      });
    });
    expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
    act(() => useMachinesStore.setState({ failedTeams: [] }));
    expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();

    unmount();
    act(() =>
      useViewerStore.setState({ agentsWorkspaceFilter: { teamKey: "t", workspaceId: "gone" } }),
    );
    expect(useViewerStore.getState().agentsWorkspaceFilter).toEqual({
      teamKey: "t",
      workspaceId: "gone",
    });
    useViewerStore.setState({ agentsWorkspaceFilter: null, terminalLayout: layout });
    act(() => useMachinesStore.setState({ byTeam: {}, failedTeams: [] }));
  });
});

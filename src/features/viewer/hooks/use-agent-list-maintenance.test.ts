// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentListProjectionStore } from "../lib/agent-list-projection";
import { useStaleArrowFilterCleanup } from "./use-stale-arrow-filter";
import { useEndedAgentPins } from "./use-ended-agent-pins";
import type { AgentSessionRow } from "@/tauri/commands";
let loading = false;
vi.mock("./use-agent-layout-loading", () => ({
  useWaitOver: () => false,
  useRemoteLayoutsLoading: () => ({ loading, loadingFor: () => loading }),
}));
const team = "acme:dev";
afterEach(cleanup);
beforeEach(() => {
  loading = false;
  useViewerStore.setState((s) => ({
    persistedStateRestored: true,
    terminalLayout: { ...s.terminalLayout, hydrated: true, byKey: {}, members: {}, admission: {} },
    workspaces: {},
    softClosedWorkspaces: {},
    agentsPaneHiddenTeams: [],
    agentPins: {},
    agentsWorkspaceFilter: null,
  }));
  useAgentListProjectionStore.setState({
    sessions: [],
    references: [],
    error: null,
    partial: false,
  });
});
it("app-root filter owner keeps incomplete placement, then drops the filter after its last agent closes", () => {
  useViewerStore.setState({
    agentsWorkspaceFilter: { teamKey: team, workspaceId: "" },
  });
  useAgentListProjectionStore.setState({ partial: true });
  loading = true;
  const hook = renderHook(useStaleArrowFilterCleanup);
  expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
  act(() => useAgentListProjectionStore.setState({ partial: false }));
  expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
  loading = false;
  act(() =>
    useAgentListProjectionStore.setState({
      references: [{ id: "r", sessionRef: "s", teamKey: team, workspaceRef: null }] as never,
    }),
  );
  expect(useViewerStore.getState().agentsWorkspaceFilter).not.toBeNull();
  act(() => useAgentListProjectionStore.setState({ references: [] }));
  expect(useViewerStore.getState().agentsWorkspaceFilter).toBeNull();
  hook.unmount();
});
it("app-root pin owner waits for full roster and layouts, keeps moved live sessions, then prunes ended pins", () => {
  const unpin = vi.spyOn(useViewerStore.getState(), "unpinAgents");
  useViewerStore.setState({
    agentPins: {
      [team]: [{ kind: "session", teamKey: team, handle: "worker", sessionId: "s" }],
    } as never,
  });
  useAgentListProjectionStore.setState({ partial: true });
  loading = true;
  renderHook(useEndedAgentPins);
  expect(unpin).not.toHaveBeenCalled();
  act(() => useAgentListProjectionStore.setState({ partial: false }));
  expect(unpin).not.toHaveBeenCalled();
  loading = false;
  act(() =>
    useAgentListProjectionStore.setState({
      sessions: [{ id: "s", sessionId: "s", handle: "worker", alive: true }] as AgentSessionRow[],
    }),
  );
  expect(unpin).not.toHaveBeenCalled();
  act(() => useAgentListProjectionStore.setState({ sessions: [] }));
  expect(unpin).toHaveBeenCalledWith(team, [expect.any(String)]);
  unpin.mockRestore();
});

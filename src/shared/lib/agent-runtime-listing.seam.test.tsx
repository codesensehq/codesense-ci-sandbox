// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { createElement } from "react";
import modelSchemas from "../../../src-tauri/src/docs_schemas.generated.json";

vi.mock("@/features/machines/stores/machines-store", () => ({ useTeamMachines: () => [] }));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: [], error: null }),
}));
vi.mock("@/features/story-launch/story-agent-launch", () => ({ orderStoryLaunchAgents: () => [] }));
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ principal: "alice" }),
}));
vi.mock("@/features/viewer/lib/cli-workspace/store/viewer-access", () => ({
  getViewerState: () => ({ workspaces: {} }),
}));
vi.mock("@/features/viewer/lib/terminal-tab-keys", () => ({ newTerminalTab: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.doUnmock("../../../src-tauri/src/docs_schemas.generated.json");
});

describe("backend listing drives every fresh-launch entry; registration drives recognition", () => {
  it.each([false, true])(
    "Hermes listed=%s: runtime picker, chooser and dependency mocks follow listing; a running Hermes is still recognized",
    async (listed) => {
      vi.resetModules();
      // Only the backend listing fact changes. All registered metadata stays intact.
      vi.doMock("../../../src-tauri/src/docs_schemas.generated.json", () => ({
        default: {
          ...modelSchemas,
          agentClis: modelSchemas.agentClis.map((row) =>
            row.runtime === "hermes" ? { ...row, listed } : row,
          ),
        },
      }));
      const registry = await import("./agent-runtimes");
      expect(registry.isAgentRuntime("hermes")).toBe(listed);
      const { mockDependencyStatus } = await import("@/app/mock/fixtures");
      expect("hermes" in mockDependencyStatus.agents).toBe(listed);
      expect(Object.keys(mockDependencyStatus.agents)).toEqual(registry.AGENT_RUNTIMES);
      const { prepareAgentLaunch } = await import("@/app/mock/handlers/agents-launch");
      const prepareMock = () =>
        prepareAgentLaunch({ agentKey: "acme/backend/hermes-open", sessionId: "listing-session" });
      if (listed) expect(prepareMock).not.toThrow();
      else expect(prepareMock).toThrow(/no agent.*hermes/);

      const { emptyDraft } = await import("@/features/agents/lib/agent-form");
      const { AgentRunsOnFields } = await import("@/features/agents/components/agent-form-fields");
      const { TooltipProvider } = await import("@/components/ui/tooltip");
      render(
        createElement(
          TooltipProvider,
          null,
          createElement(AgentRunsOnFields, {
            draft: emptyDraft(),
            onChange: vi.fn(),
            escalating: false,
          }),
        ),
      );
      fireEvent.click(screen.getByRole("combobox", { name: "Runtime" }));
      // A Preview CLI is named with its badge's word.
      expect(screen.queryByRole("option", { name: /^Hermes( Preview)?$/ }) !== null).toBe(listed);
      cleanup();
      const { NewTerminalChooser } =
        await import("@/features/viewer/components/new-terminal-chooser");
      const { NEW_WORKSPACE_TERMINAL_EVENT } =
        await import("@/features/viewer/lib/new-workspace-terminal");
      render(createElement(NewTerminalChooser, { teamKey: "acme/default", workspaceId: "ws" }));
      act(() =>
        window.dispatchEvent(
          new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
            detail: { teamKey: "acme/default", workspaceId: "ws" },
          }),
        ),
      );
      expect(screen.queryByRole("button", { name: /^Hermes( Preview)?$/ }) !== null).toBe(listed);
      cleanup();
      const { useAgentStateStore } = await import("@/shared/stores/agent-state-store");
      const { useRunningAgent } = await import("@/features/viewer/lib/hand-agents");
      useAgentStateStore.setState({
        bySession: {},
        activityBySession: {},
        exitBySession: {},
        processAgentBySession: { "listing-session": "hermes" },
      });
      const { result } = renderHook(() => useRunningAgent("listing-session"));
      // A running agent is recognized by registration, listed or not: its inbox stays reachable.
      expect(result.current).toBe(true);
    },
  );
});

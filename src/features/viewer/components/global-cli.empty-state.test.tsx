// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { workspaceGroupKey } from "@/shared/lib/workspace-key";
import { readEpicRepos, setEpicRepos } from "@/tauri/commands";

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  readEpicRepos: vi.fn(async () => []),
  setEpicRepos: vi.fn(async (_path: string, repos: string[]) => repos),
}));

const content = vi.hoisted(() => ({ kind: "loading" }));
vi.mock("@/shared/hooks/use-team-content-context", () => ({
  useTeamContentContext: () =>
    content.kind === "ready"
      ? { kind: "ready", source: { teamContentPath: "/team" } }
      : { kind: content.kind, error: "Unavailable" },
}));
vi.mock("./workspace-terminal-panels", () => ({
  WorkspaceTerminalPanels: () => <div data-testid="panels" />,
  WorkspaceInitOverlay: () => null,
}));
vi.mock("./agent-run-output-host", () => ({
  AgentRunOutputHost: ({ visible }: { visible?: boolean }) => (
    <div data-testid="run-review-host" data-shown={String(visible)} />
  ),
}));
import { GlobalCli } from "./global-cli";

const originalActions = {
  openWorkspace: useViewerStore.getState().openWorkspace,
  addTerminalTab: useViewerStore.getState().addTerminalTab,
  reopenSoftClosedWorkspace: useViewerStore.getState().reopenSoftClosedWorkspace,
};
const TEAM = "acme:alpha";
const workspace = {
  id: "focused",
  kind: "generic" as const,
  cwd: "/focused",
  status: "ready" as const,
};
const mount = () =>
  render(
    <TooltipProvider>
      <GlobalCli teamKey={TEAM} />
    </TooltipProvider>,
  );

beforeEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  installMockBackend();
  content.kind = "loading";
  vi.mocked(readEpicRepos).mockReset().mockResolvedValue([]);
  vi.mocked(setEpicRepos).mockClear();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "alpha" });
  useViewerStore.setState({
    ...originalActions,
    openedTeams: [TEAM],
    workspaces: {},
    activeWorkspace: {},
    softClosedWorkspaces: {},
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    trayMode: {},
    layoutMode: "console",
    terminalVisible: true,
  });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each(["loading", "unavailable"])(
  "explains and disables Start while team content is %s",
  (kind) => {
    content.kind = kind;
    mount();
    const start = screen.getByRole("button", { name: "Start an agent" });
    expect(start).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent(
      kind === "loading" ? "Loading team content" : "Team content is unavailable",
    );
  },
);

it("starts an agent through the real GlobalCli in the team's folder", () => {
  content.kind = "ready";
  const open = vi.spyOn(useViewerStore.getState(), "openWorkspace");
  const add = vi.spyOn(useViewerStore.getState(), "addTerminalTab").mockResolvedValue(undefined);
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Start an agent" }));
  expect(open).toHaveBeenCalledWith(
    expect.objectContaining({ teamKey: TEAM, cwd: "/team", focus: true, spawn: false }),
  );
  expect(add).toHaveBeenCalledWith(TEAM, expect.objectContaining({ name: "Agent" }));
});

it("restores the focused minimized workspace before adding the agent, even without loaded team content", () => {
  useViewerStore.setState({
    activeWorkspace: { [TEAM]: workspace.id },
    softClosedWorkspaces: { [TEAM]: { [workspace.id]: { workspace } } },
  });
  const restore = vi.spyOn(useViewerStore.getState(), "reopenSoftClosedWorkspace");
  const add = vi.spyOn(useViewerStore.getState(), "addTerminalTab").mockResolvedValue(undefined);
  const open = vi.spyOn(useViewerStore.getState(), "openWorkspace");
  mount();
  expect(screen.getByRole("button", { name: "Start an agent" })).toBeEnabled();
  act(() => fireEvent.click(screen.getByRole("button", { name: "Start an agent" })));
  expect(restore).toHaveBeenCalledWith(TEAM, workspace.id);
  expect(useViewerStore.getState().workspaces[TEAM]).toContainEqual(workspace);
  expect(add).toHaveBeenCalledWith(TEAM, expect.objectContaining({ name: "Agent" }));
  expect(open).not.toHaveBeenCalled();
  expect(restore.mock.invocationCallOrder[0]).toBeLessThan(add.mock.invocationCallOrder[0]);
});

it("keeps the CLI host while isolating repo-card state across teams with colliding workspace ids", async () => {
  const other = "acme:beta";
  let finishRead: (repos: string[]) => void = () => {};
  const slowRead = new Promise<string[]>((resolve) => {
    finishRead = resolve;
  });
  vi.mocked(readEpicRepos).mockImplementation(async (path) =>
    path === "/alpha/epic.md" ? ["alpha/repo"] : slowRead,
  );
  const epicA = { ...workspace, epicDirPath: "epic", epicDocPath: "/alpha/epic.md" };
  const epicB = { ...workspace, epicDirPath: "epic", epicDocPath: "/beta/epic.md" };
  useViewerStore.setState({
    workspaces: { [TEAM]: [epicA], [other]: [epicB] },
    activeWorkspace: { [TEAM]: workspace.id, [other]: workspace.id },
    epicReposOpen: { [workspaceGroupKey(other, workspace.id)]: true },
  });
  const { rerender } = mount();
  const host = screen.getByTestId("global-cli");
  // Let the initial nonempty evaluation settle, then manually open the card.
  await act(async () => {});
  act(() =>
    useViewerStore.getState().setEpicReposOpen(workspaceGroupKey(TEAM, workspace.id), true),
  );
  expect(await screen.findByText("alpha/repo")).toBeInTheDocument();
  act(() => useOrgStore.setState({ activeTeamName: "beta" }));
  rerender(
    <TooltipProvider>
      <GlobalCli teamKey={other} />
    </TooltipProvider>,
  );
  expect(screen.getByTestId("global-cli")).toBe(host);
  // Team alpha stays warm, hidden, and draws no card; team beta never shows alpha's.
  expect(screen.queryByText("alpha/repo")).not.toBeInTheDocument();
  await act(async () => finishRead([]));
  expect(screen.queryByText("alpha/repo")).not.toBeInTheDocument();
  expect(setEpicRepos).not.toHaveBeenCalled();
});

it("reveals the chosen workspace's rail when Start is clicked from an empty epic", () => {
  const epic = {
    ...workspace,
    kind: "epic" as const,
    epicDirPath: "alpha",
    epicDocPath: "/alpha/epic.md",
  };
  useViewerStore.setState({
    workspaces: { [TEAM]: [epic] },
    activeWorkspace: { [TEAM]: workspace.id },
    trayMode: { [TEAM]: "grouped" },
    selectedEpicDirName: { [TEAM]: "empty-beta" },
    railKeyByWorkspaceId: { [TEAM]: { [workspace.id]: "alpha" } },
    epicReposOpen: {},
  });
  const add = vi.spyOn(useViewerStore.getState(), "addTerminalTab").mockResolvedValue(undefined);
  mount();
  fireEvent.click(screen.getByRole("button", { name: "Start an agent" }));
  expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("alpha");
  expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(workspace.id);
  expect(add).toHaveBeenCalledWith(TEAM, expect.objectContaining({ name: "Agent" }));
});

it("passes collapsed CLI visibility to the retained run review host", () => {
  mount();
  expect(screen.getByTestId("run-review-host")).toHaveAttribute("data-shown", "true");
  act(() => useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false }));
  expect(screen.getByTestId("run-review-host")).toHaveAttribute("data-shown", "false");
});

it("keeps the hide line after the one-time shortcuts hint is dismissed (SV-GHI-2)", () => {
  useViewerStore.setState({ cliShortcutsHintSeen: true });
  try {
    mount();
    expect(screen.getByTestId("terminal-empty-state")).toBeInTheDocument();
    expect(screen.queryByTestId("cli-shortcuts-hint")).toBeNull();
    expect(screen.getByTestId("terminal-empty-state-hide-hint")).toHaveTextContent(
      "hides the terminal",
    );
  } finally {
    useViewerStore.setState({ cliShortcutsHintSeen: false });
  }
});

// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AGENT_RUNTIMES,
  PREVIEW_RUNTIME_NOTE,
  REGISTERED_AGENT_CLIS,
  REMOTE_LAUNCH_CLIS,
} from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import type { AgentRuntime } from "@/tauri/commands";

vi.mock("sonner", () => ({ toast: { warning: vi.fn() } }));

const fake = vi.hoisted(() => ({
  epoch: "1" as string | null,
  list: vi.fn(),
  open: vi.fn(),
  local: vi.fn().mockResolvedValue({ sessionId: "hermes-local" }),
  machines: [
    { machineId: "box", kind: "ssh", label: "Box", status: { state: "connected" } },
    { machineId: "other", kind: "ssh", label: "Other", status: { state: "connected" } },
  ],
  agents: [] as {
    agentKey: string;
    name: string;
    runtime: AgentRuntime;
    place: string | null;
  }[],
}));
vi.mock("@/features/machines/stores/machines-store", () => ({
  useTeamMachines: () => fake.machines,
}));
vi.mock("@/features/machines/lib/machine-actions", () => ({
  connectMachineFor: vi.fn(),
  openMachinesPanel: vi.fn(),
}));
vi.mock("@/features/machines/lib/machine-status", () => ({
  needsPrivateAccount: () => false,
  describeMachineStatus: () => ({ actions: [] }),
  PRIVATE_ACCOUNT_FOR_TABS: "private",
}));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: [], error: null }),
}));
vi.mock("@/features/story-launch/story-agent-launch", () => ({
  launchAgentInWorkspace: (...args: unknown[]) => fake.local(...args),
  orderStoryLaunchAgents: () => fake.agents,
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ principal: "alice" }),
}));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  currentPrivateAuthorityEpoch: () => fake.epoch,
  onPrivateAuthorityRetired: () => () => {},
}));
vi.mock("@/tauri/commands", () => ({
  listPreparedRemoteWorkspaces: (...args: unknown[]) => fake.list(...args),
}));
vi.mock("../lib/remote-tab-open", () => ({
  openRemoteTab: (...args: unknown[]) => fake.open(...args),
}));
vi.mock("@/features/viewer/lib/cli-workspace/store/viewer-access", () => ({
  getViewerState: () => ({ workspaces: { "acme/default": [{ id: "ws", cwd: "/local/tree" }] } }),
}));
vi.mock("../lib/terminal-tab-keys", () => ({ newTerminalTab: vi.fn() }));
import {
  NEW_WORKSPACE_TERMINAL_EVENT,
  requestNewWorkspaceTerminal,
} from "../lib/new-workspace-terminal";
import { NewTerminalChooser } from "./new-terminal-chooser";

const choice = { handle: "prepared-1", cwd: "/remote/tree", branch: "main", sourceRepo: "source" };
function openChooser() {
  render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
  act(() =>
    window.dispatchEvent(
      new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
        detail: { teamKey: "acme/default", workspaceId: "ws" },
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Saved Agent"), {
    target: { value: "acme/default/builder" },
  });
  fireEvent.click(screen.getByTestId("new-terminal-where-box"));
}
beforeEach(() => {
  vi.mocked(toast.warning).mockClear();
  fake.epoch = "1";
  fake.agents = [
    { agentKey: "acme/default/builder", name: "Builder", runtime: "codex", place: null },
  ];
  fake.list.mockReset();
  fake.open.mockReset();
  fake.local.mockClear();
  localStorage.clear();
});
afterEach(cleanup);
describe("saved Agent in the active terminal chooser", () => {
  it("does not offer a remote launch for an OpenCode agent", () => {
    fake.agents = [
      {
        agentKey: "acme/default/opencode",
        name: "OpenCode",
        runtime: "opencode",
        place: null,
      },
    ];
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    act(() =>
      window.dispatchEvent(
        new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
          detail: { teamKey: "acme/default", workspaceId: "ws" },
        }),
      ),
    );
    fireEvent.change(screen.getByLabelText("Saved Agent"), {
      target: { value: "acme/default/opencode" },
    });
    expect(screen.getByTestId("new-terminal-where-box")).toBeDisabled();
    expect(fake.list).not.toHaveBeenCalled();
  });

  it.each(AGENT_RUNTIMES.filter((runtime) => !REMOTE_LAUNCH_CLIS.some((cli) => cli === runtime)))(
    "launches local %s through managed preparation and refuses remote",
    async (runtime) => {
      fake.agents = [{ agentKey: `builtin/${runtime}`, name: runtime, runtime, place: null }];
      render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
      act(() =>
        window.dispatchEvent(
          new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
            detail: { teamKey: "acme/default", workspaceId: "ws" },
          }),
        ),
      );
      fireEvent.change(screen.getByLabelText("Saved Agent"), {
        target: { value: `builtin/${runtime}` },
      });
      expect(screen.getByTestId("new-terminal-where-box")).toBeDisabled();
      fireEvent.click(screen.getByTestId("new-terminal-where-local"));
      await waitFor(() =>
        expect(fake.local).toHaveBeenCalledWith({
          teamKey: "acme/default",
          workspaceKey: "ws",
          agent: `builtin/${runtime}`,
        }),
      );
      expect(fake.open).not.toHaveBeenCalled();
    },
  );

  it("requires a verified prepared choice and sends the saved identity and exact cwd", async () => {
    fake.list.mockResolvedValue([choice]);
    openChooser();
    expect(fake.open).not.toHaveBeenCalled();
    await screen.findByRole("option", { name: "main · /remote/tree" });
    fireEvent.change(screen.getByLabelText("Prepared workspace"), {
      target: { value: choice.handle },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open Agent" }));
    expect(fake.open).toHaveBeenCalledWith(
      expect.objectContaining({
        agentKey: "acme/default/builder",
        machineId: "box",
        preparedWorkspaceHandle: "prepared-1",
        cwd: "/remote/tree",
      }),
    );
    expect(screen.queryByRole("dialog")).toBeNull();
  });
  it("drops stale choices after machine changes and refreshes without launching", async () => {
    let resolveOld!: (value: (typeof choice)[]) => void;
    fake.list
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveOld = resolve;
          }),
      )
      .mockResolvedValue([]);
    openChooser();
    fireEvent.click(screen.getByTestId("new-terminal-where-other"));
    await waitFor(() => expect(fake.list).toHaveBeenCalledTimes(2));
    await act(async () => resolveOld([choice]));
    expect(screen.queryByRole("option", { name: "main · /remote/tree" })).toBeNull();
    fake.list.mockResolvedValue([choice]);
    fireEvent.click(screen.getByRole("button", { name: "Refresh workspaces" }));
    await screen.findByRole("option", { name: "main · /remote/tree" });
    expect(fake.open).not.toHaveBeenCalled();
  });
  it("does not launch a prepared choice after its private authority retires", async () => {
    fake.list.mockResolvedValue([choice]);
    openChooser();
    await screen.findByRole("option", { name: "main · /remote/tree" });
    fireEvent.change(screen.getByLabelText("Prepared workspace"), {
      target: { value: choice.handle },
    });
    fake.epoch = null;
    fireEvent.click(screen.getByRole("button", { name: "Open Agent" }));
    expect(fake.open).not.toHaveBeenCalled();
  });
});

describe("chooser completion ownership", () => {
  it("does not close a reopened chooser when the dismissed launch finishes", async () => {
    let finish!: (v: unknown) => void;
    fake.local.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    const reopen = () =>
      act(() =>
        window.dispatchEvent(
          new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
            detail: { teamKey: "acme/default", workspaceId: "ws" },
          }),
        ),
      );
    reopen();
    fireEvent.change(screen.getByLabelText("Saved Agent"), {
      target: { value: "acme/default/builder" },
    });
    fireEvent.click(screen.getByTestId("new-terminal-where-local"));
    await waitFor(() => expect(fake.local).toHaveBeenCalled());
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    reopen();
    expect(screen.getByRole("dialog")).toBeTruthy();
    await act(async () => {
      finish({ sessionId: "first-launch" });
      await Promise.resolve();
    });
    expect(screen.queryByRole("dialog")).not.toBeNull();
  });
});

describe("chooser destination intent", () => {
  it("allows choosing a saved local-only agent after changing remote intent to OpenCode", async () => {
    fake.agents.push({
      agentKey: "acme/default/opencode",
      name: "My OpenCode",
      runtime: "opencode",
      place: null,
    });
    fake.list.mockResolvedValue([choice]);
    openChooser();
    await screen.findByRole("option", { name: "main · /remote/tree" });
    fireEvent.click(screen.getByRole("button", { name: "OpenCode" }));
    expect(screen.queryByRole("option", { name: "My OpenCode" })).not.toBeNull();
  });
});

it("shows launch warnings returned by local managed preparation", async () => {
  fake.local.mockResolvedValueOnce({
    sessionId: "warning-launch",
    warnings: [
      { code: "cli_version_unqualified", message: "Installed CLI version is not qualified" },
    ],
  });
  render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
  act(() =>
    window.dispatchEvent(
      new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
        detail: { teamKey: "acme/default", workspaceId: "ws" },
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Saved Agent"), {
    target: { value: "acme/default/builder" },
  });
  fireEvent.click(screen.getByTestId("new-terminal-where-local"));
  await waitFor(() => expect(fake.local).toHaveBeenCalled());
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(toast.warning).toHaveBeenCalledWith("Installed CLI version is not qualified");
});

it("shows warnings after the launching chooser was dismissed", async () => {
  let finish!: (v: unknown) => void;
  fake.local.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
  act(() =>
    window.dispatchEvent(
      new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
        detail: { teamKey: "acme/default", workspaceId: "ws" },
      }),
    ),
  );
  fireEvent.change(screen.getByLabelText("Saved Agent"), {
    target: { value: "acme/default/builder" },
  });
  fireEvent.click(screen.getByTestId("new-terminal-where-local"));
  await waitFor(() => expect(fake.local).toHaveBeenCalled());
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  await act(async () => {
    finish({
      sessionId: "warning-launch",
      warnings: [
        { code: "cli_version_unqualified", message: "Installed CLI version is not qualified" },
      ],
    });
  });
  expect(toast.warning).toHaveBeenCalledWith("Installed CLI version is not qualified");
  expect(screen.queryByRole("dialog")).toBeNull();
});

describe("the chooser names each CLI, a Preview one with its badge", () => {
  const open = (detail: Record<string, unknown> = {}) => {
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    act(() =>
      window.dispatchEvent(
        new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, {
          detail: { teamKey: "acme/default", workspaceId: "ws", ...detail },
        }),
      ),
    );
  };

  it.each(REGISTERED_AGENT_CLIS.filter((cli) => cli.listed))(
    "$runtime: its button carries Preview only when the registry says so",
    ({ runtime, preview }) => {
      open();
      const button = screen.getByRole("button", { name: new RegExp(`^${agentCliLabel(runtime)}`) });
      expect(button.querySelector('[data-testid="preview-badge"]') !== null).toBe(preview);
      if (preview) {
        expect(button).toHaveAccessibleName(`${agentCliLabel(runtime)} Preview`);
        expect(button).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);
      } else expect(button).toHaveAccessibleName(agentCliLabel(runtime));
    },
  );

  it.each(AGENT_RUNTIMES)(
    "Start managed %s opens with that CLI and the tab's folder chosen, on This Mac",
    async (runtime) => {
      open({ launch: runtime, cwd: "/work/story", machineId: "local" });
      const button = screen.getByRole("button", { name: new RegExp(`^${agentCliLabel(runtime)}`) });
      expect(button).toHaveAttribute("aria-pressed", "true");
      expect(screen.getByTestId("new-terminal-where-local")).toHaveFocus();
      fireEvent.click(screen.getByTestId("new-terminal-where-local"));
      await waitFor(() =>
        expect(fake.local).toHaveBeenCalledWith({
          teamKey: "acme/default",
          workspaceKey: "ws",
          agent: `builtin/${runtime}`,
          cwd: "/work/story",
        }),
      );
    },
  );
});

describe("a chooser asked for before its workspace is on screen", () => {
  it("opens with the request's choices when it mounts, and says where it opens", async () => {
    requestNewWorkspaceTerminal("acme/default", "ws", {
      launch: "codex",
      cwd: "/work/story",
      machineId: "local",
    });
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    const dialog = await screen.findByTestId("new-terminal-chooser");
    expect(screen.getByRole("button", { name: /^Codex/ })).toHaveAttribute("aria-pressed", "true");
    expect(dialog).toHaveTextContent("Opens in /work/story");
  });

  it("a request is taken once: a later mount opens nothing", () => {
    requestNewWorkspaceTerminal("acme/default", "ws", {
      launch: "codex",
      cwd: null,
      machineId: "local",
    });
    const first = render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    expect(screen.getByTestId("new-terminal-chooser")).toBeInTheDocument();
    first.unmount();
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    expect(screen.queryByTestId("new-terminal-chooser")).toBeNull();
  });
});

describe("a second request into a chooser that is already open", () => {
  it("takes the new request's CLI and folder, never the old CLI with the new folder", () => {
    render(<NewTerminalChooser teamKey="acme/default" workspaceId="ws" />);
    act(() =>
      requestNewWorkspaceTerminal("acme/default", "ws", {
        launch: "codex",
        cwd: "/work/a",
        machineId: "local",
      }),
    );
    expect(screen.getByRole("button", { name: /^Codex/ })).toHaveAttribute("aria-pressed", "true");
    act(() =>
      requestNewWorkspaceTerminal("acme/default", "ws", {
        launch: "claude",
        cwd: "/work/b",
        machineId: "local",
      }),
    );
    expect(screen.getByRole("button", { name: /^Claude/ })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /^Codex/ })).toHaveAttribute("aria-pressed", "false");
    expect(screen.getByTestId("new-terminal-chooser")).toHaveTextContent("Opens in /work/b");
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { IncompatibleTerminalServer } from "@/tauri/commands";
import { resetTerminalServerForTests } from "../lib/terminal-server";
import {
  closeMachineTerminalServerPrompt,
  setUpdateAllRunning,
  openMachineTerminalServerPrompt,
  promptMachineTerminalServerUpdate,
  resetMachineTerminalServerPromptForTests,
} from "../lib/machine-terminal-server";
import { TerminalServerNotice } from "./terminal-server-notice";
import { TerminalServerPrompt } from "./terminal-server-prompt";

const commands = vi.hoisted(() => ({
  terminalServerStatus: vi.fn(),
  restartTerminalServer: vi.fn(),
  prepareMachineTerminalServerReplacement: vi.fn(),
  restartMachineTerminalServer: vi.fn(),
  machineTerminalServerUpdateMode: vi.fn(),
  reinstallMachineTerminalServer: vi.fn(),
  localTerminalServerUpdate: vi.fn(),
  updateLocalTerminalServer: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  ...commands,
}));

const navigation = vi.hoisted(() => ({ openMachinesPanel: vi.fn() }));
vi.mock("@/features/machines/lib/machine-actions", () => navigation);

const machineStore = vi.hoisted(() => ({ refreshMachine: vi.fn() }));
vi.mock("@/features/machines/stores/machines-store", () => machineStore);

const listeners = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return Promise.resolve(() => listeners.delete(name));
  }),
}));

const toasts = vi.hoisted(() => ({
  warning: vi.fn(),
  success: vi.fn(),
  dismiss: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: toasts }));

const skew = (liveSessions: number | null): IncompatibleTerminalServer => ({
  liveSessions,
  serverProtocol: 3,
  appProtocol: 4,
  reason: "restart terminal server (ends 3 sessions): it speaks protocol 3, this app 4",
});

function emit(name: string, payload: unknown) {
  act(() => listeners.get(name)?.({ payload }));
}

/** The prompt, and the terminal panel's notice beside it. */
function renderShell() {
  return render(
    <>
      <TerminalServerPrompt />
      <TerminalServerNotice />
    </>,
  );
}

const restartButton = (name: string) => screen.findByRole("button", { name });

describe("TerminalServerPrompt", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listeners.clear();
    resetTerminalServerForTests();
    resetMachineTerminalServerPromptForTests();
    commands.terminalServerStatus.mockResolvedValue(null);
    commands.restartTerminalServer.mockResolvedValue(3);
    commands.machineTerminalServerUpdateMode.mockResolvedValue({ mode: "replace", sessions: null });
    commands.prepareMachineTerminalServerReplacement.mockResolvedValue({
      serverToken: "one-use-token",
      count: 3,
    });
    commands.restartMachineTerminalServer.mockResolvedValue({ status: "replaced", count: 3 });
  });

  it("renders nothing while the terminal server is compatible", async () => {
    renderShell();
    await waitFor(() => expect(commands.terminalServerStatus).toHaveBeenCalled());
    expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument();
    expect(screen.queryByTestId("terminal-server-notice")).not.toBeInTheDocument();
  });

  it("says what restarting closes, in terminals, without protocol numbers", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    renderShell();
    const button = await restartButton("Restart terminal server (closes 3 terminals)");
    expect(button).toHaveAttribute("data-destructive", "true");
    const prompt = screen.getByTestId("terminal-server-prompt");
    expect(prompt).toHaveTextContent(
      "A terminal server from another version of CodeSense is still running on this machine, so new terminals can't open. Restarting it closes the 3 terminals it's running. Anything still running in them stops, and their scrollback is lost.",
    );
    expect(prompt).not.toHaveTextContent(/protocol|session/i);
  });

  it("names one terminal in the singular", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(1));
    renderShell();
    expect(await restartButton("Restart terminal server (closes 1 terminal)")).toBeInTheDocument();
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "Restarting it closes the terminal it's running. Anything still running in it stops, and its scrollback is lost.",
    );
  });

  it("offers a plain, non-destructive restart when nothing would be lost", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(0));
    renderShell();
    const button = await restartButton("Restart terminal server");
    expect(button).toHaveAttribute("data-destructive", "false");
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "It isn't running any terminals, so nothing is lost.",
    );
  });

  it("does not invent a count for a server too old to give one", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(null));
    renderShell();
    expect(
      await restartButton("Restart terminal server (closes its terminals)"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "Restarting it closes the terminals it's running.",
    );
  });

  it("prompts when an incompatible server is found later", async () => {
    renderShell();
    await waitFor(() => expect(listeners.has("terminal-server-incompatible")).toBe(true));
    emit("terminal-server-incompatible", skew(1));
    expect(await restartButton("Restart terminal server (closes 1 terminal)")).toBeInTheDocument();
  });

  it("restarts the server and confirms the count the restart reports", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockResolvedValue(2);
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    await waitFor(() =>
      expect(toasts.success).toHaveBeenCalledWith("Terminal server restarted", {
        description: "2 terminals closed. New terminals open normally.",
      }),
    );
    expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument();
    expect(screen.queryByTestId("terminal-server-notice")).not.toBeInTheDocument();
  });

  it("says only that terminals open normally when the old server did not count", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(null));
    commands.restartTerminalServer.mockResolvedValue(null);
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes its terminals)"));
    await waitFor(() =>
      expect(toasts.success).toHaveBeenCalledWith("Terminal server restarted", {
        description: "New terminals open normally.",
      }),
    );
  });

  it("shows the next step when the restart fails, and keeps the cost", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockRejectedValue({ oldStopped: false, message: "timed out" });
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't restart the terminal server. Try again, or quit and reopen CodeSense.",
    );
    expect(
      screen.getByRole("button", { name: "Restart terminal server (closes 3 terminals)" }),
    ).toBeInTheDocument();
  });

  it("offers to try again once the old server stopped but the new one didn't start", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockRejectedValueOnce({
      oldStopped: true,
      message: "the new terminal server did not start",
    });
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    const again = await restartButton("Try again");
    const prompt = screen.getByTestId("terminal-server-prompt");
    expect(
      screen.getByRole("heading", { name: "Couldn't start the terminal server" }),
    ).toBeInTheDocument();
    expect(prompt).toHaveTextContent(
      "The old terminal server stopped, but the new one didn't start.",
    );
    expect(prompt).not.toHaveTextContent(/closes/);
    expect(screen.queryByRole("alert"), "no second, duplicate failure").not.toBeInTheDocument();
    expect(again).toHaveAttribute("data-destructive", "false");
    await waitFor(() => expect(screen.getByRole("button", { name: "Not now" })).toHaveFocus());

    commands.restartTerminalServer.mockResolvedValueOnce(null);
    await userEvent.click(again);
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument(),
    );
  });

  it("retries the restart itself after a failure", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockRejectedValueOnce({
      oldStopped: false,
      message: "timed out",
    });
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    await screen.findByRole("alert");
    commands.restartTerminalServer.mockResolvedValueOnce(null);
    await userEvent.click(
      screen.getByRole("button", { name: "Restart terminal server (closes 3 terminals)" }),
    );
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument(),
    );
    expect(commands.restartTerminalServer).toHaveBeenCalledTimes(2);
  });

  it("declining after the old server stopped says terminals won't survive quit, and leaves no stale notice", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockRejectedValueOnce({ oldStopped: true, message: "no start" });
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    await restartButton("Try again");
    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument(),
    );
    expect(toasts.warning).toHaveBeenCalledWith(
      "Terminals will open, but they'll close when you quit CodeSense. The terminal server will start again next time you open CodeSense.",
    );
    expect(screen.queryByTestId("terminal-server-notice")).not.toBeInTheDocument();
  });

  it("returns focus to Not now when the restart fails", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    commands.restartTerminalServer.mockRejectedValueOnce({
      oldStopped: false,
      message: "timed out",
    });
    renderShell();
    await userEvent.click(await restartButton("Restart terminal server (closes 3 terminals)"));
    await screen.findByRole("alert");
    expect(
      screen.getByRole("heading", { name: "Restart the terminal server?" }),
    ).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Not now" })).toHaveFocus());
  });

  it("declining leaves the notice with the way back, not a header-covering toast", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    renderShell();
    await userEvent.click(await screen.findByRole("button", { name: "Not now" }));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument(),
    );
    expect(toasts.warning).not.toHaveBeenCalled();
    expect(commands.restartTerminalServer).not.toHaveBeenCalled();
    expect(screen.getByTestId("terminal-server-notice")).toHaveTextContent(
      "The terminal server from another CodeSense version is still running your 3 terminals. Restart it to open new ones. This closes those 3.",
    );

    emit("terminal-server-incompatible", skew(3));
    expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server…" }));
    expect(await screen.findByTestId("terminal-server-prompt")).toBeInTheDocument();
  });

  it("re-offers the restart when the user tries to open a terminal after declining", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    renderShell();
    await userEvent.click(await screen.findByRole("button", { name: "Not now" }));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument(),
    );
    emit("terminal-server-refused", skew(3));
    expect(await screen.findByTestId("terminal-server-prompt")).toBeInTheDocument();
  });

  it("settles the notice when the server was replaced before the user came back", async () => {
    commands.terminalServerStatus.mockResolvedValue(skew(3));
    renderShell();
    await userEvent.click(await screen.findByRole("button", { name: "Not now" }));
    commands.terminalServerStatus.mockResolvedValue(null);
    await userEvent.click(await screen.findByRole("button", { name: "Restart terminal server…" }));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-server-notice")).not.toBeInTheDocument(),
    );
    expect(screen.queryByTestId("terminal-server-prompt")).not.toBeInTheDocument();
  });

  it("reports a server that restarted under the app, in terminals", async () => {
    renderShell();
    await waitFor(() => expect(listeners.has("terminal-server-restarted")).toBe(true));
    emit("terminal-server-restarted", { endedSessions: 3 });
    expect(toasts.warning).toHaveBeenCalledWith("The terminal server restarted", {
      description: "3 terminals that were open closed with it.",
    });
    emit("terminal-server-restarted", { endedSessions: 1 });
    expect(toasts.warning).toHaveBeenLastCalledWith("The terminal server restarted", {
      description: "1 terminal that was open closed with it.",
    });
    emit("terminal-server-restarted", { endedSessions: 0 });
    expect(toasts.warning).toHaveBeenCalledTimes(2);
  });
});

describe("TerminalServerPrompt remote Machine replacement", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listeners.clear();
    resetTerminalServerForTests();
    resetMachineTerminalServerPromptForTests();
    commands.terminalServerStatus.mockResolvedValue(null);
    commands.prepareMachineTerminalServerReplacement.mockResolvedValue({
      serverToken: "token-a",
      count: 2,
    });
    commands.restartMachineTerminalServer.mockResolvedValue({ status: "replaced", count: 2 });
    commands.machineTerminalServerUpdateMode.mockResolvedValue({ mode: "replace", sessions: null });
  });

  it("SW-AGR-19: a server too old for the safe replacement is reinstalled after the same confirmation", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({
      mode: "reinstall",
      sessions: null,
    });
    commands.reinstallMachineTerminalServer.mockResolvedValue({
      status: "reinstalled",
      sessions: 2,
    });
    renderShell();
    const outcome = promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 2,
      update: true,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    await expect(outcome).resolves.toEqual({ kind: "updated" });
    expect(commands.reinstallMachineTerminalServer).toHaveBeenCalledWith("box-a", true, 2);
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();
    expect(toasts.success).toHaveBeenCalledWith("2 terminals closed. Build box is ready.");
    expect(screen.queryByTestId("terminal-server-prompt")).toBeNull();
  });

  it("SW-AGR-19 PR-4: after the update, the person reads what closed and what reopened", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({ mode: "reinstall", sessions: 2 });
    const summary = (complete: boolean, message: string) => ({
      reopened: [{ name: "shell", workspace: "SW-1" }],
      notReopened: [],
      recorded: true,
      complete,
      message,
    });
    commands.reinstallMachineTerminalServer
      .mockResolvedValueOnce({
        status: "reinstalled",
        sessions: 2,
        restored: summary(true, "Build box: 1 tab reopened."),
      })
      .mockResolvedValueOnce({
        status: "reinstalled",
        sessions: 2,
        restored: summary(false, "Build box: 1 tab reopened; agents ended (relaunch them): Barb."),
      });
    renderShell();
    for (const [toastKind, message] of [
      ["success", "Build box: 1 tab reopened."],
      ["warning", "Build box: 1 tab reopened; agents ended (relaunch them): Barb."],
    ] as const) {
      const outcome = promptMachineTerminalServerUpdate({
        machineId: "box-a",
        label: "Build box",
        count: 2,
        update: true,
      });
      await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
      await expect(outcome).resolves.toEqual({ kind: "updated" });
      expect(toasts[toastKind]).toHaveBeenCalledWith("2 terminals closed. Build box is ready.", {
        description: message,
      });
    }
  });

  it("SW-AGR-19: a failed reinstall stays honest about whether the old server stopped, and the outcome says why", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({
      mode: "reinstall",
      sessions: null,
    });
    commands.reinstallMachineTerminalServer.mockResolvedValue({
      status: "failed",
      oldStopped: true,
      repair: "the old terminal server stopped, but installing failed: disk full",
    });
    renderShell();
    const outcome = promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 2,
      update: true,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    expect(await screen.findByTestId("terminal-server-prompt")).toHaveTextContent("disk full");
    act(() => closeMachineTerminalServerPrompt());
    await expect(outcome).resolves.toEqual({
      kind: "failed",
      repair: "the old terminal server stopped, but installing failed: disk full",
    });
  });

  it("SW-AGR-19: a refused reinstall says why (e.g. more sessions than confirmed), never 'could not be confirmed'", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({
      mode: "reinstall",
      sessions: null,
    });
    commands.reinstallMachineTerminalServer.mockRejectedValue(
      "3 sessions run there now, not the 2 you confirmed; confirm again",
    );
    renderShell();
    void promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 2,
      update: true,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    const prompt = await screen.findByTestId("terminal-server-prompt");
    await waitFor(() => expect(prompt).toHaveTextContent("not the 2 you confirmed"));
    expect(prompt).not.toHaveTextContent("could not be confirmed");
  });

  it("SW-AGR-19: when the path changed since the confirmation opened, it is described again and nothing runs", async () => {
    commands.machineTerminalServerUpdateMode
      .mockResolvedValueOnce({ mode: "replace", sessions: null })
      .mockResolvedValueOnce({ mode: "reinstall", sessions: null });
    renderShell();
    void promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 2,
      update: true,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    await waitFor(() =>
      expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
        "CodeSense stops it and installs this app's terminal server",
      ),
    );
    expect(commands.reinstallMachineTerminalServer).not.toHaveBeenCalled();
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
  });

  it("SW-AGR-19: another machine's running update is never reported as this one's", async () => {
    let finishA: (v: unknown) => void = () => {};
    commands.restartMachineTerminalServer.mockReturnValue(new Promise((r) => (finishA = r)));
    renderShell();
    const a = promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Box A",
      count: 2,
      update: true,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    await screen.findByRole("button", { name: "Updating…" });
    await expect(
      promptMachineTerminalServerUpdate({
        machineId: "box-b",
        label: "Box B",
        count: 1,
        update: true,
      }),
    ).resolves.toEqual({
      kind: "failed",
      repair: expect.stringContaining("Another terminal server update is running"),
    });
    finishA({ status: "replaced", count: 2 });
    await expect(a).resolves.toEqual({ kind: "updated" });
  });

  it("SW-AGR-19: more sessions than confirmed asks again with the live count, then sends that count", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({ mode: "reinstall", sessions: 2 });
    commands.reinstallMachineTerminalServer
      .mockResolvedValueOnce({ status: "sessionsChanged", sessions: 3 })
      .mockResolvedValueOnce({ status: "reinstalled", sessions: 3 });
    renderShell();
    const outcome = promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: null,
      update: true,
    });
    const prompt = await screen.findByTestId("terminal-server-prompt");
    // The live count from the plan, not the caller's.
    expect(prompt).toHaveTextContent("Restarting closes 2 terminals");
    await userEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    await waitFor(() =>
      expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
        "Restarting closes 3 terminals",
      ),
    );
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "The session count changed: 3 terminals run there now.",
    );
    await userEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    await expect(outcome).resolves.toEqual({ kind: "updated" });
    expect(commands.reinstallMachineTerminalServer.mock.calls).toEqual([
      ["box-a", true, 2],
      ["box-a", true, 3],
    ]);
  });

  it("SW-AGR-19: a team machine's confirmation says teammates' sessions on that login end too", async () => {
    commands.machineTerminalServerUpdateMode.mockResolvedValue({ mode: "reinstall", sessions: 1 });
    renderShell();
    void promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 1,
      update: true,
      shared: true,
    });
    expect(await screen.findByTestId("terminal-server-prompt")).toHaveTextContent(
      "Build box is shared with your team: sessions teammates run on it under the same login end too.",
    );
  });

  it("SW-AGR-19 PR-5: This Mac's update names its sessions and another build's server, then replaces exactly what was confirmed", async () => {
    const expected = { serverId: "srv", epoch: "e1", pid: 4243, sessionIds: ["a", "b"] };
    commands.localTerminalServerUpdate.mockResolvedValue({
      expected,
      terminalServer: {
        appVersion: "0.0.23",
        upToDate: false,
        relation: "otherBuild",
        announcedVersion: "0.1.0",
        devBuild: false,
        serverPath: "/Users/me/codesense/target/debug/codesense-session-server",
      },
    });
    commands.updateLocalTerminalServer
      .mockRejectedValueOnce({
        oldStopped: false,
        changed: true,
        message: "The terminal server or its sessions changed. Review the update again.",
      })
      .mockResolvedValueOnce({ sessions: 2, restored: null });
    renderShell();
    const first = promptMachineTerminalServerUpdate({
      machineId: "local",
      label: "This Mac",
      count: null,
    });
    const prompt = await screen.findByTestId("terminal-server-prompt");
    expect(prompt).toHaveTextContent(
      "The 2 terminals it runs now end — every terminal in this app",
    );
    expect(prompt).toHaveTextContent("the agent you may be talking to");
    expect(prompt).toHaveTextContent("Tabs reopen afterwards as shells");
    expect(prompt).not.toHaveTextContent("including sessions started after");
    expect(prompt).toHaveTextContent(
      "It is another CodeSense build's (/Users/me/codesense/target/debug/codesense-session-server)",
    );
    await userEvent.click(screen.getByRole("button", { name: "Update terminal server" }));
    expect(await screen.findByTestId("terminal-server-prompt")).toHaveTextContent("changed");
    expect(commands.updateLocalTerminalServer).toHaveBeenCalledWith(expected);
    act(() => closeMachineTerminalServerPrompt());
    await expect(first).resolves.toMatchObject({ kind: "failed" });
    const second = promptMachineTerminalServerUpdate({
      machineId: "local",
      label: "This Mac",
      count: null,
    });
    await userEvent.click(await screen.findByRole("button", { name: "Update terminal server" }));
    await expect(second).resolves.toEqual({ kind: "updated" });
    expect(commands.machineTerminalServerUpdateMode).not.toHaveBeenCalled();
  });

  it("SW-AGR-19 PR-5: This Mac's prompt says why it could not be read, and is held while Update all runs", async () => {
    commands.localTerminalServerUpdate.mockRejectedValue(
      "This Mac's terminal server is not connected",
    );
    renderShell();
    await expect(
      promptMachineTerminalServerUpdate({ machineId: "local", label: "This Mac", count: null }),
    ).resolves.toEqual({
      kind: "failed",
      repair: "This Mac's terminal server is not connected",
    });
    setUpdateAllRunning(true);
    await expect(
      promptMachineTerminalServerUpdate({ machineId: "box", label: "box", count: null }),
    ).resolves.toMatchObject({ kind: "failed", repair: expect.stringContaining("Update all") });
    setUpdateAllRunning(false);
  });

  it("SW-AGR-19 PR-5: This Mac already on this app's build says so, opening nothing", async () => {
    commands.localTerminalServerUpdate.mockResolvedValue(null);
    renderShell();
    await expect(
      promptMachineTerminalServerUpdate({ machineId: "local", label: "This Mac", count: null }),
    ).resolves.toEqual({
      kind: "failed",
      repair: "This Mac's terminal server is already this app's build.",
    });
    expect(screen.queryByTestId("terminal-server-prompt")).toBeNull();
  });

  it("SW-AGR-19: closing the confirmation resolves as closed, and nothing runs", async () => {
    renderShell();
    const outcome = promptMachineTerminalServerUpdate({
      machineId: "box-a",
      label: "Build box",
      count: 2,
      update: true,
    });
    await screen.findByTestId("terminal-server-prompt");
    act(() => closeMachineTerminalServerPrompt());
    await expect(outcome).resolves.toEqual({ kind: "closed" });
    // Only the read of how it would update (for the confirmation's words).
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();
    expect(commands.reinstallMachineTerminalServer).not.toHaveBeenCalled();
  });

  function open(machineId = "box-a", label = "Build box", count: number | null = 2) {
    act(() => openMachineTerminalServerPrompt({ machineId, label, count }));
  }

  it("keeps compatible cancellation inert and warns about sessions started after the prompt", async () => {
    renderShell();
    act(() =>
      openMachineTerminalServerPrompt({
        machineId: "box-a",
        label: "Build box",
        count: null,
        compatible: true,
      }),
    );
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "including sessions started after this prompt opened",
    );
    expect(screen.getByTestId("terminal-server-prompt")).not.toHaveTextContent("too old");
    await userEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();
  });

  it("names the live sessions an update of a connected server ends, and that its timers end", () => {
    renderShell();
    act(() =>
      openMachineTerminalServerPrompt({
        machineId: "box-a",
        label: "Build box",
        count: 3,
        compatible: true,
      }),
    );
    const prompt = screen.getByTestId("terminal-server-prompt");
    expect(prompt).toHaveTextContent("3 terminals");
    expect(prompt).toHaveTextContent("timers and scheduled work running on the server end too");
  });

  it("shows a preparation error even when the machine remains connected", async () => {
    commands.prepareMachineTerminalServerReplacement.mockRejectedValueOnce(
      new Error("proof unavailable"),
    );
    machineStore.refreshMachine.mockResolvedValueOnce({ status: { state: "connected" } });
    renderShell();
    act(() =>
      openMachineTerminalServerPrompt({
        machineId: "box-a",
        label: "Build box",
        count: null,
        compatible: true,
      }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Build box remains connected. proof unavailable",
    );
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();
    expect(toasts.success).not.toHaveBeenCalled();
  });

  it("treats a lost execution response as unknown even when a replacement is connected", async () => {
    commands.restartMachineTerminalServer.mockRejectedValueOnce(
      new Error("completion response lost"),
    );
    machineStore.refreshMachine.mockResolvedValueOnce({ status: { state: "connected" } });
    renderShell();
    act(() =>
      openMachineTerminalServerPrompt({
        machineId: "box-a",
        label: "Build box",
        count: null,
        compatible: true,
      }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The restart result could not be confirmed",
    );
    expect(screen.getByRole("alert")).not.toHaveTextContent("was not stopped");
    expect(screen.queryByRole("button", { name: "Restart terminal server" })).toBeNull();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(toasts.success).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole("button", { name: "Open Machines" }));
    expect(navigation.openMachinesPanel).toHaveBeenCalledWith(null);
    expect(commands.restartMachineTerminalServer).toHaveBeenCalledTimes(1);
  });

  it("reports compatible completion without inventing an ended-session count", async () => {
    commands.prepareMachineTerminalServerReplacement.mockResolvedValueOnce({
      serverToken: "token-a",
      count: null,
    });
    commands.restartMachineTerminalServer.mockResolvedValueOnce({
      status: "replaced",
      count: null,
    });
    renderShell();
    act(() =>
      openMachineTerminalServerPrompt({
        machineId: "box-a",
        label: "Build box",
        count: 5,
        compatible: true,
      }),
    );
    await userEvent.dblClick(screen.getByRole("button", { name: "Restart terminal server" }));
    await waitFor(() => expect(toasts.success).toHaveBeenCalledWith("Build box is ready."));
    expect(commands.restartMachineTerminalServer).toHaveBeenCalledTimes(1);
  });

  it("is target-bound, names the exact count, and initially focuses Keep read-only", async () => {
    renderShell();
    open();
    expect(
      screen.getByRole("heading", { name: "Restart the terminal server on Build box?" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "Restarting closes 2 terminals.",
    );
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Keep read-only" })).toHaveFocus(),
    );
  });

  it("does not fabricate an unknown count", () => {
    renderShell();
    open("old-box", "Old box", null);
    expect(screen.getByTestId("terminal-server-prompt")).toHaveTextContent(
      "It is too old to report how many sessions it owns. Restarting may end sessions",
    );
    expect(screen.getByTestId("terminal-server-prompt")).not.toHaveTextContent(/0 terminals/);
  });

  it("cancel and Escape perform zero backend calls", async () => {
    renderShell();
    open();
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByTestId("terminal-server-prompt")).toBeNull());
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();

    open();
    await userEvent.click(screen.getByRole("button", { name: "Keep read-only" }));
    expect(commands.prepareMachineTerminalServerReplacement).not.toHaveBeenCalled();
    expect(commands.restartMachineTerminalServer).not.toHaveBeenCalled();
  });

  it("suppresses duplicate confirmation and passes only machineId plus the reserved token", async () => {
    let finish!: (value: { status: "replaced"; count: number }) => void;
    commands.restartMachineTerminalServer.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderShell();
    open();
    const restart = screen.getByRole("button", { name: "Restart terminal server" });
    await userEvent.dblClick(restart);
    expect(await screen.findByRole("button", { name: "Restarting…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Keep read-only" })).toBeDisabled();
    expect(commands.prepareMachineTerminalServerReplacement).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(commands.restartMachineTerminalServer).toHaveBeenCalledTimes(1));
    expect(commands.restartMachineTerminalServer).toHaveBeenCalledWith("box-a", "token-a");
    act(() => finish({ status: "replaced", count: 2 }));
    await waitFor(() =>
      expect(toasts.success).toHaveBeenCalledWith("2 terminals closed. Build box is ready."),
    );
  });

  it("keeps pre-drain and post-drain failures honest", async () => {
    commands.restartMachineTerminalServer.mockResolvedValueOnce({
      status: "failed",
      oldStopped: false,
      count: 2,
      repair: "authority changed",
    });
    renderShell();
    open();
    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The terminal server was not stopped. Build box remains read-only. authority changed",
    );

    commands.restartMachineTerminalServer.mockResolvedValueOnce({
      status: "failed",
      oldStopped: true,
      count: 2,
      repair: "candidate did not start",
    });
    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    expect(await screen.findByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Restart terminal server" })).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The old terminal server stopped, but Build box isn't ready. candidate did not start",
    );
  });

  it("keeps two Machine targets isolated", async () => {
    renderShell();
    open("box-b", "Box B", 0);
    await userEvent.click(screen.getByRole("button", { name: "Restart terminal server" }));
    await waitFor(() => expect(commands.restartMachineTerminalServer).toHaveBeenCalled());
    expect(commands.prepareMachineTerminalServerReplacement).toHaveBeenCalledWith("box-b");
    expect(commands.restartMachineTerminalServer).toHaveBeenCalledWith("box-b", "token-a");
    expect(toasts.success).toHaveBeenCalledWith("2 terminals closed. Box B is ready.");
  });
});

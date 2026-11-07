// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { incompatibleTerminalServer } from "@/app/mock/handlers/terminal-server";
import { machineHandlers, mockTerminalServer } from "@/app/mock/handlers/machines";
import type { MachineSummary } from "@/tauri/commands";
import {
  decline,
  found,
  resetTerminalServerForTests,
  useTerminalServer,
} from "@/features/shell/lib/terminal-server";
import {
  resetMachineTerminalServerPromptForTests,
  useMachineTerminalServerPrompt,
} from "@/features/shell/lib/machine-terminal-server";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
  ZERO_LATENCY,
} from "../__test-utils__/machines-backend";
import { runMachineAction } from "../lib/machine-actions";
import { getTeamMachines } from "../stores/machines-store";
import { MachinesGroup } from "./machines-group";
import { TerminalServerPrompt } from "@/features/shell/components/terminal-server-prompt";

beforeEach(() => {
  resetMachinesUi();
  resetTerminalServerForTests();
  resetMachineTerminalServerPromptForTests();
});

/** Whether the RT-6b restart prompt is showing (read through its own hook). */
function RestartPromptProbe() {
  return <span data-testid="rt6b-prompt-open">{String(useTerminalServer().open)}</span>;
}

function MachineRestartPromptProbe() {
  const { target } = useMachineTerminalServerPrompt();
  return <span data-testid="machine-restart-target">{target?.machineId ?? "none"}</span>;
}

const rowIds = () =>
  screen
    .getAllByTestId(/^machine-row-(?!status|note)/)
    .map((el) => el.dataset.testid!.replace("machine-row-", ""));

describe("Connections → Machines group", () => {
  it("zero friction: only This Mac · Terminal server running + a quiet Add machine", async () => {
    installMachinesBackend("none");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const group = await screen.findByTestId("connections-machines-group");
    expect(rowIds()).toEqual(["local"]);
    expect(within(group).getByTestId("machine-row-local")).toHaveTextContent(
      /This Mac.*Running · 4 workspaces.*Terminal server/,
    );
    // A healthy local terminal server has nothing to restart.
    expect(within(group).queryByTestId("machine-action-restartServer-local")).toBeNull();
    expect(within(group).getByTestId("connections-add-machine")).toBeInTheDocument();
  });

  it("SW-AGR-19: each connected machine shows its terminal server next to the app", async () => {
    // The real mock list, with one remote server older than the app.
    const real = machineHandlers("demo", ZERO_LATENCY);
    installMachinesBackend("demo", {
      list_machines: async (args) =>
        ((await real.list_machines!(args)) as MachineSummary[]).map((m) =>
          m.machineId === "gpu-box-2"
            ? {
                ...m,
                terminalServer: mockTerminalServer(false, {
                  serverVersion: "0.0.21",
                  upToDate: false,
                  relation: "older",
                }),
              }
            : m,
        ),
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const local = await screen.findByTestId("machine-terminal-server-local");
    expect(local).toHaveTextContent(/^Terminal server 0\.0\.22 ✓ up to date · started /);
    // This Mac also names the executable it runs.
    expect(screen.getByTestId("machine-terminal-server-path-local")).toHaveTextContent(
      "/Applications/CodeSense.app/Contents/MacOS/codesense-session-server",
    );
    expect(screen.getByTestId("machine-terminal-server-gpu-box-2")).toHaveTextContent(
      "Terminal server 0.0.21 · older than app 0.0.22",
    );
    // A machine that is not connected reports no server.
    expect(screen.queryByTestId("machine-terminal-server-devbox-01")).toBeNull();
  });

  it("This Mac offers the RT-6b restart only while another version's server runs", async () => {
    const { calls } = installMachinesBackend("none", {
      terminal_server_status: () => incompatibleTerminalServer(3),
    });
    render(
      <>
        <MachinesGroup teamKey={TEAM_KEY} />
        <RestartPromptProbe />
      </>,
    );
    await screen.findByTestId("machine-row-local");
    act(() => found(incompatibleTerminalServer(3)));
    const restart = screen.getByTestId("machine-action-restartServer-local");
    expect(restart).toHaveTextContent("Restart terminal server (closes 3 terminals)");
    // It opens the existing prompt (which says what the restart closes) — it
    // never restarts from the row.
    act(() => decline());
    expect(screen.getByTestId("rt6b-prompt-open")).toHaveTextContent("false");
    await userEvent.click(restart);
    await waitFor(() => expect(screen.getByTestId("rt6b-prompt-open")).toHaveTextContent("true"));
    expect(calls.some((c) => c.cmd.startsWith("restart_"))).toBe(false);
  });

  it("offers the safe replacement for a supported frozen Machine, the confirmed update otherwise, and keeps the target isolated", async () => {
    const base = {
      kind: "ssh" as const,
      scope: { kind: "personal" as const },
      declaration: null,
      binding: null,
      canPublish: true,
      attention: 0,
      capabilities: { agentCallbacks: true },
    };
    const { calls } = installMachinesBackend("none", {
      list_machines: () => [
        {
          ...base,
          machineId: "box-a",
          label: "Box A",
          status: { state: "terminalServerFrozen", count: 4, replacementSupported: true },
        },
        {
          ...base,
          machineId: "box-b",
          label: "Box B",
          status: { state: "terminalServerFrozen", count: null, replacementSupported: false },
        },
      ],
    });
    render(
      <>
        <MachinesGroup teamKey={TEAM_KEY} />
        <MachineRestartPromptProbe />
      </>,
    );
    const supported = await screen.findByTestId("machine-row-box-a");
    const unsupported = screen.getByTestId("machine-row-box-b");
    expect(within(supported).getByTestId("machine-action-restartServer-box-a")).toBeInTheDocument();
    // SW-AGR-19: no safe replacement is no dead end — the same confirmation,
    // then the reinstall.
    expect(unsupported).toHaveTextContent("update ends its sessions");
    expect(within(unsupported).getByTestId("machine-action-restartServer-box-b")).toHaveTextContent(
      "Update terminal server…",
    );

    await userEvent.click(within(supported).getByTestId("machine-action-restartServer-box-a"));
    expect(screen.getByTestId("machine-restart-target")).toHaveTextContent("box-a");
    expect(calls.some((call) => call.cmd.includes("replacement"))).toBe(false);
  });

  it("SW-AGR-19 PR-4: a machine with an unfinished update's tabs offers to reopen them", async () => {
    const { calls } = installMachinesBackend("demo", {
      machine_pending_tabs: ({ machineId }) => (machineId === "gpu-box-2" ? 3 : null),
      reopen_machine_pending_tabs: () => ({
        reopened: [],
        notReopened: [],
        recorded: true,
        complete: true,
        message: "gpu-box-2: 3 tabs reopened.",
      }),
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const reopen = await screen.findByTestId("machine-action-reopenTabs-gpu-box-2");
    expect(reopen).toHaveTextContent("Reopen 3 tabs from before the update");
    await userEvent.click(reopen);
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "reopen_machine_pending_tabs")).toBe(true),
    );
    await waitFor(() =>
      expect(screen.queryByTestId("machine-action-reopenTabs-gpu-box-2")).toBeNull(),
    );
  });

  it("SW-AGR-19 PR-5/8: Update all lists what ends, then the backend updates each machine as reviewed", async () => {
    const real = machineHandlers("demo", ZERO_LATENCY);
    const { calls } = installMachinesBackend("demo", {
      list_machines: async (args) =>
        ((await real.list_machines!(args)) as MachineSummary[]).map((m) =>
          m.machineId === "gpu-box-2" || m.machineId === "local"
            ? {
                ...m,
                terminalServer: mockTerminalServer(m.machineId === "local", {
                  serverVersion: "0.0.21",
                  upToDate: false,
                  relation: "older",
                }),
              }
            : m,
        ),
      review_terminal_server_update: ({ machineId }) =>
        machineId === "local"
          ? { serverId: "srv-local", epoch: "1", sessions: 1, pid: 1, sessionIds: ["a"] }
          : { serverId: "srv-gpu", epoch: "1", sessions: 2, sessionIds: ["g1", "g2"] },
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("connections-update-all"));
    const dialog = await screen.findByTestId("update-all-dialog");
    expect(dialog).toHaveTextContent("Update 2 terminal servers?");
    await waitFor(() =>
      expect(within(dialog).getByTestId("update-all-machine-gpu-box-2")).toHaveTextContent(
        "2 sessions end",
      ),
    );
    await userEvent.click(within(dialog).getByTestId("update-all-confirm"));
    expect(await within(dialog).findByTestId("update-all-outcome-local")).toHaveTextContent(
      "Updated",
    );
    expect(within(dialog).getByTestId("update-all-outcome-gpu-box-2")).toHaveTextContent("Updated");
    // Closed and opened again after the run: a fresh review, never the
    // finished run.
    await userEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await userEvent.click(await screen.findByTestId("connections-update-all"));
    const again = await screen.findByTestId("update-all-dialog");
    expect(within(again).queryByTestId("update-all-outcome-local")).toBeNull();
    // The backend runs it, bound to what was reviewed (it orders remote
    // machines first, This Mac last).
    const started = calls.find((c) => c.cmd === "start_terminal_servers_update");
    expect(started?.args).toEqual({
      items: [
        {
          machineId: "gpu-box-2",
          expected: { serverId: "srv-gpu", epoch: "1", sessions: 2, sessionIds: ["g1", "g2"] },
        },
        {
          machineId: "local",
          expected: { serverId: "srv-local", epoch: "1", sessions: 1, pid: 1, sessionIds: ["a"] },
        },
      ],
    });
  });

  it("SW-AGR-19 PR-7 (AC8): a stray terminal server on This Mac is listed and stops only after its own confirmation", async () => {
    const stops: unknown[] = [];
    let strays = [
      {
        pid: 51234,
        started: "Thu Sep 24 10:02:11 2026",
        exe: "/Users/me/codesense-wt/build/target/debug/codesense-session-server",
        root: "/private/tmp/replace-home-x7/.codesense",
        kind: "otherBuild",
        sessions: 2,
      },
    ];
    installMachinesBackend("none", {
      list_stray_terminal_servers: async () => strays,
      stop_stray_terminal_server: async ({ target }) => {
        stops.push(target);
        strays = [];
        return { status: "stopped", killed: false };
      },
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-stray-51234");
    expect(row).toHaveTextContent("Another terminal server on This Mac · 2 sessions");
    expect(row).toHaveTextContent("another CodeSense build's terminal server");
    expect(row).toHaveTextContent("started Thu Sep 24 10:02:11 2026");
    expect(row).toHaveTextContent("/private/tmp/replace-home-x7/.codesense");
    expect(within(row).getByTestId("machine-stray-exe-51234")).toHaveTextContent(
      "/Users/me/codesense-wt/build/target/debug/codesense-session-server",
    );
    await userEvent.click(within(row).getByTestId("machine-stray-stop-51234"));
    expect(within(row).getByTestId("machine-stray-stop-no-51234")).toHaveFocus();
    expect(within(row).getByRole("group", { name: "Confirm stop" })).toHaveTextContent(
      "Stop it? Its 2 sessions end, including those of another CodeSense window",
    );
    // Escape cancels, and focus goes back to "Stop…".
    await userEvent.keyboard("{Escape}");
    expect(within(row).queryByRole("group", { name: "Confirm stop" })).toBeNull();
    expect(within(row).getByTestId("machine-stray-stop-51234")).toHaveFocus();
    await userEvent.click(within(row).getByTestId("machine-stray-stop-51234"));
    expect(stops).toEqual([]);
    await userEvent.click(within(row).getByTestId("machine-stray-stop-yes-51234"));
    // The exact process confirmed: pid, start time and root.
    expect(stops).toEqual([
      {
        pid: 51234,
        started: "Thu Sep 24 10:02:11 2026",
        root: "/private/tmp/replace-home-x7/.codesense",
      },
    ]);
    await waitFor(() => expect(screen.queryByTestId("machine-stray-51234")).toBeNull());
  });

  it("SW-AGR-19 PR-7: a stray's unknown sessions are said as such, and a stop that finds it gone says nothing was stopped", async () => {
    installMachinesBackend("none", {
      list_stray_terminal_servers: async () => [
        {
          pid: 7,
          started: "Thu Sep 24 10:02:11 2026",
          exe: "/A/codesense-session-server",
          root: "/tmp/r",
          kind: "ownRoot",
          sessions: null,
        },
      ],
      stop_stray_terminal_server: async () => ({ status: "gone" }),
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-stray-7");
    expect(row).toHaveTextContent("sessions unknown");
    expect(row).toHaveTextContent("a replacement may still be starting");
    await userEvent.click(within(row).getByTestId("machine-stray-stop-7"));
    expect(within(row).getByRole("group", { name: "Confirm stop" })).toHaveTextContent(
      "Any sessions it runs end",
    );
    await userEvent.click(within(row).getByTestId("machine-stray-stop-no-7"));
    expect(within(row).queryByRole("group", { name: "Confirm stop" })).toBeNull();
  });

  it("SW-AGR-19 (AC5): a server too old for the safe replacement is reinstalled after the confirmation — mode, then reinstall, never prepare or set-up", async () => {
    const { calls } = installMachinesBackend("none", {
      list_machines: () => [
        {
          kind: "ssh" as const,
          scope: { kind: "personal" as const },
          declaration: null,
          binding: null,
          canPublish: true,
          attention: 0,
          capabilities: { agentCallbacks: true },
          machineId: "box-b",
          label: "Box B",
          status: { state: "terminalServerFrozen", count: 2, replacementSupported: false },
        },
      ],
      machine_terminal_server_update_mode: () => ({ mode: "reinstall", sessions: 2 }),
      reinstall_machine_terminal_server: () => ({ status: "reinstalled", sessions: 2 }),
    });
    render(
      <>
        <MachinesGroup teamKey={TEAM_KEY} />
        <TerminalServerPrompt />
      </>,
    );
    await userEvent.click(await screen.findByTestId("machine-action-restartServer-box-b"));
    const prompt = await screen.findByTestId("terminal-server-prompt");
    expect(prompt).toHaveTextContent("CodeSense stops it and installs this app's terminal server");
    await userEvent.click(within(prompt).getByRole("button", { name: /terminal server$/ }));
    await waitFor(() =>
      expect(calls.map((c) => c.cmd)).toContain("reinstall_machine_terminal_server"),
    );
    const update = new Set([
      "machine_terminal_server_update_mode",
      "reinstall_machine_terminal_server",
      "prepare_machine_terminal_server_replacement",
      "restart_machine_terminal_server",
    ]);
    const order = calls.map((c) => c.cmd).filter((cmd) => update.has(cmd));
    // Asked once for the confirmation's words, and again when confirmed (the
    // server may have changed meanwhile); then the reinstall.
    expect(order).toEqual([
      "machine_terminal_server_update_mode",
      "machine_terminal_server_update_mode",
      "reinstall_machine_terminal_server",
    ]);
    expect(calls.find((c) => c.cmd === "reinstall_machine_terminal_server")?.args).toEqual({
      machineId: "box-b",
      confirmEndSessions: true,
      expectedSessions: 2,
    });
    expect(useUiStore.getState().machinesPanel).toBeNull();
  });

  it("a connected private machine: name and status first, then its settings; Open shell opens a home shell in the active workspace", async () => {
    const { calls } = installMachinesBackend("none", {
      list_machines: () => [
        {
          machineId: "droplet",
          kind: "ssh",
          label: "Droplet",
          scope: { kind: "personal" },
          declaration: {
            mode: "private-account",
            host: "droplet.example",
            port: 22,
            jumpHost: null,
            defaultBaseDir: null,
            pinnedHostKeyFp: "pin",
            pinnedServerId: "server",
          },
          binding: {
            username: "dev",
            keyPath: null,
            credentialRef: null,
            autoConnect: false,
            hidden: false,
          },
          canPublish: true,
          remoteMcpControl: false,
          status: { state: "connected", workspaces: null },
          attention: 0,
          capabilities: { agentCallbacks: false },
        },
      ],
      terminal_remote_shell_tab: () => ({ status: "refused", code: "unavailable", message: "x" }),
    });
    useViewerStore.setState({
      workspaces: { [TEAM_KEY]: [{ id: "ws-1", kind: "root", name: "repo", path: "/repo" }] },
      activeWorkspace: { [TEAM_KEY]: "ws-1" },
    } as never);
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-row-droplet");

    expect(screen.getByTestId("machine-row-status-droplet").textContent).toBe("●Connected");
    expect(row).not.toHaveTextContent("workspace count unavailable");
    expect(screen.getByTestId("machine-row-note-droplet")).toHaveTextContent(
      "Turn on Allow agents… to let agents on this machine report back",
    );
    const text = row.textContent ?? "";
    expect(text.indexOf("Droplet")).toBeLessThan(text.indexOf("Reconnect in background"));
    expect(text.indexOf("Connected")).toBeLessThan(text.indexOf("Allow agents on this machine"));

    await userEvent.click(within(row).getByTestId("machine-action-openShell-droplet"));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "terminal_remote_shell_tab")?.args).toMatchObject({
        request: {
          operation: "create",
          machineId: "droplet",
          workspaceId: "ws-1",
          teamKey: TEAM_KEY,
          launch: "shell",
        },
      }),
    );
  });

  it("removing a legacy machine stops the old server it left, asking first while terminals run in it", async () => {
    const retire: boolean[] = [];
    const legacyRow = {
      machineId: "old-box",
      kind: "ssh",
      label: "Old box",
      scope: { kind: "personal" },
      declaration: {
        mode: "legacy",
        host: "old.example",
        port: 22,
        jumpHost: null,
        defaultBaseDir: null,
        pinnedHostKeyFp: "pin",
        pinnedServerId: "server",
      },
      binding: null,
      canPublish: true,
      status: { state: "idle" },
      attention: 0,
      capabilities: { agentCallbacks: false },
    };
    let removed = false;
    const { calls } = installMachinesBackend("none", {
      list_machines: () => (removed ? [] : [legacyRow]),
      retire_legacy_terminal_server: ({ stopWithSessions }) => {
        retire.push(Boolean(stopWithSessions));
        return stopWithSessions
          ? { status: "stopped", sessions: 2 }
          : { status: "running", sessions: 2 };
      },
      remove_machine: () => {
        removed = true;
      },
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-row-old-box");
    await userEvent.click(within(row).getByTestId("machine-action-remove-old-box"));
    expect(within(row).getByTestId("machine-confirm-old-box")).toHaveTextContent(
      "stops the old terminal server",
    );
    await userEvent.click(within(row).getByTestId("machine-confirm-yes-old-box"));

    // Terminals still run there: nothing is removed or stopped until confirmed.
    const confirm = await within(row).findByText(
      /2 terminals still run in the old terminal server/,
    );
    expect(confirm).toBeInTheDocument();
    expect(retire).toEqual([false]);
    expect(calls.some((c) => c.cmd === "remove_machine")).toBe(false);

    await userEvent.click(within(row).getByTestId("machine-confirm-yes-old-box"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "remove_machine")).toBe(true));
    expect(retire).toEqual([false, true]);
    const order = calls.map((c) => c.cmd);
    expect(order.lastIndexOf("retire_legacy_terminal_server")).toBeLessThan(
      order.indexOf("remove_machine"),
    );
  });

  it("renders nothing until the registry is available", () => {
    installMachinesBackend("none", {
      list_machines: () => {
        throw new Error("not mocked: list_machines");
      },
    });
    const { container } = render(<MachinesGroup teamKey={TEAM_KEY} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("This Mac is always first; every status state reads as glyph + word", async () => {
    installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await screen.findByTestId("connections-machines-group");
    expect(rowIds()[0]).toBe("local");
    const status = (id: string) => screen.getByTestId(`machine-row-status-${id}`).textContent;
    expect(status("gpu-box-2")).toBe("●Connected · 2 workspaces");
    expect(status("archive-box")).toBe("○Not connected");
    expect(status("edge-01")).toBe("◐Reconnecting · sessions still running on edge-01");
    expect(status("devbox-01")).toMatch(/^▲Offline · since /);
    expect(status("build-farm")).toBe("◌Needs setup");
    expect(status("staging-box")).toBe("🔑Sign-in failed · key rejected");
    expect(status("ci-runner")).toBe("⛔Host key changed · blocked");
    expect(status("scratch-vm")).toBe("⬇Terminal server not installed");
    expect(status("old-vm")).toBe("◆Update needed · server v0.8.2, app needs v1.0");
    expect(screen.getByTestId("machine-row-note-lab-pi")).toHaveTextContent(
      "Agents can't report back here",
    );
  });

  it("publishers see Edit / Unshare / Remove on shared rows", async () => {
    installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-row-gpu-box-2");
    for (const verb of ["edit", "unshare", "remove"]) {
      expect(within(row).getByTestId(`machine-action-${verb}-gpu-box-2`)).toBeInTheDocument();
    }
    expect(within(row).queryByTestId("machine-action-hide-gpu-box-2")).toBeNull();
  });

  it("others see Set up / Hide (local) — no Edit / Unshare / Remove", async () => {
    const { calls } = installMachinesBackend("shared");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-row-build-farm");
    expect(within(row).getByTestId("machine-action-setUp-build-farm")).toBeInTheDocument();
    expect(within(row).queryByTestId("machine-action-edit-build-farm")).toBeNull();
    expect(within(row).queryByTestId("machine-action-unshare-build-farm")).toBeNull();
    expect(within(row).queryByTestId("machine-action-remove-build-farm")).toBeNull();

    await userEvent.click(within(row).getByTestId("machine-action-hide-build-farm"));
    await waitFor(() =>
      expect(screen.getByTestId("machine-row-build-farm")).toHaveTextContent("Hidden"),
    );
    expect(calls.find((c) => c.cmd === "set_machine_hidden")?.args).toEqual({
      machineId: "build-farm",
      hidden: true,
    });
    expect(screen.getByTestId("machine-action-unhide-build-farm")).toBeInTheDocument();
  });

  it("Set up / Review key open the machine reserve panel; Add machine opens the add panel", async () => {
    installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("machine-action-setUp-build-farm"));
    expect(useUiStore.getState().machinesPanel).toEqual({ mode: "setUp", machineId: "build-farm" });
    await userEvent.click(screen.getByTestId("machine-action-reviewKey-ci-runner"));
    expect(useUiStore.getState().machinesPanel).toEqual({
      mode: "reviewKey",
      machineId: "ci-runner",
    });
    await userEvent.click(screen.getByTestId("connections-add-machine"));
    expect(useUiStore.getState().machinesPanel).toEqual({ mode: "add" });
  });

  it("Remove asks first, inline; Cancel (focused) or Escape backs out without removing", async () => {
    const { calls } = installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("machine-action-remove-gpu-box-2"));
    expect(screen.getByTestId("machine-confirm-gpu-box-2")).toHaveTextContent(
      "Remove gpu-box-2 for everyone on",
    );
    expect(screen.getByTestId("machine-confirm-no-gpu-box-2")).toHaveFocus();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByTestId("machine-confirm-gpu-box-2")).toBeNull();
    await userEvent.click(screen.getByTestId("machine-action-remove-gpu-box-2"));
    await userEvent.click(screen.getByTestId("machine-confirm-no-gpu-box-2"));
    expect(screen.queryByTestId("machine-confirm-gpu-box-2")).toBeNull();
    expect(calls.some((c) => c.cmd === "remove_machine")).toBe(false);
    expect(screen.getByTestId("machine-row-gpu-box-2")).toBeInTheDocument();
  });

  it("Unshare asks first, then keeps the machine for you only", async () => {
    installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("machine-action-unshare-gpu-box-2"));
    expect(screen.getByTestId("machine-confirm-gpu-box-2")).toHaveTextContent(
      "Unshare gpu-box-2? Teammates lose it",
    );
    await userEvent.click(screen.getByTestId("machine-confirm-yes-gpu-box-2"));
    await waitFor(() =>
      expect(screen.getByTestId("machine-row-gpu-box-2")).toHaveTextContent("Only me"),
    );
  });

  it("only a publisher is offered Edit address on an unreachable machine", async () => {
    const unreachable = {
      state: "unreachable" as const,
      lastReachedAtMs: 0,
      attempt: 1,
      retryInSecs: 5,
    };
    installMachinesBackend("shared", {
      list_machines: () => [
        {
          machineId: "gpu-box-2",
          kind: "ssh",
          label: "gpu-box-2",
          scope: { kind: "team", orgSlug: TEAM_KEY.split(":")[0], team: TEAM_KEY.split(":")[1] },
          declaration: null,
          binding: null,
          canPublish: false,
          status: unreachable,
          attention: 0,
          capabilities: { agentCallbacks: true },
        },
      ],
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    const row = await screen.findByTestId("machine-row-gpu-box-2");
    expect(within(row).getByTestId("machine-action-retry-gpu-box-2")).toBeInTheDocument();
    expect(within(row).queryByTestId("machine-action-editAddress-gpu-box-2")).toBeNull();
  });

  it("Disconnect drops a connected machine to Not connected", async () => {
    installMachinesBackend("demo");
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await userEvent.click(await screen.findByTestId("machine-action-disconnect-lab-pi"));
    await waitFor(() =>
      expect(screen.getByTestId("machine-row-status-lab-pi")).toHaveTextContent("Not connected"),
    );
  });
});

describe("row verbs vs an in-flight connect", () => {
  it("a slow connect that lands after Disconnect doesn't undo it", async () => {
    let finish: () => void = () => {};
    installMachinesBackend("demo", {
      connect_machine: () =>
        new Promise((resolve) => {
          finish = () =>
            resolve({
              machineId: "lab-pi",
              kind: "ssh",
              label: "lab-pi",
              scope: { kind: "personal" },
              declaration: null,
              binding: null,
              canPublish: true,
              status: { state: "connected", workspaces: 1 },
              attention: 0,
              capabilities: { agentCallbacks: false },
            });
        }),
    });
    render(<MachinesGroup teamKey={TEAM_KEY} />);
    await screen.findByTestId("machine-row-lab-pi");
    void runMachineAction(
      "connect",
      getTeamMachines(TEAM_KEY)!.find((m) => m.machineId === "lab-pi")!,
      TEAM_KEY,
    );
    await userEvent.click(screen.getByTestId("machine-action-disconnect-lab-pi"));
    await waitFor(() =>
      expect(screen.getByTestId("machine-row-status-lab-pi")).toHaveTextContent("Not connected"),
    );
    await act(async () => finish());
    expect(screen.getByTestId("machine-row-status-lab-pi")).toHaveTextContent("Not connected");
  });
});

it("background reconnect is an explicit personal setting; shared profiles start off", async () => {
  const { calls } = installMachinesBackend("demo");
  render(<MachinesGroup teamKey={TEAM_KEY} />);
  const switches = await screen.findAllByRole("checkbox", { name: /Reconnect .* in background/ });
  for (const control of switches) expect(control).not.toBeChecked();
  expect(calls.some((call) => call.cmd === "set_machine_auto_connect")).toBe(false);
  await userEvent.click(switches[0]);
  await waitFor(() =>
    expect(
      calls.some(
        (call) => call.cmd === "set_machine_auto_connect" && call.args.autoConnect === true,
      ),
    ).toBe(true),
  );
  for (const control of switches.slice(1)) expect(control).not.toBeChecked();
});

it("requires personal confirmation before enabling remote control and can revoke directly", async () => {
  let enabled = false;
  const { calls } = installMachinesBackend("none", {
    list_machines: () => [
      {
        machineId: "private-host",
        kind: "ssh",
        label: "Private host",
        scope: { kind: "team", orgSlug: TEAM_KEY.split(":")[0], team: TEAM_KEY.split(":")[1] },
        declaration: { mode: "private-account", host: "host.example", port: 22 },
        binding: {
          username: "owner",
          keyPath: null,
          credentialRef: null,
          autoConnect: false,
          hidden: false,
        },
        canPublish: false,
        status: { state: "idle" },
        attention: 0,
        capabilities: { agentCallbacks: false },
        remoteMcpControl: enabled,
      },
    ],
    set_machine_remote_mcp_control: (args) => {
      enabled = args.enabled === true;
    },
  });
  render(<MachinesGroup teamKey={TEAM_KEY} />);
  const checkbox = await screen.findByRole("checkbox", {
    name: "Allow agents on this machine to use CodeSense",
  });
  expect(checkbox).not.toBeChecked();
  await userEvent.click(checkbox);
  expect(calls.some((c) => c.cmd === "set_machine_remote_mcp_control")).toBe(false);
  await userEvent.click(screen.getByTestId("machine-confirm-no-private-host"));
  expect(checkbox).not.toBeChecked();
  await userEvent.click(checkbox);
  await userEvent.click(screen.getByTestId("machine-confirm-yes-private-host"));
  await waitFor(() => expect(checkbox).toBeChecked());
  expect(
    calls.filter((c) => c.cmd === "set_machine_remote_mcp_control").map((c) => c.args),
  ).toEqual([{ machineId: "private-host", enabled: true }]);
  await userEvent.click(checkbox);
  await waitFor(() => expect(checkbox).not.toBeChecked());
  expect(calls.filter((c) => c.cmd === "set_machine_remote_mcp_control").at(-1)?.args).toEqual({
    machineId: "private-host",
    enabled: false,
  });
  expect(calls.some((c) => c.cmd === "set_machine_auto_connect")).toBe(false);
});

it("a legacy machine says terminal tabs need a private account and offers the switch", async () => {
  installMachinesBackend("none", {
    list_machines: () => [
      {
        machineId: "legacy-host",
        kind: "ssh",
        label: "Legacy host",
        scope: { kind: "personal" },
        declaration: { mode: "legacy", host: "host.example", port: 22 },
        binding: {
          username: "owner",
          keyPath: null,
          credentialRef: null,
          autoConnect: false,
          hidden: false,
        },
        canPublish: true,
        status: { state: "connected", workspaces: 0 },
        attention: 0,
        capabilities: { agentCallbacks: false },
        remoteMcpControl: false,
      },
    ],
  });
  render(<MachinesGroup teamKey={TEAM_KEY} />);
  expect(await screen.findByTestId("machine-legacy-note-legacy-host")).toHaveTextContent(
    "Terminal tabs need a private account.",
  );
  await userEvent.click(screen.getByTestId("machine-action-switchPrivate-legacy-host"));
  expect(useUiStore.getState().machinesPanel).toEqual({
    mode: "setUp",
    machineId: "legacy-host",
    privateAccount: true,
  });
});

it("does not offer remote control for legacy machines", async () => {
  installMachinesBackend("demo");
  render(<MachinesGroup teamKey={TEAM_KEY} />);
  await screen.findByTestId("connections-machines-group");
  expect(
    screen.queryByRole("checkbox", { name: "Allow agents on this machine to use CodeSense" }),
  ).toBeNull();
});

describe("Machines → unreadable machine files", () => {
  it("names a file it can't read, opens it, and removes it", async () => {
    let present = true;
    const { calls } = installMachinesBackend("demo", {
      list_unreadable_machine_files: () =>
        present
          ? [
              {
                machineId: "bad-123456",
                scope: { kind: "team", orgSlug: "acme/widgets", team: "default" },
                path: "/ctx/machines/bad-123456.yaml",
                error: "invalid YAML at line 1",
              },
            ]
          : [],
      remove_unreadable_machine_file: () => {
        present = false;
      },
    });
    const user = userEvent.setup();
    render(<MachinesGroup teamKey={TEAM_KEY} />);

    const row = await screen.findByTestId("machine-unreadable-bad-123456");
    expect(row).toHaveTextContent("Can't read bad-123456.yaml: invalid YAML at line 1");

    await user.click(within(row).getByRole("button", { name: "Open file" }));
    expect(calls.find((c) => c.cmd === "open_unreadable_machine_file")?.args).toMatchObject({
      machineId: "bad-123456",
    });

    await user.click(within(row).getByRole("button", { name: "Remove" }));
    await user.click(within(row).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.queryByTestId("machine-unreadable-bad-123456")).toBeNull());
    expect(calls.some((c) => c.cmd === "remove_unreadable_machine_file")).toBe(true);
  });
});

it("shows the reviewed identity and keeps refusing until the person confirms", async () => {
  const real = machineHandlers("demo", ZERO_LATENCY);
  let accepted = false;
  const identityChange = {
    accepted: {
      host: "old.int",
      port: 22,
      jumpHost: null,
      hostKeyFingerprint: "SHA256:old",
      serverId: "old-server",
    },
    declared: {
      host: "new.int",
      port: 2222,
      jumpHost: "bastion",
      hostKeyFingerprint: "SHA256:new",
      serverId: "new-server",
    },
    revision: "reviewed-identity",
    author: "Teammate",
    date: "2026-10-21T12:00:00Z",
  };
  const { calls } = installMachinesBackend("demo", {
    list_machines: async (args) =>
      ((await real.list_machines!(args)) as MachineSummary[]).map((m) =>
        m.machineId === "gpu-box-2"
          ? {
              ...m,
              declaration: { ...m.declaration!, identityChange: accepted ? null : identityChange },
              status: {
                state: "connectionFailed",
                lastError: "Machine identity changed in team config",
              },
            }
          : m,
      ),
    resolve_machine_identity: ({ accept }) => {
      accepted = accept === true;
    },
  });
  render(<MachinesGroup teamKey={TEAM_KEY} />);
  const row = await screen.findByTestId("machine-row-gpu-box-2");
  expect(row).toHaveTextContent("Machine identity changed in team config");
  expect(row).toHaveTextContent("SHA256:old");
  expect(row).toHaveTextContent("SHA256:new");
  expect(row).toHaveTextContent("old-server");
  expect(row).toHaveTextContent("new-server");
  expect(row).toHaveTextContent("Teammate");
  await userEvent.click(within(row).getByRole("button", { name: "Keep refusing" }));
  await waitFor(() =>
    expect(calls.some((c) => c.cmd === "resolve_machine_identity" && c.args.accept === false)).toBe(
      true,
    ),
  );
  expect(row).toHaveTextContent("SHA256:old");
  await userEvent.click(within(row).getByRole("button", { name: "Confirm" }));
  await waitFor(() =>
    expect(
      calls.some(
        (c) =>
          c.cmd === "resolve_machine_identity" &&
          c.args.accept === true &&
          c.args.revision === "reviewed-identity",
      ),
    ).toBe(true),
  );
  await waitFor(() => expect(within(row).queryByRole("button", { name: "Confirm" })).toBeNull());
});

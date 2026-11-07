// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
} from "../__test-utils__/machines-backend";
import { loadTeamMachines } from "../stores/machines-store";
import { MachineReserve } from "./machine-reserve";

beforeEach(() => {
  resetMachinesUi();
});

describe("Session privacy", () => {
  it("Switch to private account opens a legacy machine already switching, keeping its record", async () => {
    installMachinesBackend("none", {
      list_machines: () => [
        {
          machineId: "legacy-host",
          kind: "ssh",
          label: "Legacy host",
          scope: { kind: "personal" },
          declaration: {
            mode: "legacy",
            host: "host.example",
            port: 22,
            jumpHost: null,
            defaultBaseDir: null,
            pinnedHostKeyFp: "SHA256:legacy",
            pinnedServerId: "server",
          },
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
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: "legacy-host", privateAccount: true }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    await screen.findByTestId("add-machine-panel");
    const privacy = screen.getByLabelText("Session privacy");
    expect(privacy).toHaveValue("private-account");
    expect(privacy).toBeEnabled();
    expect(
      screen.getByText(/keeps this machine's name, host key and settings/),
    ).toBeInTheDocument();
  });
});

describe("Set up — every error is inline with its next step", () => {
  it("re-running setup on a machine whose host key changed stays blocked", async () => {
    installMachinesBackend("demo");
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: "ci-runner" }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    await userEvent.click(await screen.findByTestId("add-machine-test"));
    expect(await screen.findByTestId("add-machine-blocked")).toBeInTheDocument();
  });

  it("a member isn't offered address next steps on a teammate's machine", async () => {
    installMachinesBackend("shared", {
      test_machine_connection: () => ({
        steps: [
          {
            id: "reachable",
            status: "fail",
            detail: "No response",
            nextSteps: ["editAddress", "useJumpHost", "retry"],
          },
        ],
        installPlan: null,
        fileViewOnly: false,
      }),
    });
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: "build-farm" }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    await userEvent.type(await screen.findByLabelText("Your user on it"), "quinn");
    await userEvent.click(screen.getByTestId("add-machine-test"));
    const row = await screen.findByTestId("machine-check-reachable");
    expect(within(row).getByTestId("machine-next-retry")).toBeInTheDocument();
    expect(within(row).queryByTestId("machine-next-editAddress")).toBeNull();
    expect(within(row).queryByTestId("machine-next-useJumpHost")).toBeNull();
  });
});

describe("Set up / Review key", () => {
  it("Set up prefills the shared machine; a member can't re-scope it", async () => {
    installMachinesBackend("shared");
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: "build-farm" }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    expect(await screen.findByText("Set up build-farm")).toBeInTheDocument();
    // A member binds only their own user; the shared address/label are the publishers'.
    expect(screen.getByTestId("machine-locked-address")).toHaveTextContent("build-farm.int");
    expect(screen.getByLabelText("Your user on it")).toHaveValue("");
    expect(screen.queryByLabelText("Label")).toBeNull();
    expect(screen.queryByText("+ Jump host")).toBeNull();
    expect(
      screen.getByText(/Shared with default — you sign in with your own key/),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("machine-share-team")).toBeNull();
  });

  it("a publisher editing a set-up machine sees Edit, not Set up", async () => {
    installMachinesBackend("demo");
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "setUp", machineId: "gpu-box-2" }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    expect(await screen.findByText("Edit gpu-box-2")).toBeInTheDocument();
    expect(screen.getByText(/Change how CodeSense reaches gpu-box-2/)).toBeInTheDocument();
  });

  it("Review key shows both fingerprints and offers no accept", async () => {
    installMachinesBackend("demo");
    await loadTeamMachines(TEAM_KEY);
    render(
      <MachineReserve
        panel={{ mode: "reviewKey", machineId: "ci-runner" }}
        teamKey={TEAM_KEY}
        onClose={vi.fn()}
      />,
    );
    const panel = await screen.findByTestId("machine-review-key-panel");
    expect(within(panel).getByTestId("review-key-pinned").textContent).not.toBe(
      within(panel).getByTestId("review-key-presented").textContent,
    );
    expect(within(panel).queryByRole("button", { name: /trust|accept/i })).toBeNull();
  });
});

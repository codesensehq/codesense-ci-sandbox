// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { useViewerStore } from "@/shared/stores/viewer-store";
import type { MachineSummary } from "@/tauri/commands";

import { workspaceMachineChoices } from "@/features/machines/lib/machine-choices";

import { OpenCliMenuItem } from "./open-cli-menu-item";

let machines: MachineSummary[] | null = [];
vi.mock("@/features/machines/stores/machines-store", () => ({
  useTeamMachines: () => machines,
}));

const TEAM = "org:team";
const STORY_PATH = "/ctx/_bmad-output/epic/SW-1.md";

function machine(
  machineId: string,
  state: string,
  extra: Record<string, unknown> = {},
): MachineSummary {
  return {
    machineId,
    label: machineId.toUpperCase(),
    kind: "ssh",
    status: { state, lastError: "refused", lastReachedAtMs: 0 },
    ...extra,
  } as unknown as MachineSummary;
}

function renderItem() {
  const onOpen = vi.fn();
  const onClose = vi.fn();
  render(
    <div role="menu">
      <OpenCliMenuItem
        teamKey={TEAM}
        storyFilePath={STORY_PATH}
        onOpen={onOpen}
        onClose={onClose}
      />
    </div>,
  );
  return { onOpen, onClose };
}

beforeEach(() => {
  machines = [];
  useViewerStore.setState({ workspaces: {}, softClosedWorkspaces: {} });
});

describe("workspaceMachineChoices", () => {
  it("offers connected and connectable machines; disables unreachable ones with why", () => {
    const rows = workspaceMachineChoices(
      [
        machine("up", "connected"),
        machine("idle", "idle"),
        machine("down", "connectionFailed"),
        machine("gone", "unreachable"),
        machine("legacy", "connected", { declaration: { mode: "legacy" } }),
        machine("hidden", "connected", { binding: { hidden: true } }),
        { ...machine("local", "connected"), kind: "local" } as MachineSummary,
      ],
      0,
    );
    expect(rows.map((r) => r.machineId)).toEqual(["up", "idle", "down", "gone", "legacy"]);
    const by = Object.fromEntries(rows.map((r) => [r.machineId, r]));
    expect(by.up).toMatchObject({ blocked: null, note: null });
    expect(by.idle).toMatchObject({ blocked: null, note: "Connects first" });
    expect(by.down.blocked).toBe("DOWN is unreachable");
    expect(by.gone.blocked).toBe("GONE is unreachable");
    expect(by.legacy.blocked).toMatch(/private account/);
  });
});

describe("OpenCliMenuItem", () => {
  it("an existing story workspace opens directly where it lives: no submenu", () => {
    machines = [machine("up", "connected")];
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: "SW-1", kind: "story", storyId: "SW-1" } as never] },
    });
    const { onOpen, onClose } = renderItem();
    const item = screen.getByRole("menuitem", { name: "Open CLI" });
    expect(screen.queryByTestId("nav-open-cli-local")).toBeNull();
    fireEvent.click(item);
    expect(onClose).toHaveBeenCalled();
    expect(onOpen).toHaveBeenCalledWith(STORY_PATH, undefined);
  });

  it("a minimized story workspace also opens directly", () => {
    machines = [machine("up", "connected")];
    useViewerStore.setState({
      softClosedWorkspaces: { [TEAM]: { "SW-1": { workspace: { id: "SW-1" } } as never } },
    });
    const { onOpen } = renderItem();
    fireEvent.click(screen.getByRole("menuitem", { name: "Open CLI" }));
    expect(onOpen).toHaveBeenCalledWith(STORY_PATH, undefined);
  });

  it("a team without machines opens the plain way", () => {
    const { onOpen } = renderItem();
    fireEvent.click(screen.getByRole("menuitem", { name: "Open CLI" }));
    expect(onOpen).toHaveBeenCalledWith(STORY_PATH, undefined);
  });

  it("no workspace yet: a submenu picks This Mac or a machine; an unreachable one is disabled", async () => {
    machines = [machine("up", "connected"), machine("down", "connectionFailed")];
    const user = userEvent.setup();
    const { onOpen } = renderItem();
    await user.click(screen.getByRole("menuitem", { name: /Open CLI/ }));

    expect(await screen.findByTestId("nav-open-cli-local")).toHaveTextContent("This Mac");
    const down = screen.getByTestId("nav-open-cli-down");
    expect(down).toHaveAttribute("data-disabled");
    expect(down).toHaveTextContent("DOWN is unreachable");

    await user.click(screen.getByTestId("nav-open-cli-up"));
    expect(onOpen).toHaveBeenCalledWith(STORY_PATH, { kind: "machine", machineId: "up" });
  });

  it("This Mac in the submenu creates the workspace locally", async () => {
    machines = [machine("up", "connected")];
    const user = userEvent.setup();
    const { onOpen } = renderItem();
    await user.click(screen.getByRole("menuitem", { name: /Open CLI/ }));
    await user.click(await screen.findByTestId("nav-open-cli-local"));
    expect(onOpen).toHaveBeenCalledWith(STORY_PATH, { kind: "local" });
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { placeStoryWorkspace, storyWorkspacePlace } from "./place-story-workspace";

const TEAM = "org:team";
const PATH = "/ctx/_bmad-output/epic/SW-9.md";

const open = vi.fn(async (..._args: unknown[]) => "/remote/wt");

const machines = vi.hoisted(() => ({
  list: [] as { machineId: string; kind: string; binding?: { hidden?: boolean } }[],
}));
vi.mock("@/features/machines/stores/machines-store", () => ({
  ensureTeamMachines: vi.fn(async () => true),
  getTeamMachines: vi.fn(() => machines.list),
}));

beforeEach(() => {
  open.mockClear();
  machines.list = [{ machineId: "m1", kind: "ssh" }];
  useViewerStore.setState({
    workspaces: {},
    softClosedWorkspaces: {},
    openOrFocusStoryWorkspace: open as never,
  });
});

function withWorkspace(location?: { machineId: string }) {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        {
          id: "SW-9",
          kind: "story",
          storyId: "SW-9",
          ...(location ? { location: { kind: "machine", requestId: "r", ...location } } : {}),
        } as never,
      ],
    },
  });
}

describe("placeStoryWorkspace", () => {
  it("reports where a story's workspace lives", () => {
    expect(storyWorkspacePlace(TEAM, PATH)).toBeNull();
    withWorkspace();
    expect(storyWorkspacePlace(TEAM, PATH)).toBe("local");
    withWorkspace({ machineId: "m1" });
    expect(storyWorkspacePlace(TEAM, PATH)).toBe("m1");
  });

  it("creates a missing workspace on the machine, with a terminal there", async () => {
    await placeStoryWorkspace(TEAM, PATH, "m1");
    expect(open).toHaveBeenCalledWith(TEAM, PATH, undefined, undefined, true, true, {
      kind: "machine",
      machineId: "m1",
    });
  });

  it("creates a workspace only on one of the team's own machines", async () => {
    // A dashboard's machineId is untrusted: an id the team doesn't have, a hidden
    // machine, or This Mac's own row never takes a workspace on a machine.
    machines.list = [
      { machineId: "m1", kind: "ssh" },
      { machineId: "gone", kind: "ssh", binding: { hidden: true } },
      { machineId: "local", kind: "local" },
    ];
    for (const id of ["nope", "gone", "../m1", "m1 "]) {
      await expect(placeStoryWorkspace(TEAM, PATH, id)).rejects.toThrow(
        /^unavailable: .* is not one of this team's machines/,
      );
    }
    expect(open).not.toHaveBeenCalled();
  });

  it("leaves a missing This Mac workspace to the launch, and an existing one where it is", async () => {
    await placeStoryWorkspace(TEAM, PATH, "local");
    withWorkspace({ machineId: "m1" });
    await placeStoryWorkspace(TEAM, PATH, "m1");
    expect(open).not.toHaveBeenCalled();
  });

  it("refuses a workspace that lives somewhere else", async () => {
    withWorkspace();
    await expect(placeStoryWorkspace(TEAM, PATH, "m1")).rejects.toThrow(
      /already lives on This Mac/,
    );
    withWorkspace({ machineId: "m2" });
    await expect(placeStoryWorkspace(TEAM, PATH, "local")).rejects.toThrow(
      /already lives on machine m2/,
    );
    expect(open).not.toHaveBeenCalled();
  });

  it("fails when the machine did not create it", async () => {
    open.mockResolvedValueOnce("");
    await expect(placeStoryWorkspace(TEAM, PATH, "m1")).rejects.toThrow(/was not created/);
  });
});

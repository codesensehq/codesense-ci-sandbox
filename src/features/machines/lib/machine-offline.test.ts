// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";

import {
  frozenTabMachineIn,
  frozenTabMachineName,
  isRemoteMachineId,
  machineInTeams,
  offlineMachineName,
  machineLabelOf,
  registerMachineLookup,
  UNKNOWN_MACHINE_LABEL,
} from "./machine-offline";
import type { MachineSummary } from "@/tauri/commands";

const machine = (machineId: string, label: string, state: string): MachineSummary =>
  ({ machineId, label, status: { state, lastReachedAtMs: 0 } }) as unknown as MachineSummary;

afterEach(() => registerMachineLookup(() => null));

describe("the machine a remote tab is named by", () => {
  it("is a readable fallback, never the raw id, when no list knows the machine", () => {
    expect(machineLabelOf("m-7f3a")).toBe(UNKNOWN_MACHINE_LABEL);
    expect(frozenTabMachineName({ machineId: "m-7f3a", offlineSince: 1 })).toBe(
      UNKNOWN_MACHINE_LABEL,
    );
    expect(frozenTabMachineIn({}, { machineId: "m-7f3a", offlineSince: 1 })).toBe(
      UNKNOWN_MACHINE_LABEL,
    );
  });

  it("is the machine's label once a list knows it, by the same rule for both reads", () => {
    const studio = machine("m-1", "Studio Mac", "connected");
    registerMachineLookup((id) => (id === "m-1" ? studio : null));
    const byTeam = { "acme:core": [studio] };
    // Read live on a connected machine: not frozen.
    expect(frozenTabMachineName({ machineId: "m-1" })).toBeNull();
    expect(frozenTabMachineIn(byTeam, { machineId: "m-1" })).toBeNull();
    // Held frozen while the machine reads connected.
    expect(frozenTabMachineName({ machineId: "m-1", offlineSince: 5 })).toBe("Studio Mac");
    expect(frozenTabMachineIn(byTeam, { machineId: "m-1", offlineSince: 5 })).toBe("Studio Mac");
    expect(machineLabelOf("m-1")).toBe("Studio Mac");
  });

  it("names an offline machine whether or not its tab is held frozen", () => {
    const box = machine("m-2", "box", "unreachable");
    registerMachineLookup(() => box);
    expect(frozenTabMachineName({ machineId: "m-2" })).toBe("box");
    expect(frozenTabMachineIn({ t: [box] }, { machineId: "m-2" })).toBe("box");
  });

  it("reads the machine as the tab's own team lists it, when two teams list one id", () => {
    const up = machine("box", "Studio", "connected");
    const down = machine("box", "Studio", "unreachable");
    const byTeam = { "acme:ops": [up], "team-1": [down] };
    expect(frozenTabMachineIn(byTeam, { machineId: "box" }, "team-1")).toBe("Studio");
    expect(frozenTabMachineIn(byTeam, { machineId: "box" }, "acme:ops")).toBeNull();
    // A team that does not list it falls back to the first team that does.
    expect(frozenTabMachineIn(byTeam, { machineId: "box" }, "acme:none")).toBeNull();
    expect(machineInTeams(byTeam, "box", "team-1")).toBe(down);
    expect(machineInTeams(byTeam, "box")).toBe(up);
    expect(machineInTeams(byTeam, "nope", "team-1")).toBeUndefined();
  });

  it("is null for a machine whose status no list has read, unless its tab is held frozen", () => {
    const unread = { machineId: "m-3", label: "Unread" } as unknown as MachineSummary;
    expect(frozenTabMachineIn({ t: [unread] }, { machineId: "m-3" }, "t")).toBeNull();
    expect(frozenTabMachineIn({ t: [unread] }, { machineId: "m-3", offlineSince: 9 }, "t")).toBe(
      "Unread",
    );
  });

  it("is null for This Mac", () => {
    expect(frozenTabMachineName({ machineId: "local", offlineSince: 1 })).toBeNull();
    expect(frozenTabMachineIn({}, null)).toBeNull();
  });
});

describe("a remote machine id", () => {
  it("is one rule for every read: `local` and a missing id are This Mac", () => {
    expect(isRemoteMachineId("box")).toBe(true);
    for (const id of ["local", "", null, undefined]) expect(isRemoteMachineId(id)).toBe(false);
    // Even a lookup that knows a machine under `local` never makes This Mac remote.
    const local = machine("local", "Local", "unreachable");
    registerMachineLookup(() => local);
    expect(offlineMachineName("local")).toBeNull();
    expect(frozenTabMachineName({ machineId: "local", offlineSince: 1 })).toBeNull();
    expect(frozenTabMachineIn({ t: [local] }, { machineId: "local", offlineSince: 1 }, "t")).toBe(
      null,
    );
    expect(offlineMachineName("box")).toBe("Local");
  });
});

describe("machineInTeams", () => {
  const ops = machine("box", "Ops box", "connected");
  const core = machine("box", "Core box", "unreachable");
  const other = machine("m-9", "Nine", "connected");

  it("prefers the named team's list", () => {
    expect(machineInTeams({ "acme:ops": [ops], "acme:core": [core] }, "box", "acme:core")).toBe(
      core,
    );
  });

  it("falls back to the first team that lists it when the named team does not, or none is named", () => {
    const byTeam = { "acme:ops": [ops, other], "acme:core": [core] };
    expect(machineInTeams(byTeam, "box", "acme:none")).toBe(ops);
    expect(machineInTeams(byTeam, "m-9", "acme:core")).toBe(other);
    expect(machineInTeams(byTeam, "box")).toBe(ops);
    expect(machineInTeams({}, "box", "acme:ops")).toBeUndefined();
  });
});

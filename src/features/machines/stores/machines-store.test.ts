// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MachineSummary } from "@/tauri/commands";
import { machineHandlers } from "@/app/mock/handlers/machines";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
  ZERO_LATENCY,
} from "../__test-utils__/machines-backend";
import { readBuffer, rememberBuffer } from "../lib/remote-buffers";
import {
  __resetMachineAppearanceForTest,
  restoreMachineAppearance,
  setMachineColor,
  setMachineImage,
  useMachineAppearanceStore,
} from "../lib/machine-appearance";
import {
  ENSURE_MACHINE_MAX_WAIT_MS,
  ENSURE_MACHINES_WAIT_MS,
  APPEARANCE_FORGET_AFTER_MS,
  ensureTeamMachines,
  loadTeamMachines,
  noteMachineChanged,
  useMachinesStore,
} from "./machines-store";

const read = { content: "x", size: 1, truncated: false };

beforeEach(() => {
  resetMachinesUi();
});

describe("machines store", () => {
  it("a machine a teammate removed or unshared takes its file contents along", async () => {
    const base = machineHandlers("demo", ZERO_LATENCY);
    let gone = false;
    installMachinesBackend("demo", {
      list_machines: async (args) => {
        const all = (await base.list_machines!(args)) as MachineSummary[];
        return gone ? all.filter((m) => m.machineId !== "gpu-box-2") : all;
      },
    });
    expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
    rememberBuffer("gpu-box-2", "/home/dev/src/codesense/README.md", {
      status: "ready",
      read,
      at: 1,
    });
    rememberBuffer("devbox-01", "/home/dev/src/api/README.md", { status: "ready", read, at: 1 });

    gone = true;
    expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
    expect(readBuffer("gpu-box-2", "/home/dev/src/codesense/README.md")).toBeUndefined();
    expect(readBuffer("devbox-01", "/home/dev/src/api/README.md")).toBeDefined();
  });

  describe("the look a user gave a machine", () => {
    const images = new Map<string, string>();
    let gone = false;
    beforeEach(async () => {
      images.clear();
      gone = false;
      __resetMachineAppearanceForTest({
        get: async (k) => images.get(k),
        put: async (k, v) => void images.set(k, v),
        delete: async (k) => void images.delete(k),
      });
      await restoreMachineAppearance("alice");
      const base = machineHandlers("demo", ZERO_LATENCY);
      installMachinesBackend("demo", {
        list_machines: async (args) => {
          const all = (await base.list_machines!(args)) as MachineSummary[];
          return gone ? all.filter((m) => m.machineId !== "gpu-box-2") : all;
        },
      });
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      setMachineColor("gpu-box-2", "#39c5cf");
      await setMachineImage("gpu-box-2", "data:image/webp;base64,UklGRg==");
      setMachineColor("devbox-01", "#f778ba");
    });
    afterEach(() => {
      vi.useRealTimers();
      __resetMachineAppearanceForTest();
    });
    const prefs = () => useMachineAppearanceStore.getState().prefs;

    it("survives a machine dropping out of one read and coming back", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      gone = true;
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      expect(prefs()["gpu-box-2"]?.color).toBe("#39c5cf");
      vi.setSystemTime(Date.now() + APPEARANCE_FORGET_AFTER_MS + 1);
      gone = false;
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      gone = true;
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      expect(prefs()["gpu-box-2"]?.color).toBe("#39c5cf");
      expect(images.size).toBe(1);
    });

    it("is forgotten once the machine stays missing from reads five minutes apart", async () => {
      vi.useFakeTimers({ toFake: ["Date"] });
      gone = true;
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      vi.setSystemTime(Date.now() + APPEARANCE_FORGET_AFTER_MS - 1000);
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      expect(prefs()["gpu-box-2"]).toBeDefined();
      vi.setSystemTime(Date.now() + 1000);
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      await vi.waitFor(() => expect(images.size).toBe(0));
      expect(prefs()["gpu-box-2"]).toBeUndefined();
      expect(prefs()["devbox-01"]?.color).toBe("#f778ba");
    });

    it("goes at once when a registry change named the machine (an agent's remove_machine)", async () => {
      noteMachineChanged("gpu-box-2");
      gone = true;
      expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
      await vi.waitFor(() => expect(images.size).toBe(0));
      expect(prefs()["gpu-box-2"]).toBeUndefined();
      expect(prefs()["devbox-01"]?.color).toBe("#f778ba");
    });
  });
});

describe("ensureTeamMachines", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  /** A loaded team list, then a re-read held open until `land` is called —
   *  the read that carries a machine saved moments ago. */
  async function slowReadCarryingANewMachine() {
    const base = machineHandlers("demo", ZERO_LATENCY);
    let slow: { resolve: () => void } | null = null;
    let reads = 0;
    installMachinesBackend("demo", {
      list_machines: async (args) => {
        const all = (await base.list_machines!(args)) as MachineSummary[];
        reads += 1;
        if (reads === 1) return all;
        await new Promise<void>((resolve) => (slow = { resolve }));
        return [...all, { ...all[all.length - 1]!, machineId: "just-saved", label: "Just saved" }];
      },
    });
    expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
    vi.useFakeTimers();
    void loadTeamMachines(TEAM_KEY);
    await vi.advanceTimersByTimeAsync(0);
    return { land: () => slow!.resolve() };
  }

  it("waits past the usual bound for a read that brings a machine just saved", async () => {
    const { land } = await slowReadCarryingANewMachine();
    let answered: boolean | null = null;
    void ensureTeamMachines(TEAM_KEY, "just-saved").then((ok) => (answered = ok));

    await vi.advanceTimersByTimeAsync(ENSURE_MACHINES_WAIT_MS + 1000);
    expect(answered).toBeNull();

    land();
    await vi.advanceTimersByTimeAsync(0);
    expect(answered).toBe(true);
    expect(
      useMachinesStore.getState().byTeam[TEAM_KEY]?.some((m) => m.machineId === "just-saved"),
    ).toBe(true);
  });

  it("answers at the usual bound when the machine is already listed", async () => {
    const { land } = await slowReadCarryingANewMachine();
    const listed = useMachinesStore.getState().byTeam[TEAM_KEY]![0]!.machineId;
    let answered: boolean | null = null;
    void ensureTeamMachines(TEAM_KEY, listed).then((ok) => (answered = ok));

    await vi.advanceTimersByTimeAsync(ENSURE_MACHINES_WAIT_MS);
    expect(answered).toBe(true);
    land();
  });

  it("answers at the usual bound when no machine is named", async () => {
    const { land } = await slowReadCarryingANewMachine();
    let answered: boolean | null = null;
    void ensureTeamMachines(TEAM_KEY).then((ok) => (answered = ok));

    await vi.advanceTimersByTimeAsync(ENSURE_MACHINES_WAIT_MS);
    expect(answered).toBe(true);
    land();
  });

  it("gives up on a hung read after the outer cap when the machine never lands", async () => {
    await slowReadCarryingANewMachine();
    let answered: boolean | null = null;
    void ensureTeamMachines(TEAM_KEY, "just-saved").then((ok) => (answered = ok));

    await vi.advanceTimersByTimeAsync(ENSURE_MACHINE_MAX_WAIT_MS - 1);
    expect(answered).toBeNull();
    await vi.advanceTimersByTimeAsync(1);
    expect(answered).toBe(true);
  });

  it("starts a read when the machine is missing and none is in flight", async () => {
    const base = machineHandlers("demo", ZERO_LATENCY);
    let saved = false;
    const { calls } = installMachinesBackend("demo", {
      list_machines: async (args) => {
        const all = (await base.list_machines!(args)) as MachineSummary[];
        return saved
          ? [...all, { ...all[all.length - 1]!, machineId: "just-saved", label: "Just saved" }]
          : all;
      },
    });
    expect(await loadTeamMachines(TEAM_KEY)).toBe(true);
    const before = calls.filter((c) => c.cmd === "list_machines").length;

    saved = true;
    expect(await ensureTeamMachines(TEAM_KEY, "just-saved")).toBe(true);
    expect(calls.filter((c) => c.cmd === "list_machines").length).toBe(before + 1);
    expect(
      useMachinesStore.getState().byTeam[TEAM_KEY]?.some((m) => m.machineId === "just-saved"),
    ).toBe(true);
  });
});

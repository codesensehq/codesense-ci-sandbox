// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

const handlers = new Map<string, () => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (event: string, handler: () => void) => {
    handlers.set(event, handler);
    return Promise.resolve(() => handlers.delete(event));
  },
}));

const avatarPacksList = vi.fn();
const avatarDefaultPackSet = vi.fn(async (_pack: string | null) => {});
vi.mock("@/tauri/commands", () => ({
  avatarPacksListForTeam: (teamKey: string) => avatarPacksList(teamKey),
  avatarDefaultPackSet: (pack: string | null) => avatarDefaultPackSet(pack),
  avatarPackShareWithTeam: vi.fn(async () => {}),
}));

import { useAvatarPacks, useAvatarPacksStore, useTeamAvatarPacks } from "./use-avatar-packs";
import { BUILTIN_AVATAR_PACKS, BUILTIN_FUNNY_PACK, parseFaceRef } from "@/shared/lib/avatar-packs";
import { useOrgStore } from "@/shared/stores/org-store";
import { AVATARS_CHANGED_EVENT } from "@/tauri/events";

const CREW = {
  id: "crew",
  name: "Crew",
  scope: "team" as const,
  faces: [{ id: "grumpy", ref: "crew:grumpy", label: "Grumpy", src: "data:image/png;base64,AAAA" }],
};

describe("useAvatarPacks", () => {
  beforeEach(() => {
    handlers.clear();
    avatarPacksList.mockReset();
    useAvatarPacksStore.setState({ byTeam: {} });
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  });

  it("ships the built-in packs with original faces", () => {
    const funny = BUILTIN_AVATAR_PACKS.find((p) => p.id === BUILTIN_FUNNY_PACK);
    expect(funny?.faces.length).toBeGreaterThanOrEqual(12);
    expect(funny?.faces[0].ref).toMatch(/^builtin-funny:/);
    expect(BUILTIN_AVATAR_PACKS.find((p) => p.id === "builtin-crest")?.faces.length).toBe(36);
    for (const pack of BUILTIN_AVATAR_PACKS)
      for (const face of pack.faces) expect(face.src).toBeTruthy();
  });

  it("lists the team's packs before the built-ins and resolves their refs", async () => {
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: ["team avatars/bad: skipped"] });
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(result.current.packs[0]?.id).toBe("crew"));
    expect(result.current.packs.map((p) => p.id)).toEqual([
      "crew",
      "builtin-chess",
      "builtin-funny",
      "builtin-crest",
    ]);
    expect(result.current.warnings).toEqual(["team avatars/bad: skipped"]);
    expect(result.current.resolveFace("crew:grumpy")).toBe("data:image/png;base64,AAAA");
    expect(result.current.resolveFace("builtin-funny:hardhat-raccoon")).toBeTruthy();
  });

  it("resolves a face whose pack is not on this machine to null, not an error", async () => {
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(avatarPacksList).toHaveBeenCalled());
    expect(result.current.resolveFace("tpb:ricky")).toBeNull();
    expect(result.current.resolveFace("crew:missing")).toBeNull();
    expect(result.current.resolveFace("owl")).toBeNull();
    expect(result.current.resolveFace("#ff0000")).toBeNull();
    expect(result.current.resolveFace(null)).toBeNull();
  });

  it("keeps the built-ins when the read fails", async () => {
    avatarPacksList.mockRejectedValue(new Error("no active team"));
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(result.current.warnings).toEqual(["no active team"]));
    expect(result.current.packs.map((p) => p.id)).toEqual([
      "builtin-chess",
      "builtin-funny",
      "builtin-crest",
    ]);
    expect(result.current.resolveFace("builtin-crest:red-bolt")).toBeTruthy();
  });

  it("re-reads when a face is added and when the team changes", async () => {
    avatarPacksList.mockResolvedValue({ packs: [], warnings: [] });
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(avatarPacksList).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(handlers.has(AVATARS_CHANGED_EVENT)).toBe(true));

    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    act(() => handlers.get(AVATARS_CHANGED_EVENT)?.());
    await waitFor(() => expect(result.current.resolveFace("crew:grumpy")).not.toBeNull());

    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    await waitFor(() => expect(avatarPacksList).toHaveBeenCalledTimes(3));
  });

  it("parses only pack:id refs", () => {
    expect(parseFaceRef(" crew:grumpy ")).toEqual(["crew", "grumpy"]);
    expect(parseFaceRef("Crew:grumpy")).toBeNull();
    expect(parseFaceRef("crew:")).toBeNull();
    expect(parseFaceRef("a:b:c")).toBeNull();
  });
});

describe("useTeamAvatarPacks", () => {
  beforeEach(() => {
    avatarPacksList.mockReset();
    useAvatarPacksStore.setState({ byTeam: {} });
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  });

  it("reads every listed team's packs, and gives each team its own", async () => {
    const OPS = { ...CREW, id: "ops-crew", faces: [{ ...CREW.faces[0], ref: "ops-crew:grumpy" }] };
    avatarPacksList.mockImplementation((teamKey: string) =>
      Promise.resolve({ packs: teamKey === "acme:ops" ? [OPS] : [CREW], warnings: [] }),
    );
    const { result } = renderHook(() => useTeamAvatarPacks(["acme:core", "acme:ops"]));
    await waitFor(() => expect(result.current("acme:ops").packs[0]?.id).toBe("ops-crew"));
    expect(avatarPacksList).toHaveBeenCalledWith("acme:core");
    expect(avatarPacksList).toHaveBeenCalledWith("acme:ops");
    expect(result.current("acme:core").packs[0]?.id).toBe("crew");
    expect(result.current("acme:ops").resolveFace("ops-crew:grumpy")).toBeTruthy();
    expect(result.current("acme:ops").resolveFace("crew:grumpy")).toBeNull();
    // Only the active team's packs can be shared to or given a default.
    expect(result.current("acme:core").canShare).toBe(true);
    expect(result.current("acme:ops").canShare).toBe(false);
  });

  it("keeps a loaded team's faces the same object while another team loads", async () => {
    let resolveOps: (v: unknown) => void = () => {};
    avatarPacksList.mockImplementation((teamKey: string) =>
      teamKey === "acme:ops"
        ? new Promise((resolve) => {
            resolveOps = resolve;
          })
        : Promise.resolve({ packs: [CREW], warnings: [] }),
    );
    const { result, rerender } = renderHook(({ keys }) => useTeamAvatarPacks(keys), {
      initialProps: { keys: ["acme:core"] },
    });
    await waitFor(() => expect(result.current("acme:core").packs[0]?.id).toBe("crew"));
    const coreFaces = result.current("acme:core").faces;
    // The list of teams grows, and the new team's read lands later.
    rerender({ keys: ["acme:core", "acme:ops"] });
    expect(result.current("acme:core").faces).toBe(coreFaces);
    await act(async () => resolveOps({ packs: [], warnings: [] }));
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeDefined());
    expect(result.current("acme:core").faces).toBe(coreFaces);
  });
});

describe("one read per team", () => {
  beforeEach(() => {
    handlers.clear();
    avatarPacksList.mockReset();
    avatarDefaultPackSet.mockClear();
    useAvatarPacksStore.setState({ byTeam: {} });
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  });

  it("asks once for a team both hooks need, and both draw it with the same faces", async () => {
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const { result } = renderHook(() => ({
      active: useAvatarPacks(),
      listed: useTeamAvatarPacks(["acme:core"]),
    }));
    await waitFor(() => expect(result.current.active.packs[0]?.id).toBe("crew"));
    expect(avatarPacksList).toHaveBeenCalledTimes(1);
    expect(result.current.listed("acme:core").faces).toBe(result.current.active.faces);
  });

  it("re-reads a listed team that is not the active one when its faces change", async () => {
    avatarPacksList.mockResolvedValue({ packs: [], warnings: [] });
    const { result } = renderHook(() => useTeamAvatarPacks(["acme:core", "acme:ops"]));
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeDefined());
    await waitFor(() => expect(handlers.has(AVATARS_CHANGED_EVENT)).toBe(true));
    avatarPacksList.mockImplementation((teamKey: string) =>
      Promise.resolve({ packs: teamKey === "acme:ops" ? [CREW] : [], warnings: [] }),
    );
    act(() => handlers.get(AVATARS_CHANGED_EVENT)?.());
    await waitFor(() =>
      expect(result.current("acme:ops").resolveFace("crew:grumpy")).not.toBeNull(),
    );
    expect(result.current("acme:ops").canShare).toBe(false);
  });

  it("re-reads a team whose first read is still in flight when its faces change, keeping the newer", async () => {
    let first: (v: unknown) => void = () => {};
    avatarPacksList.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          first = resolve;
        }),
    );
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(avatarPacksList).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(handlers.has(AVATARS_CHANGED_EVENT)).toBe(true));
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    act(() => handlers.get(AVATARS_CHANGED_EVENT)?.());
    await waitFor(() => expect(result.current.packs[0]?.id).toBe("crew"));
    expect(avatarPacksList).toHaveBeenCalledTimes(2);
    // The older read lands late and is dropped.
    await act(async () => first({ packs: [], warnings: ["old"] }));
    expect(result.current.packs[0]?.id).toBe("crew");
  });

  it("reads a team again on its next use after a failed read", async () => {
    avatarPacksList.mockRejectedValueOnce(new Error("not synced"));
    const first = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(first.result.current.warnings).toEqual(["not synced"]));
    first.unmount();
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const again = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(again.result.current.packs[0]?.id).toBe("crew"));
    expect(avatarPacksList).toHaveBeenCalledTimes(2);
  });

  it("forgets the teams neither listed nor active", async () => {
    avatarPacksList.mockResolvedValue({ packs: [], warnings: [] });
    const { rerender } = renderHook(({ keys }) => useTeamAvatarPacks(keys), {
      initialProps: { keys: ["acme:core", "acme:ops"] },
    });
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeDefined());
    rerender({ keys: ["acme:core"] });
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeUndefined());
    expect(useAvatarPacksStore.getState().byTeam["acme:core"]).toBeDefined();
  });

  it("always gives the active team's packs, listed or not", async () => {
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const { result } = renderHook(() => useTeamAvatarPacks(["acme:ops"]));
    await waitFor(() => expect(result.current("acme:core").packs[0]?.id).toBe("crew"));
    expect(result.current("acme:core").canShare).toBe(true);
    expect(result.current("acme:ops").canShare).toBe(false);
  });

  it("keeps a team another mounted caller still lists when one caller stops listing it", async () => {
    avatarPacksList.mockResolvedValue({ packs: [], warnings: [] });
    renderHook(() => useTeamAvatarPacks(["acme:ops"]));
    const other = renderHook(({ keys }) => useTeamAvatarPacks(keys), {
      initialProps: { keys: ["acme:ops", "acme:dev"] },
    });
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:dev"]).toBeDefined());
    other.rerender({ keys: [] });
    await waitFor(() => expect(useAvatarPacksStore.getState().byTeam["acme:dev"]).toBeUndefined());
    expect(useAvatarPacksStore.getState().byTeam["acme:ops"]).toBeDefined();
    expect(useAvatarPacksStore.getState().byTeam["acme:core"]).toBeDefined();
  });

  it("reads the active team again after a failed read, waiting longer each time", async () => {
    vi.useFakeTimers();
    try {
      avatarPacksList.mockRejectedValue(new Error("not synced"));
      const { result } = renderHook(() => useAvatarPacks());
      await act(async () => {});
      expect(result.current.warnings).toEqual(["not synced"]);
      expect(avatarPacksList).toHaveBeenCalledTimes(1);
      await act(async () => vi.advanceTimersByTime(2_000));
      expect(avatarPacksList).toHaveBeenCalledTimes(2);
      // The second failure waits twice as long.
      await act(async () => vi.advanceTimersByTime(2_000));
      expect(avatarPacksList).toHaveBeenCalledTimes(2);
      avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
      await act(async () => vi.advanceTimersByTime(2_000));
      expect(avatarPacksList).toHaveBeenCalledTimes(3);
      expect(result.current.packs[0]?.id).toBe("crew");
      await act(async () => vi.advanceTimersByTime(60_000));
      expect(avatarPacksList).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("refuses to set a default pack or share once another team is active", async () => {
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const { result } = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(result.current.packs[0]?.id).toBe("crew"));
    const { setDefaultPack, sharePack } = result.current;
    act(() => useOrgStore.setState({ activeTeamName: "ops" }));
    await expect(setDefaultPack("crew")).rejects.toThrow("The active team changed");
    await expect(sharePack("crew")).rejects.toThrow("The active team changed");
    expect(avatarDefaultPackSet).not.toHaveBeenCalled();
  });

  it("sets no default pack and shares nothing with no team, and sets one for the team it was asked in", async () => {
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    const none = renderHook(() => useAvatarPacks());
    expect(none.result.current.canShare).toBe(false);
    await act(() => none.result.current.setDefaultPack("crew"));
    expect(avatarDefaultPackSet).not.toHaveBeenCalled();
    none.unmount();

    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
    avatarPacksList.mockResolvedValue({ packs: [CREW], warnings: [] });
    const core = renderHook(() => useAvatarPacks());
    await waitFor(() => expect(core.result.current.packs[0]?.id).toBe("crew"));
    const setCore = core.result.current.setDefaultPack;
    // The team changes while the call is on its way: the pack is still set for the team asked in.
    let finish: () => void = () => {};
    avatarDefaultPackSet.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const done = setCore("crew");
    act(() => useOrgStore.setState({ activeTeamName: "ops" }));
    await act(async () => {
      finish();
      await done;
    });
    expect(useAvatarPacksStore.getState().byTeam["acme:core"]?.defaultPack).toBe("crew");
    expect(useAvatarPacksStore.getState().byTeam["acme:ops"]?.defaultPack ?? null).toBeNull();
  });
});

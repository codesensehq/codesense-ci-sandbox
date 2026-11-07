// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
const { invoke, reload, flushProfileState } = vi.hoisted(() => ({
  invoke: vi.fn(),
  reload: vi.fn(),
  flushProfileState: vi.fn(),
}));
vi.mock("@/shared/lib/flush-profile-state", () => ({ flushProfileState }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/shared/lib/private-terminal-authority", () => ({
  listenForPrivateAuthorityRetirement: () => Promise.resolve(),
  observePrivateAuthority: vi.fn(),
}));
beforeEach(() => {
  vi.resetModules();
  invoke.mockReset();
  reload.mockReset();
  flushProfileState.mockReset().mockResolvedValue(undefined);
});
const profile = { principal: "42", displayName: "Local 42", machineOwner: null };
const status = { active: profile, profiles: [profile], privateAuthority: null };
it("initializes offline from local metadata without querying provider credentials", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  expect(useLocalProfileStore.getState().status?.active?.principal).toBe("42");
  expect(invoke.mock.calls.map(([command]) => command)).toEqual(["local_profile_status"]);
});
it("keeps ambiguous historical profiles unselected", async () => {
  invoke.mockResolvedValue({ ...status, active: null });
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  expect(useLocalProfileStore.getState().status?.active).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
});
it("awaits native selection and reloads without retargeting the renderer's persistence principal", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  const { rendererPrincipal } = await import("@/shared/lib/local-profile-session");
  await useLocalProfileStore.getState().initialize();
  let resolve!: (value: unknown) => void;
  invoke.mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  vi.stubGlobal("window", { location: { reload } });
  try {
    const selecting = useLocalProfileStore.getState().select("84");
    expect(reload).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(resolve).toBeTypeOf("function"));
    resolve({ ...status, active: { ...profile, principal: "84" } });
    await selecting;
    expect(reload).toHaveBeenCalledOnce();
    expect(await rendererPrincipal()).toBe("42");
  } finally {
    vi.unstubAllGlobals();
  }
});
it("shows local persistence errors without falling back to another namespace", async () => {
  invoke.mockRejectedValue(new Error("profile file unavailable"));
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  expect(useLocalProfileStore.getState().error).toContain("profile file unavailable");
  expect(useLocalProfileStore.getState().status).toBeNull();
});

it("reloads when background metadata chooses a profile after an ambiguous boot", async () => {
  invoke.mockResolvedValueOnce({ ...status, active: null }).mockResolvedValueOnce(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  const { rendererPrincipal } = await import("@/shared/lib/local-profile-session");
  await useLocalProfileStore.getState().initialize();
  vi.stubGlobal("window", { location: { reload } });
  try {
    await useLocalProfileStore.getState().initialize();
    expect(reload).toHaveBeenCalledOnce();
    expect(await rendererPrincipal()).toBeNull();
    expect(useLocalProfileStore.getState().status?.active).toBeNull();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("aborts selection and reload when old-profile state cannot be saved", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  flushProfileState.mockRejectedValue(new Error("notes disk full"));
  vi.stubGlobal("window", { location: { reload } });
  try {
    await useLocalProfileStore.getState().select("84");
    expect(useLocalProfileStore.getState().error).toContain("notes disk full");
    expect(useLocalProfileStore.getState().status?.active?.principal).toBe("42");
    expect(useLocalProfileStore.getState().busy).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    expect(reload).not.toHaveBeenCalled();
  } finally {
    vi.unstubAllGlobals();
  }
});
it("reports unsafe numeric profiles inline before flushing or selecting", async () => {
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().select("9007199254740993");
  expect(useLocalProfileStore.getState().error).toContain("safely");
  expect(flushProfileState).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
});
it("waits for durability before retiring the old profile", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();
  let saved!: () => void;
  flushProfileState.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        saved = resolve;
      }),
  );
  vi.stubGlobal("window", { location: { reload } });
  try {
    const switching = useLocalProfileStore.getState().select("84");
    expect(invoke).not.toHaveBeenCalledWith("select_local_profile", expect.anything());
    saved();
    await switching;
    expect(invoke).toHaveBeenCalledWith("select_local_profile", {
      principal: "84",
      displayName: undefined,
    });
    expect(reload).toHaveBeenCalledOnce();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("keeps a usable chooser when the active historical principal exceeds the numeric adapter", async () => {
  const unsafe = { ...profile, principal: "9007199254740993" };
  invoke.mockResolvedValue({ ...status, active: unsafe, profiles: [unsafe, profile] });
  const { useLocalProfileStore } = await import("./local-profile-store");
  const { rendererPrincipal } = await import("@/shared/lib/local-profile-session");
  await useLocalProfileStore.getState().initialize();
  expect(useLocalProfileStore.getState().status?.profiles).toEqual([unsafe, profile]);
  expect(useLocalProfileStore.getState().status?.active).toBeNull();
  expect(useLocalProfileStore.getState().error).toContain("safely");
  expect(await rendererPrincipal()).toBe(unsafe.principal);
  vi.stubGlobal("window", { location: { reload } });
  try {
    invoke.mockResolvedValue(status);
    await useLocalProfileStore.getState().select("42");
    expect(invoke).toHaveBeenCalledWith("select_local_profile", {
      principal: "42",
      displayName: undefined,
    });
    expect(flushProfileState).not.toHaveBeenCalled();
    expect(reload).toHaveBeenCalledOnce();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("persists rapid avatar choices in dispatch order", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();

  let finishFirst!: (value: unknown) => void;
  let finishSecond!: (value: unknown) => void;
  invoke.mockReset();
  invoke
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishFirst = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishSecond = resolve;
        }),
    );

  const first = useLocalProfileStore.getState().setIdentity({
    avatar: { kind: "pack", ref: "first:face" },
  });
  const second = useLocalProfileStore.getState().setIdentity({
    avatar: { kind: "pack", ref: "second:face" },
  });
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenNthCalledWith(
    1,
    "set_local_identity",
    expect.objectContaining({
      expectedPrincipal: "42",
      avatar: { kind: "pack", ref: "first:face" },
    }),
  );
  finishFirst(profile);
  await first;
  await Promise.resolve();
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(invoke).toHaveBeenNthCalledWith(
    2,
    "set_local_identity",
    expect.objectContaining({
      expectedPrincipal: "42",
      avatar: { kind: "pack", ref: "second:face" },
    }),
  );
  finishSecond(profile);
  await second;
});

it("drains queued identity edits before dispatching a profile switch", async () => {
  invoke.mockResolvedValue(status);
  const { useLocalProfileStore } = await import("./local-profile-store");
  await useLocalProfileStore.getState().initialize();

  const events: string[] = [];
  let releaseFirst!: () => void;
  const firstGate = new Promise<void>((resolve) => {
    releaseFirst = resolve;
  });
  invoke.mockImplementation(async (command, args) => {
    if (command === "set_local_identity") {
      const label = args.avatar?.ref ?? args.displayName;
      events.push(`identity:${label}`);
      if (label === "first:face") await firstGate;
      return profile;
    }
    if (command === "select_local_profile") {
      events.push("select");
      return { ...status, active: { ...profile, principal: "84" } };
    }
    throw new Error(`Unexpected invoke: ${command}`);
  });
  vi.stubGlobal("window", { location: { reload } });
  try {
    const first = useLocalProfileStore.getState().setIdentity({
      avatar: { kind: "pack", ref: "first:face" },
    });
    const latest = useLocalProfileStore.getState().setIdentity({ displayName: "Latest name" });
    const switching = useLocalProfileStore.getState().select("84");
    // The selection continuation is already queued after flushProfileState.
    // Releasing from the next microtask makes the buggy path dispatch select
    // before the second identity write can start.
    queueMicrotask(releaseFirst);
    await Promise.all([first, latest, switching]);
    expect(events).toEqual(["identity:first:face", "identity:Latest name", "select"]);
  } finally {
    releaseFirst();
    vi.unstubAllGlobals();
  }
});

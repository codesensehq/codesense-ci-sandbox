// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { listeners, getUse, unlisten } = vi.hoisted(() => ({
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
  getUse: vi.fn(),
  unlisten: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => {
      listeners.delete(name);
      return unlisten();
    });
  },
}));
vi.mock("@/tauri/commands", () => ({
  getScratchTeamUse: getUse,
  setScratchTeamUse: vi.fn(),
}));

import { useScratchTeamUse, useScratchTeamUseStore } from "./scratch-team-use-store";

beforeEach(() => {
  listeners.clear();
  getUse.mockReset();
  unlisten.mockReset();
  useScratchTeamUseStore.setState({ byTeam: {}, saving: {} });
});

describe("useScratchTeamUse (SLW-PT-20)", () => {
  it("reads off until the backend confirms on, and a failed read stays off", async () => {
    getUse.mockRejectedValue(new Error("boom"));
    const { result } = renderHook(() => useScratchTeamUse("acme", "platform"));
    expect(result.current).toBe(false);
    await waitFor(() =>
      expect(useScratchTeamUseStore.getState().byTeam["acme:platform"]).toBe(false),
    );
    expect(result.current).toBe(false);
  });

  it("re-reads when another view changes this team's switch", async () => {
    getUse.mockResolvedValue(true);
    const { result } = renderHook(() => useScratchTeamUse("acme", "platform"));
    await waitFor(() => expect(result.current).toBe(true));
    getUse.mockResolvedValue(false);
    await waitFor(() => expect(listeners.has("scratch-use-changed")).toBe(true));
    act(() => {
      listeners.get("scratch-use-changed")?.({ payload: { orgSlug: "acme", team: "other" } });
    });
    expect(getUse).toHaveBeenCalledTimes(1);
    act(() => {
      listeners.get("scratch-use-changed")?.({ payload: { orgSlug: "acme", team: "platform" } });
    });
    await waitFor(() => expect(result.current).toBe(false));
  });
});

it("handles an unlisten rejection when the Tauri bridge has torn down", async () => {
  getUse.mockResolvedValue(true);
  unlisten.mockRejectedValue(new Error("invoke unavailable after teardown"));
  const { result, unmount } = renderHook(() => useScratchTeamUse("acme", "platform"));
  await waitFor(() => expect(result.current).toBe(true));
  unmount();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(unlisten).toHaveBeenCalledOnce();
  expect(listeners.size).toBe(0);
});

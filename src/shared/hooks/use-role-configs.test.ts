// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { rolesList } from "@/tauri/commands";
import { useRoleConfigs, useRoleConfigsStore } from "./use-role-configs";

const handlers = vi.hoisted(() => new Map<string, () => void>());
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: () => void) => {
    handlers.set(event, handler);
    return () => {
      handlers.delete(event);
    };
  }),
}));
vi.mock("@/tauri/commands", () => ({ rolesList: vi.fn() }));
afterEach(cleanup);

it("reconciles role settings changed while every consumer was unmounted when the UI reopens", async () => {
  const team = "acme:backend";
  const original = { id: "reviewer", name: "Reviewer", icon: null, defaultHandler: null };
  const updated = {
    ...original,
    name: "Code review",
    icon: "builtin-chess:rook",
    defaultHandler: "acme/backend/reviewer",
  };
  const added = { id: "qa", name: "Quality", icon: null, defaultHandler: null };
  useRoleConfigsStore.setState({ byTeam: {}, errors: {} });
  vi.mocked(rolesList).mockResolvedValue([original]);
  const first = renderHook(() => useRoleConfigs(team));
  await waitFor(() => expect(first.result.current.roles).toEqual([original]));
  await act(async () => first.unmount());
  expect(handlers.size).toBe(0);

  vi.mocked(rolesList).mockResolvedValue([updated, added]);
  handlers.get("roles:changed")?.();
  handlers.get("sync-wave-completed")?.();
  expect(useRoleConfigsStore.getState().byTeam[team]).toEqual([original]);
  const reopened = renderHook(() => useRoleConfigs(team));
  await waitFor(() => expect(reopened.result.current.roles).toEqual([updated, added]));
  expect(rolesList).toHaveBeenCalledTimes(2);
  expect(rolesList).toHaveBeenLastCalledWith(team);
});

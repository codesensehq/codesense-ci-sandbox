// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

const { mockInvoke, mockListen } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockListen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mockListen }));

import {
  LICENSE_STATE_EVENT,
  licenseCheckNow,
  licenseRestartToUpdate,
  licenseState,
  onLicenseState,
} from "./api";

const view = {
  channel: "beta-staging",
  update: null,
};

describe("license api", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockListen.mockReset();
  });

  it("invokes each license command by its snake_case name", async () => {
    mockInvoke.mockResolvedValue(view);
    await licenseState();
    expect(mockInvoke).toHaveBeenLastCalledWith("license_state");
    await licenseCheckNow();
    expect(mockInvoke).toHaveBeenLastCalledWith("license_check_now");
    await licenseRestartToUpdate();
    expect(mockInvoke).toHaveBeenLastCalledWith("license_restart_to_update");
  });

  it("passes the command's view through unchanged", async () => {
    mockInvoke.mockResolvedValue(view);
    await expect(licenseState()).resolves.toEqual(view);
  });

  it("subscribes to the license state event and hands over the payload", async () => {
    const unlisten = vi.fn();
    mockListen.mockResolvedValue(unlisten);
    const handler = vi.fn();
    await expect(onLicenseState(handler)).resolves.toBe(unlisten);
    expect(mockListen).toHaveBeenCalledWith(LICENSE_STATE_EVENT, expect.any(Function));
    const callback = mockListen.mock.calls[0][1] as (event: { payload: unknown }) => void;
    callback({ payload: view });
    expect(handler).toHaveBeenCalledWith(view);
  });
});

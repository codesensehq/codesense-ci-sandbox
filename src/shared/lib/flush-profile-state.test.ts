// SPDX-License-Identifier: MIT
import { expect, it, vi, beforeEach } from "vitest";
const { viewer, org, config, shortcuts } = vi.hoisted(() => ({
  viewer: vi.fn(),
  org: vi.fn(),
  config: vi.fn(),
  shortcuts: vi.fn(),
}));
vi.mock("@/shared/stores/config-store", () => ({ flushConfigForProfileSwitch: config }));
vi.mock("@/shared/stores/viewer-store", () => ({ flushViewerForProfileSwitch: viewer }));
vi.mock("@/shared/stores/org-store", () => ({ flushOrgForProfileSwitch: org }));
vi.mock("@/shared/stores/shortcuts-store", () => ({ flushShortcutsForProfileSwitch: shortcuts }));
import { flushProfileState } from "./flush-profile-state";
beforeEach(() => {
  config.mockReset().mockResolvedValue(undefined);
  shortcuts.mockReset().mockResolvedValue(undefined);
  viewer.mockReset().mockResolvedValue(undefined);
  org.mockReset().mockResolvedValue(undefined);
});
it("serializes viewer and org read/modify/write saves", async () => {
  let saved!: () => void;
  org.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        saved = resolve;
      }),
  );
  const flushing = flushProfileState();
  await vi.waitFor(() => expect(org).toHaveBeenCalledOnce());
  expect(viewer).not.toHaveBeenCalled();
  saved();
  await flushing;
  expect(viewer).toHaveBeenCalledOnce();
});
it("does not proceed past a failed org write", async () => {
  org.mockRejectedValue(new Error("disk full"));
  await expect(flushProfileState()).rejects.toThrow("disk full");
  expect(viewer).not.toHaveBeenCalled();
});

it("waits for config writes before either org or viewer can overwrite the shared file", async () => {
  let finish!: () => void;
  config.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const flushing = flushProfileState();
  await vi.waitFor(() => expect(config).toHaveBeenCalledOnce());
  expect(org).not.toHaveBeenCalled();
  expect(viewer).not.toHaveBeenCalled();
  finish();
  await flushing;
  expect(org).toHaveBeenCalledOnce();
  expect(viewer).toHaveBeenCalledOnce();
});
it("aborts the shared-file flush when a config write failed", async () => {
  config.mockRejectedValue(new Error("config write failed"));
  await expect(flushProfileState()).rejects.toThrow("config write failed");
  expect(org).not.toHaveBeenCalled();
  expect(viewer).not.toHaveBeenCalled();
});

it("drains a pending global shortcut write before a profile can switch", async () => {
  let finish: (() => void) | undefined;
  shortcuts.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  let finished = false;
  const flushing = flushProfileState().then(() => {
    finished = true;
  });
  try {
    await vi.waitFor(() => expect(shortcuts).toHaveBeenCalledOnce());
    expect(finished).toBe(false);
  } finally {
    finish?.();
    await flushing;
  }
});
it("keeps the current principal when a shortcut write could not be made durable", async () => {
  shortcuts.mockRejectedValue(new Error("shortcut write failed"));
  await expect(flushProfileState()).rejects.toThrow("shortcut write failed");
});

// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";

import { liveHub } from "@/features/dashboard/lib/live-hub";
import { recordsPrepareNativeView } from "@/tauri/commands";
import { watchNotesProjection } from "./use-notes-projection";

vi.mock("@/tauri/commands", () => ({
  recordsPrepareNativeView: vi.fn(async (org: string) => `records:private:${org}:notes`),
}));
vi.mock("@/features/dashboard/lib/live-hub", () => ({ liveHub: { watch: vi.fn() } }));
vi.mock("@/shared/stores/viewer-store", () => ({ useViewerStore: { getState: () => ({}) } }));

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
const stops: Array<ReturnType<typeof vi.fn>> = [];

beforeEach(() => {
  vi.clearAllMocks();
  stops.length = 0;
  vi.mocked(liveHub.watch).mockImplementation(() => {
    const stop = vi.fn();
    stops.push(stop);
    return stop;
  });
});

it("watches each org's notes once, and stops when the last watcher leaves", async () => {
  const acme = { githubUserId: 42, orgSlug: "acme/ctx" };
  const first = watchNotesProjection(acme);
  const second = watchNotesProjection(acme);
  await settle();
  expect(recordsPrepareNativeView).toHaveBeenCalledTimes(1);
  expect(liveHub.watch).toHaveBeenCalledWith(
    "records",
    "records:private:acme/ctx:notes",
    expect.any(Function),
    { window: [0, 100_000] },
  );
  first();
  expect(stops[0]).not.toHaveBeenCalled();
  second();
  expect(stops[0]).toHaveBeenCalledTimes(1);

  // Switching org starts the new org's view.
  const stopOther = watchNotesProjection({ githubUserId: 42, orgSlug: "other/ctx" });
  await settle();
  expect(liveHub.watch).toHaveBeenLastCalledWith(
    "records",
    "records:private:other/ctx:notes",
    expect.any(Function),
    { window: [0, 100_000] },
  );
  stopOther();
  expect(stops[1]).toHaveBeenCalledTimes(1);
});

it("never watches notes kept on this Mac with no org, or when signed out", async () => {
  watchNotesProjection({ githubUserId: 42, orgSlug: null })();
  watchNotesProjection({ githubUserId: 0, orgSlug: "acme/ctx" })();
  await settle();
  expect(recordsPrepareNativeView).not.toHaveBeenCalled();
});

it("a watch stopped before its view is ready never starts", async () => {
  watchNotesProjection({ githubUserId: 42, orgSlug: "acme/ctx" })();
  await settle();
  expect(liveHub.watch).not.toHaveBeenCalled();
});

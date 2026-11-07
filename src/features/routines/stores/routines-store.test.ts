// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { RoutineRun, RoutineRunDetail, RoutineRunHistory } from "@/tauri/commands";

const { routineRunDetail, routineRuns } = vi.hoisted(() => ({
  routineRunDetail: vi.fn(),
  routineRuns: vi.fn(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  routineRunDetail,
  routineRuns,
}));

import { resetRoutinesStoreForTests, useRoutinesStore } from "./routines-store";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function detail(runId: string): RoutineRunDetail {
  return { run_id: runId } as unknown as RoutineRunDetail;
}

function runs(prefix: string, n: number): RoutineRun[] {
  return Array.from({ length: n }, (_, i) => ({ run_id: `${prefix}-${i}` }) as RoutineRun);
}

function page(list: RoutineRun[], nextBefore: string | null): RoutineRunHistory {
  return { runs: list, next_before: nextBefore } as RoutineRunHistory;
}

beforeEach(() => {
  resetRoutinesStoreForTests();
  routineRunDetail.mockReset();
  routineRuns.mockReset();
});

afterEach(() => resetRoutinesStoreForTests());

describe("routines store replies that land out of order", () => {
  it("a run read still out when the user closes the run never reopens it", async () => {
    const slow = deferred<RoutineRunDetail>();
    routineRunDetail.mockReturnValueOnce(slow.promise);
    const opening = useRoutinesStore.getState().openRunDetail("run-a");
    useRoutinesStore.getState().closeRunDetail();
    slow.resolve(detail("run-a"));
    await opening;
    expect(useRoutinesStore.getState().openRun).toBeNull();
  });

  it("an older run read never replaces the run opened after it", async () => {
    const older = deferred<RoutineRunDetail>();
    routineRunDetail.mockReturnValueOnce(older.promise);
    routineRunDetail.mockResolvedValueOnce(detail("run-b"));
    const first = useRoutinesStore.getState().openRunDetail("run-a");
    await useRoutinesStore.getState().openRunDetail("run-b");
    older.resolve(detail("run-a"));
    await first;
    expect(useRoutinesStore.getState().openRun?.run_id).toBe("run-b");
  });

  it("selecting another routine drops a run read still out", async () => {
    const slow = deferred<RoutineRunDetail>();
    routineRunDetail.mockReturnValueOnce(slow.promise);
    routineRuns.mockResolvedValue(page([], null));
    const opening = useRoutinesStore.getState().openRunDetail("run-a");
    useRoutinesStore.getState().select("acme/other");
    slow.resolve(detail("run-a"));
    await opening;
    expect(useRoutinesStore.getState().openRun).toBeNull();
  });

  it("an older runs read never overwrites a newer one", async () => {
    const older = deferred<RoutineRunHistory>();
    routineRuns.mockReturnValueOnce(older.promise);
    routineRuns.mockResolvedValueOnce(page(runs("new", 2), null));
    const first = useRoutinesStore.getState().loadRuns("k");
    await useRoutinesStore.getState().loadRuns("k");
    older.resolve(page(runs("old", 1), null));
    await first;
    expect(useRoutinesStore.getState().runs.k.map((r) => r.run_id)).toEqual(["new-0", "new-1"]);
  });

  it("a re-read sent before Show older runs does not fold the older page away", async () => {
    routineRuns.mockResolvedValueOnce(page(runs("a", 50), "cursor-1"));
    await useRoutinesStore.getState().loadRuns("k");

    const reread = deferred<RoutineRunHistory>();
    routineRuns.mockReturnValueOnce(reread.promise);
    const refreshing = useRoutinesStore.getState().loadRuns("k");

    routineRuns.mockResolvedValueOnce(page(runs("b", 50), null));
    await useRoutinesStore.getState().loadMoreRuns("k");
    expect(useRoutinesStore.getState().runs.k).toHaveLength(100);

    // The stale 50-run reply lands; the store re-reads at the size now shown.
    routineRuns.mockResolvedValueOnce(page([...runs("a", 50), ...runs("b", 50)], null));
    reread.resolve(page(runs("a", 50), "cursor-1"));
    await refreshing;
    await vi.waitFor(() => expect(routineRuns).toHaveBeenLastCalledWith("k", 100));
    await vi.waitFor(() => expect(useRoutinesStore.getState().runs.k).toHaveLength(100));
  });
});

it("drops a closed draft seed so a later agent pick cannot reopen it", () => {
  useRoutinesStore
    .getState()
    .startCreateFor("personal/first", { draft: { task: "Tidy note alpha" }, audience: "personal" });
  useRoutinesStore.getState().closeRoutine();
  expect(useRoutinesStore.getState()).toMatchObject({
    routineOwner: null,
    creating: false,
    createSeed: null,
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook, waitFor } from "@/test-utils";

import { useRunOutcomes } from "@/features/viewer/hooks/use-run-outcomes";
import type { RunOutcomeRead } from "@/shared/lib/run-outcome";

const listRunOutcomes = vi.fn<() => Promise<RunOutcomeRead[]>>();
vi.mock("@/tauri/commands", () => ({ listRunOutcomes: () => listRunOutcomes() }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function read(runId: string): RunOutcomeRead {
  return {
    outcome: {
      story_id: "SW-A",
      run_id: runId,
      status: "success",
      ended_at: "2026-08-30T12:00:00Z",
    },
    validated: "completed",
  };
}

describe("useRunOutcomes", () => {
  beforeEach(() => listRunOutcomes.mockReset());

  it("loads only while active and retains the last success on refresh failure", async () => {
    listRunOutcomes.mockResolvedValueOnce([read("run-1")]);
    const { result, rerender } = renderHook(({ active }) => useRunOutcomes(active), {
      initialProps: { active: false },
    });
    expect(listRunOutcomes).not.toHaveBeenCalled();
    rerender({ active: true });
    await waitFor(() => expect(result.current.outcomes?.[0]?.outcome.run_id).toBe("run-1"));

    listRunOutcomes.mockRejectedValueOnce(new Error("offline"));
    await act(() => result.current.reload());
    expect(result.current.outcomes?.[0]?.outcome.run_id).toBe("run-1");
    expect(result.current.error).toBe("offline");
  });

  it("lets only the newest overlapping request commit", async () => {
    const older = deferred<RunOutcomeRead[]>();
    const newer = deferred<RunOutcomeRead[]>();
    listRunOutcomes.mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const { result } = renderHook(() => useRunOutcomes(true));
    await waitFor(() => expect(listRunOutcomes).toHaveBeenCalledTimes(1));
    let refresh!: Promise<void>;
    act(() => {
      refresh = result.current.reload();
    });
    newer.resolve([read("new")]);
    await act(() => refresh);
    older.resolve([read("old")]);
    await act(() => older.promise);
    expect(result.current.outcomes?.[0]?.outcome.run_id).toBe("new");
  });

  it("ignores a response after leaving the Runs axis", async () => {
    const pending = deferred<RunOutcomeRead[]>();
    listRunOutcomes.mockReturnValueOnce(pending.promise);
    const { result, rerender } = renderHook(({ active }) => useRunOutcomes(active), {
      initialProps: { active: true },
    });
    rerender({ active: false });
    pending.resolve([read("late")]);
    await act(() => pending.promise);
    expect(result.current.outcomes).toBeNull();
  });
});

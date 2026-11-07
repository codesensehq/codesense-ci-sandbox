// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";

// --- controllable command mocks --------------------------------------------
const listContentMergeMarks = vi.fn((_org: string, _team: string) => Promise.resolve<string[]>([]));
const setContentMergeMark = vi.fn(
  (_org: string, _team: string, _path: string, _keys: Record<string, string>) =>
    Promise.resolve<void>(undefined),
);
const clearContentMergeMark = vi.fn((_org: string, _team: string, _path: string) =>
  Promise.resolve<void>(undefined),
);
vi.mock("@/tauri/commands", () => ({
  listContentMergeMarks: (o: string, t: string) => listContentMergeMarks(o, t),
  setContentMergeMark: (o: string, t: string, p: string, k: Record<string, string>) =>
    setContentMergeMark(o, t, p, k),
  clearContentMergeMark: (o: string, t: string, p: string) => clearContentMergeMark(o, t, p),
}));

const toastError = vi.fn();
vi.mock("sonner", () => ({
  toast: Object.assign(() => {}, { error: (m: string) => toastError(m) }),
}));

import { useMergeMarks } from "./use-merge-marks";

beforeEach(() => {
  vi.clearAllMocks();
  listContentMergeMarks.mockResolvedValue([]);
  setContentMergeMark.mockResolvedValue(undefined);
  clearContentMergeMark.mockResolvedValue(undefined);
});

describe("useMergeMarks", () => {
  it("isolates marks and stale reads when the active team changes", async () => {
    let resolveTeamA!: (paths: string[]) => void;
    let resolveTeamB!: (paths: string[]) => void;
    listContentMergeMarks.mockImplementation(
      (_org, team) =>
        new Promise<string[]>((resolve) => {
          if (team === "team-a") resolveTeamA = resolve;
          else resolveTeamB = resolve;
        }),
    );
    const { result, rerender } = renderHook(
      ({ team }: { team: string }) => useMergeMarks("/org", team),
      { initialProps: { team: "team-a" } },
    );

    rerender({ team: "team-b" });
    expect(result.current.isMarked("a.json")).toBe(false);
    await act(async () => resolveTeamA(["a.json"]));
    expect(result.current.isMarked("a.json")).toBe(false);
    await act(async () => resolveTeamB(["b.json"]));
    expect(result.current.isMarked("b.json")).toBe(true);
  });

  it("composes rapid optimistic toggles instead of replacing sibling marks", async () => {
    let resolveFirst!: () => void;
    let resolveSecond!: () => void;
    setContentMergeMark
      .mockImplementationOnce(() => new Promise<void>((resolve) => (resolveFirst = resolve)))
      .mockImplementationOnce(() => new Promise<void>((resolve) => (resolveSecond = resolve)));
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());

    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.toggle("a.json");
      second = result.current.toggle("b.json");
    });
    expect(result.current.isMarked("a.json")).toBe(true);
    expect(result.current.isMarked("b.json")).toBe(true);
    await act(async () => Promise.resolve());
    await act(async () => {
      resolveFirst();
      resolveSecond();
      await Promise.all([first, second]);
    });
  });

  it("preserves server siblings when an optimistic toggle precedes the initial read", async () => {
    let resolveLoad!: (paths: string[]) => void;
    listContentMergeMarks.mockImplementation(
      () => new Promise<string[]>((resolve) => (resolveLoad = resolve)),
    );
    const { result } = renderHook(() => useMergeMarks("/org", "team"));

    await act(async () => result.current.toggle("local.json"));
    await act(async () => resolveLoad(["server.json"]));
    expect(result.current.isMarked("local.json")).toBe(true);
    expect(result.current.isMarked("server.json")).toBe(true);
  });

  it("retires the pre-load overlay after reconciliation so a later reload stays authoritative", async () => {
    let resolveInitial!: (paths: string[]) => void;
    listContentMergeMarks
      .mockImplementationOnce(() => new Promise<string[]>((resolve) => (resolveInitial = resolve)))
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([]);
    const { result, rerender } = renderHook(
      ({ team }: { team: string }) => useMergeMarks("/org", team),
      { initialProps: { team: "team-a" } },
    );

    await act(async () => result.current.toggle("local.json"));
    await act(async () => resolveInitial([]));
    expect(result.current.isMarked("local.json")).toBe(true);

    rerender({ team: "team-b" });
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalledTimes(2));
    rerender({ team: "team-a" });
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(result.current.isMarked("local.json")).toBe(false));
  });

  it("retires a settled overlay when its in-flight load is cancelled", async () => {
    listContentMergeMarks
      .mockImplementationOnce(() => new Promise<string[]>(() => {}))
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([]);
    const { result, rerender } = renderHook(
      ({ team }: { team: string }) => useMergeMarks("/org", team),
      { initialProps: { team: "team-a" } },
    );
    await act(async () => result.current.toggle("local.json"));
    rerender({ team: "team-b" });
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalledTimes(2));
    rerender({ team: "team-a" });
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(result.current.isMarked("local.json")).toBe(false));
  });

  it("does not toast when a superseded same-path operation fails", async () => {
    let rejectMark!: (error: Error) => void;
    setContentMergeMark.mockImplementation(
      () => new Promise<void>((_resolve, reject) => (rejectMark = reject)),
    );
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());
    let mark!: Promise<void>;
    let unmark!: Promise<void>;
    act(() => {
      mark = result.current.toggle("config.json");
      unmark = result.current.toggle("config.json");
    });
    await act(async () => Promise.resolve());
    await act(async () => {
      rejectMark(new Error("stale failure"));
      await Promise.all([mark, unmark]);
    });
    expect(toastError).not.toHaveBeenCalled();
    expect(result.current.isMarked("config.json")).toBe(false);
  });

  it("reverts to confirmed state when both rapid same-path writes fail", async () => {
    let rejectMark!: (error: Error) => void;
    setContentMergeMark.mockImplementation(
      () => new Promise<void>((_resolve, reject) => (rejectMark = reject)),
    );
    clearContentMergeMark.mockRejectedValue(new Error("clear failed"));
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());
    let mark!: Promise<void>;
    let unmark!: Promise<void>;
    act(() => {
      mark = result.current.toggle("config.json");
      unmark = result.current.toggle("config.json");
    });
    await act(async () => Promise.resolve());
    await act(async () => {
      rejectMark(new Error("mark failed"));
      await Promise.all([mark, unmark]);
    });
    expect(result.current.isMarked("config.json")).toBe(false);
    expect(toastError).toHaveBeenCalledTimes(1);
    expect(toastError).toHaveBeenCalledWith(expect.stringContaining("Couldn't unmark"));
  });

  it("preserves a sibling optimistic operation when another path fails", async () => {
    let rejectA!: (error: Error) => void;
    let resolveB!: () => void;
    setContentMergeMark.mockImplementation(
      (_org, _team, path) =>
        new Promise<void>((resolve, reject) => {
          if (path === "a.json") rejectA = reject;
          else resolveB = resolve;
        }),
    );
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());
    let toggleA!: Promise<void>;
    let toggleB!: Promise<void>;
    act(() => {
      toggleA = result.current.toggle("a.json");
      toggleB = result.current.toggle("b.json");
    });
    await act(async () => Promise.resolve());
    await act(async () => {
      rejectA(new Error("a failed"));
      await toggleA;
    });
    expect(result.current.isMarked("a.json")).toBe(false);
    expect(result.current.isMarked("b.json")).toBe(true);
    await act(async () => {
      resolveB();
      await toggleB;
    });
    expect(result.current.isMarked("b.json")).toBe(true);
  });

  it("uses the latest optimistic value for rapid toggles of the same path", async () => {
    let resolveMark!: () => void;
    setContentMergeMark.mockImplementation(
      () => new Promise<void>((resolve) => (resolveMark = resolve)),
    );
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());

    let mark!: Promise<void>;
    let unmark!: Promise<void>;
    act(() => {
      mark = result.current.toggle("config.json");
      unmark = result.current.toggle("config.json");
    });
    expect(clearContentMergeMark).not.toHaveBeenCalled();
    await act(async () => Promise.resolve());
    await act(async () => {
      resolveMark();
      await Promise.all([mark, unmark]);
    });
    expect(setContentMergeMark).toHaveBeenCalledTimes(1);
    expect(clearContentMergeMark).toHaveBeenCalledTimes(1);
    expect(result.current.isMarked("config.json")).toBe(false);
  });

  it("loads the team's marked paths once and reflects them via isMarked (AC-3)", async () => {
    listContentMergeMarks.mockResolvedValue([".mcp.json", ".claude/settings.json"]);
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(result.current.isMarked(".mcp.json")).toBe(true));
    expect(result.current.isMarked(".claude/settings.json")).toBe(true);
    expect(result.current.isMarked("other.json")).toBe(false);
    expect(listContentMergeMarks).toHaveBeenCalledTimes(1);
    expect(listContentMergeMarks).toHaveBeenCalledWith("/org", "team");
  });

  it("marks an unmarked path via setContentMergeMark with empty array keys (AC-1)", async () => {
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());
    await act(async () => {
      await result.current.toggle(".claude/settings.json");
    });
    // AC-4: the exact content-relative logical path is passed through verbatim.
    expect(setContentMergeMark).toHaveBeenCalledWith("/org", "team", ".claude/settings.json", {});
    expect(clearContentMergeMark).not.toHaveBeenCalled();
    expect(result.current.isMarked(".claude/settings.json")).toBe(true);
  });

  it("unmarks an already-marked path via clearContentMergeMark (AC-1)", async () => {
    listContentMergeMarks.mockResolvedValue([".mcp.json"]);
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(result.current.isMarked(".mcp.json")).toBe(true));
    await act(async () => {
      await result.current.toggle(".mcp.json");
    });
    expect(clearContentMergeMark).toHaveBeenCalledWith("/org", "team", ".mcp.json");
    expect(setContentMergeMark).not.toHaveBeenCalled();
    expect(result.current.isMarked(".mcp.json")).toBe(false);
  });

  it("reverts the optimistic flip and toasts when the seam fails (AC-5)", async () => {
    setContentMergeMark.mockRejectedValue(new Error("disk full"));
    const { result } = renderHook(() => useMergeMarks("/org", "team"));
    await waitFor(() => expect(listContentMergeMarks).toHaveBeenCalled());
    await act(async () => {
      await result.current.toggle("config.json");
    });
    expect(result.current.isMarked("config.json")).toBe(false); // reverted
    expect(toastError).toHaveBeenCalledWith(expect.stringContaining("Couldn't mark merge file"));
  });

  it("no-ops (no read, no write) when there is no active team", async () => {
    const { result } = renderHook(() => useMergeMarks("", ""));
    await act(async () => {
      await result.current.toggle(".mcp.json");
    });
    expect(listContentMergeMarks).not.toHaveBeenCalled();
    expect(setContentMergeMark).not.toHaveBeenCalled();
    expect(result.current.isMarked(".mcp.json")).toBe(false);
  });
});

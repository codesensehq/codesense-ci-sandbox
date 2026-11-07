// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useEpicRepoOverlay } from "./use-epic-repo-overlay";

// SW-EW-18: the overlay open-state now lives in the singleton viewer store, so
// reset it between tests — otherwise an `open` flag set by one `it` leaks into
// the next (they reuse the same workspace id) and tests pass only by order.
beforeEach(() => {
  useViewerStore.setState({ epicReposOpen: {} });
});

const epicWs = (id: string, epicDirPath = "epic-x") => ({
  id,
  epicDirPath,
  epicDocPath: `/ctx/epics/${epicDirPath}/epic.md`,
  sourceRepoSlug: undefined,
});

describe("useEpicRepoOverlay (SW-EW-15)", () => {
  it("auto-shows when an active epic workspace has ZERO repos", async () => {
    const read = vi.fn(async () => [] as string[]);
    const { result } = renderHook(() =>
      useEpicRepoOverlay(epicWs("integration:/a"), ["integration:/a"], read),
    );
    // NON-BLOCKING: opens only after the async read resolves — this is the exact
    // path the original effect-cancellation bug broke.
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));
    expect(read).toHaveBeenCalledWith("epic-x");
  });

  it("does NOT auto-show when the epic already has repos", async () => {
    const read = vi.fn(async () => ["acme/web"]);
    const { result } = renderHook(() =>
      useEpicRepoOverlay(epicWs("integration:/a"), ["integration:/a"], read),
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.isOpen("integration:/a")).toBe(false);
  });

  it("does not evaluate an epic row without a doc path (read can't resolve)", async () => {
    const read = vi.fn(async () => [] as string[]);
    const noDocPath = {
      id: "integration:/a",
      epicDirPath: "epic-x",
      epicDocPath: undefined,
      sourceRepoSlug: undefined,
    };
    const { result } = renderHook(() => useEpicRepoOverlay(noDocPath, ["integration:/a"], read));
    await act(async () => {
      await Promise.resolve();
    });
    expect(read).not.toHaveBeenCalled();
    expect(result.current.isOpen("integration:/a")).toBe(false);
  });

  it("retries the auto-show read after a transient rejection (not permanently suppressed)", async () => {
    let fail = true;
    const read = vi.fn(async () => {
      if (fail) throw new Error("transient");
      return [];
    });
    const { result, rerender } = renderHook(
      ({ ws }) => useEpicRepoOverlay(ws, ["integration:/a"], read),
      { initialProps: { ws: epicWs("integration:/a") } },
    );
    // First read rejects → overlay stays closed, NOT marked evaluated. Flush the
    // catch/finally microtasks so the in-flight guard clears before the retry.
    await waitFor(() => expect(read).toHaveBeenCalledTimes(1));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(result.current.isOpen("integration:/a")).toBe(false);
    // The read can now succeed; a later render re-attempts and the nudge fires.
    fail = false;
    rerender({ ws: { ...epicWs("integration:/a"), id: "integration:/a" } });
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));
  });

  it("ignores non-epic (slugged / story) active workspaces", async () => {
    const read = vi.fn(async () => [] as string[]);
    const slugged = {
      id: "integration:/a:acme/web",
      epicDirPath: "epic-x",
      sourceRepoSlug: "acme/web",
    };
    const { result } = renderHook(() =>
      useEpicRepoOverlay(slugged, ["integration:/a:acme/web"], read),
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(read).not.toHaveBeenCalled();
    expect(result.current.isOpen("integration:/a:acme/web")).toBe(false);
  });

  it("auto-shows at most once — a manual close is not re-opened while the ws stays open", async () => {
    const read = vi.fn(async () => [] as string[]);
    const { result, rerender } = renderHook(
      ({ ws }) => useEpicRepoOverlay(ws, ["integration:/a"], read),
      { initialProps: { ws: epicWs("integration:/a") } },
    );
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));
    act(() => result.current.close("integration:/a"));
    expect(result.current.isOpen("integration:/a")).toBe(false);
    // A re-render (same ws still present) must not re-auto-show.
    rerender({ ws: epicWs("integration:/a") });
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.isOpen("integration:/a")).toBe(false);
    expect(read).toHaveBeenCalledTimes(1);
  });

  it("re-evaluates after the workspace is closed and reopened (pruned from present ids)", async () => {
    const read = vi.fn(async () => [] as string[]);
    const { result, rerender } = renderHook(({ ws, ids }) => useEpicRepoOverlay(ws, ids, read), {
      initialProps: { ws: epicWs("integration:/a"), ids: ["integration:/a"] as string[] },
    });
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));
    act(() => result.current.close("integration:/a"));

    // Workspace closes → disappears from present ids → evaluation forgotten.
    rerender({ ws: null as never, ids: [] });
    // Reopen the same epic (same deterministic id) → auto-shows again.
    rerender({ ws: epicWs("integration:/a"), ids: ["integration:/a"] });
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("clears a stale overlay when a reopened epic is no longer empty", async () => {
    // Open empty → auto-show. Close the workspace. Reopen with repos now present
    // → re-evaluation is authoritative and must NOT show a leftover overlay.
    let repos: string[] = [];
    const read = vi.fn(async () => repos);
    const { result, rerender } = renderHook(({ ws, ids }) => useEpicRepoOverlay(ws, ids, read), {
      initialProps: { ws: epicWs("integration:/a"), ids: ["integration:/a"] as string[] },
    });
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(true));

    // Workspace closes (forget evaluation), repos get configured, then reopen.
    rerender({ ws: null as never, ids: [] });
    repos = ["acme/web"];
    rerender({ ws: epicWs("integration:/a"), ids: ["integration:/a"] });
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    // The 2nd read's authoritative clear lands async — wait for it.
    await waitFor(() => expect(result.current.isOpen("integration:/a")).toBe(false));
  });

  it("toggle opens then closes the overlay", () => {
    const read = vi.fn(async () => ["acme/web"]); // non-empty → no auto-show
    const { result } = renderHook(() =>
      useEpicRepoOverlay(epicWs("integration:/a"), ["integration:/a"], read),
    );
    act(() => result.current.toggle("integration:/a"));
    expect(result.current.isOpen("integration:/a")).toBe(true);
    act(() => result.current.toggle("integration:/a"));
    expect(result.current.isOpen("integration:/a")).toBe(false);
  });

  it("a rejected read leaves the overlay closed (no throw)", async () => {
    const read = vi.fn(async () => {
      throw new Error("read failed");
    });
    const { result } = renderHook(() =>
      useEpicRepoOverlay(epicWs("integration:/a"), ["integration:/a"], read),
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.isOpen("integration:/a")).toBe(false);
  });
});

it("keeps open and evaluation state separate for identical workspace ids in different teams", async () => {
  const read = vi.fn(async () => [] as string[]);
  const { result, rerender } = renderHook(
    ({ team }) => useEpicRepoOverlay(epicWs("same"), ["same"], read, team),
    { initialProps: { team: "acme:alpha" } },
  );
  await waitFor(() => expect(result.current.isOpen("same")).toBe(true));
  act(() => result.current.close("same"));
  rerender({ team: "acme:beta" });
  await waitFor(() => expect(result.current.isOpen("same")).toBe(true));
  expect(read).toHaveBeenCalledTimes(2);
  expect(useViewerStore.getState().epicReposOpen["acme:alpha|same"]).toBe(false);
  expect(useViewerStore.getState().epicReposOpen["acme:beta|same"]).toBe(true);
  rerender({ team: "acme:alpha" });
  expect(result.current.isOpen("same")).toBe(false);
  expect(read).toHaveBeenCalledTimes(2);
});

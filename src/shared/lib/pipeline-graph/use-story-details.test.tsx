// SPDX-License-Identifier: MIT
/**
 * R-14 Task 5 / AC #36: useStoryDetails hook behaviour.
 *
 * - First call to the hook with a path fetches via Tauri.
 * - Second call with the same path returns from the module-level cache
 *   (no second fetch).
 * - When `lastChangedContextFile[repoPath]` matches the path, cache
 *   invalidates and a fresh fetch occurs.
 * - LRU eviction at the configured ceiling.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import {
  useStoryDetails,
  _resetStoryDetailsCacheForTests,
  _seedStoryDetailsCacheForTests,
  _storyDetailsCacheSizeForTests,
} from "./use-story-details";

vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return {
    ...actual,
    getStoryDetails: vi.fn(),
  };
});

import { getStoryDetails, type StoryDetails } from "@/tauri/commands";

const getStoryDetailsMock = vi.mocked(getStoryDetails);

// SW-EW-32: the hook now takes a summary-headings list (part of the cache key).
const H = ["Purpose", "Story"];

describe("useStoryDetails", () => {
  beforeEach(() => {
    _resetStoryDetailsCacheForTests();
    getStoryDetailsMock.mockReset();
    // Reset relevant file-refresh-store state.
    seedFileRefresh();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("fetches on first call and returns data", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({
      title: "T",
      storyText: "S",
    });
    const { result } = renderHook(() => useStoryDetails("/a.md", H));
    await waitFor(() => {
      expect(result.current.data).toEqual({ title: "T", storyText: "S" });
    });
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);
    expect(getStoryDetailsMock).toHaveBeenCalledWith("/a.md", H);
  });

  it("strips CodeSense markers and HTML comments from the card summary", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({
      title: "T",
      storyText: "x <!-- cs:evidence-section -->",
    });
    const { result } = renderHook(() => useStoryDetails("/marker.md", H));
    await waitFor(() => expect(result.current.data?.storyText).toBe("x"));
  });

  it("a summary that is only a marker reads as no summary", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({
      title: "T",
      storyText: "<!-- cs:evidence-section -->\n",
    });
    const { result } = renderHook(() => useStoryDetails("/only-marker.md", H));
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(result.current.data?.storyText).toBeNull();
  });

  it("returns cached value on subsequent mounts for the same path", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({
      title: "T1",
      storyText: "S1",
    });
    const first = renderHook(() => useStoryDetails("/a.md", H));
    await waitFor(() => expect(first.result.current.data).toBeDefined());
    first.unmount();

    const second = renderHook(() => useStoryDetails("/a.md", H));
    // Cache hit — populates synchronously on mount via initial useState.
    expect(second.result.current.data).toEqual({ title: "T1", storyText: "S1" });
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);
  });

  it.each(["/stories/a.md", "/stories"])(
    "invalidates and refetches when a change at %s contains the story",
    async (changed) => {
      getStoryDetailsMock
        .mockResolvedValueOnce({ title: "T1", storyText: "S1" })
        .mockResolvedValueOnce({ title: "T2", storyText: "S2" });
      const { result, rerender } = renderHook(() => useStoryDetails("/stories/a.md", H));
      await waitFor(() => expect(result.current.data).toEqual({ title: "T1", storyText: "S1" }));
      expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);

      // Simulate watcher firing for /a.md in some repo.
      useFileRefreshStore.setState({ lastChangedContextFile: { repoA: new Set([changed]) } });
      rerender();

      await waitFor(() => expect(result.current.data).toEqual({ title: "T2", storyText: "S2" }));
      expect(getStoryDetailsMock).toHaveBeenCalledTimes(2);
    },
  );

  // ---- SW-EW-32: headings are part of the cache key ------------------
  it("re-fetches for the same path when the headings change (distinct cache keys)", async () => {
    getStoryDetailsMock
      .mockResolvedValueOnce({ title: "T", storyText: "from-purpose" })
      .mockResolvedValueOnce({ title: "T", storyText: "from-overview" });

    const { result, rerender } = renderHook(
      ({ h }: { h: string[] }) => useStoryDetails("/a.md", h),
      { initialProps: { h: ["Purpose"] } },
    );
    await waitFor(() =>
      expect(result.current.data).toEqual({ title: "T", storyText: "from-purpose" }),
    );
    expect(getStoryDetailsMock).toHaveBeenLastCalledWith("/a.md", ["Purpose"]);

    // Same path, different headings → cache miss → fresh fetch.
    rerender({ h: ["Overview"] });
    await waitFor(() =>
      expect(result.current.data).toEqual({ title: "T", storyText: "from-overview" }),
    );
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(2);
    expect(getStoryDetailsMock).toHaveBeenLastCalledWith("/a.md", ["Overview"]);
  });

  it("hits the cache for the same (path, headings) — no second fetch", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({ title: "T", storyText: "S" });
    const first = renderHook(() => useStoryDetails("/a.md", ["Purpose", "Story"]));
    await waitFor(() => expect(first.result.current.data).toBeDefined());
    first.unmount();

    const second = renderHook(() => useStoryDetails("/a.md", ["Purpose", "Story"]));
    expect(second.result.current.data).toEqual({ title: "T", storyText: "S" });
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);
  });

  it("returns undefined for undefined filePath", () => {
    const { result } = renderHook(() => useStoryDetails(undefined, H));
    expect(result.current.data).toBeUndefined();
    expect(result.current.loading).toBe(false);
    expect(getStoryDetailsMock).not.toHaveBeenCalled();
  });

  it("does not refetch when lastChangedContextFile changes for a different path", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({ title: "T", storyText: "S" });
    const { rerender } = renderHook(() => useStoryDetails("/a.md", H));
    await waitFor(() => expect(getStoryDetailsMock).toHaveBeenCalledTimes(1));

    // Watcher fired for a different file.
    useFileRefreshStore.setState({ lastChangedContextFile: { repoA: new Set(["/other.md"]) } });
    rerender();
    // Give any spurious effect a tick.
    await new Promise((r) => setTimeout(r, 5));
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);
  });

  // ---- NB-5 coalesce trade-off (post-round-2) ------------------------
  // The hook deliberately does NOT invalidate on `lastChangedContextFile[repo]
  // === null`. Doing so would cascade across every hook in every repo on any
  // bulk change anywhere. The accepted cost: a coalesced multi-file event
  // leaves the affected preview briefly stale until the next single-file
  // edit refreshes the watcher's per-path knowledge.
  it("does NOT refetch when lastChangedContextFile[repo] is null (NB-5 coalesce trade-off)", async () => {
    getStoryDetailsMock.mockResolvedValueOnce({ title: "T1", storyText: "S1" });
    const { rerender } = renderHook(() => useStoryDetails("/a.md", H));
    await waitFor(() => expect(getStoryDetailsMock).toHaveBeenCalledTimes(1));

    // Simulate NB-5 coalesce for an unrelated repo — must NOT cascade.
    useFileRefreshStore.setState({ lastChangedContextFile: { repoA: null } });
    rerender();
    await new Promise((r) => setTimeout(r, 5));
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(1);
  });

  // ---- P6: filePath A→B race guard ----------------------------------
  it("does not overwrite current data with a late-arriving fetch from a previous filePath", async () => {
    // Stage two pending fetches in order: /a.md resolves later than /b.md.
    let resolveA!: (v: StoryDetails) => void;
    let resolveB!: (v: StoryDetails) => void;
    getStoryDetailsMock
      .mockImplementationOnce(() => new Promise<StoryDetails>((r) => (resolveA = r)))
      .mockImplementationOnce(() => new Promise<StoryDetails>((r) => (resolveB = r)));

    const { result, rerender } = renderHook(({ p }: { p: string }) => useStoryDetails(p, H), {
      initialProps: { p: "/a.md" },
    });
    // Switch to /b.md before /a's fetch resolves.
    rerender({ p: "/b.md" });
    // Resolve /b first (the current path), then /a (stale).
    resolveB({ title: "TB", storyText: "SB" });
    await waitFor(() => expect(result.current.data).toEqual({ title: "TB", storyText: "SB" }));
    resolveA({ title: "TA", storyText: "SA" });
    // Brief tick; data must STILL be /b's value, not clobbered by /a.
    await new Promise((r) => setTimeout(r, 5));
    expect(result.current.data).toEqual({ title: "TB", storyText: "SB" });
  });

  // ---- P7: preserve last-known-good on failed refetch ---------------
  it("preserves data when a same-path refetch fails after invalidation", async () => {
    getStoryDetailsMock
      .mockResolvedValueOnce({ title: "T1", storyText: "S1" })
      .mockRejectedValueOnce(new Error("transient"));
    const { result, rerender } = renderHook(() => useStoryDetails("/a.md", H));
    await waitFor(() => expect(result.current.data).toEqual({ title: "T1", storyText: "S1" }));

    // Watcher matches → invalidate → refetch (mocked to reject).
    useFileRefreshStore.setState({ lastChangedContextFile: { repoA: new Set(["/a.md"]) } });
    rerender();

    await waitFor(() => expect(getStoryDetailsMock).toHaveBeenCalledTimes(2));
    // Wait for the error handler to settle loading back to false.
    await waitFor(() => expect(result.current.loading).toBe(false));
    // Even though the refetch errored, the prior good data stays on screen.
    expect(result.current.data).toEqual({ title: "T1", storyText: "S1" });
  });

  // ---- P7 part 2: clear stale data on filePath transition ------------
  it("clears prior story data when filePath transitions to a new uncached path", async () => {
    let resolveB!: (v: StoryDetails) => void;
    getStoryDetailsMock
      .mockResolvedValueOnce({ title: "TA", storyText: "SA" })
      .mockImplementationOnce(() => new Promise<StoryDetails>((r) => (resolveB = r)));
    const { result, rerender } = renderHook(({ p }: { p: string }) => useStoryDetails(p, H), {
      initialProps: { p: "/a.md" },
    });
    await waitFor(() => expect(result.current.data).toEqual({ title: "TA", storyText: "SA" }));

    // Switch to a different path with no cached value. While /b's fetch
    // is in flight, `data` must be undefined — we don't want /a's preview
    // bleeding into /b's card.
    rerender({ p: "/b.md" });
    await waitFor(() => expect(result.current.loading).toBe(true));
    expect(result.current.data).toBeUndefined();
    resolveB({ title: "TB", storyText: "SB" });
    await waitFor(() => expect(result.current.data).toEqual({ title: "TB", storyText: "SB" }));
  });

  // ---- Code-review patch P5 / AC #36 --------------------------------
  it("evicts the least-recently-used entry when cache exceeds MAX_ENTRIES (250)", async () => {
    // Seed the cache directly with 250 entries — much faster than mounting
    // 250 hooks. The first seeded path has the smallest touchedAt tick, so
    // it's the LRU candidate once the 251st entry lands.
    const MAX = 250;
    for (let i = 0; i < MAX; i++) {
      _seedStoryDetailsCacheForTests(`/p-${i}.md`, H, { title: `T${i}`, storyText: `S${i}` });
    }
    expect(_storyDetailsCacheSizeForTests()).toBe(MAX);

    // Mount a hook for the 251st path — this triggers a fetch + storeAndEvict,
    // which should evict /p-0.md (smallest touchedAt) and leave cache at MAX.
    getStoryDetailsMock.mockResolvedValueOnce({ title: "T250", storyText: "S250" });
    const trigger = renderHook(() => useStoryDetails("/p-250.md", H));
    await waitFor(() =>
      expect(trigger.result.current.data).toEqual({ title: "T250", storyText: "S250" }),
    );
    expect(_storyDetailsCacheSizeForTests()).toBe(MAX);

    // /p-0.md should now be evicted — re-mounting it triggers a fresh fetch.
    getStoryDetailsMock.mockResolvedValueOnce({ title: "T0-refetched", storyText: "S0-refetched" });
    const evicted = renderHook(() => useStoryDetails("/p-0.md", H));
    await waitFor(() =>
      expect(evicted.result.current.data).toEqual({
        title: "T0-refetched",
        storyText: "S0-refetched",
      }),
    );

    // /p-249.md (most recently seeded, still cached) should hit synchronously
    // without firing a third Tauri call.
    const hit = renderHook(() => useStoryDetails("/p-249.md", H));
    expect(hit.result.current.data).toEqual({ title: "T249", storyText: "S249" });
    expect(getStoryDetailsMock).toHaveBeenCalledTimes(2);
  });
});

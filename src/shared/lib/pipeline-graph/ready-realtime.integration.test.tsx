// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";

// SW-FX-2 AC-5/AC-6 — realtime "ready to pick up" affordance.
//
// #246 wired `isReady` into the transform against the full index, and the
// hook-level rescan/short-circuit mechanics are covered by
// use-pipeline-tree.test.ts. What this suite locks down is the *end-to-end*
// behavior the story is named for: when a DEPENDENCY story's frontmatter
// `status` flips to `done`, the dependent node's rendered readiness flips
// live — driven through the genuine chain
//   fileRefreshKeys bump → usePipelineTree rescan → buildFullGroupIndex
//   → derivedTreeToReactFlow → node.data.isReady
// — with no refetch/restart, and
// no churn when a content-only edit leaves status/structure untouched.

async function flushPromises() {
  for (let i = 0; i < 5; i++) {
    await Promise.resolve();
  }
}

// Mock the scanner BEFORE importing the hook so the hook captures the mock.
const { scanMock } = vi.hoisted(() => ({ scanMock: vi.fn() }));
vi.mock("@/tauri/commands", async () => {
  const actual = await vi.importActual<typeof import("@/tauri/commands")>("@/tauri/commands");
  return { ...actual, scanPipelineTree: scanMock };
});

import { usePipelineTree } from "./use-pipeline-tree";
import { buildFullGroupIndex } from "./build-group-index";
import { derivedTreeToReactFlow } from "./derived-tree-to-react-flow";
import { findScopedSubtree } from "./pipeline-utils";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { seedFileRefresh } from "@/shared/stores/__test-utils__/seed-file-refresh";
import { buildTree, buildStoryRef } from "./test-fixtures/derived-tree";

const TEAM_CONTENT = "/org/_team/default/content";

function setHydrated(flag: boolean) {
  useConfigStore.setState({ initialized: flag });
  useViewerStore.setState({ stateLoaded: flag, scopeRecoveryComplete: flag });
}

function seedRepo() {
  useConfigStore.setState({
    repos: [
      {
        repoPath: "/r1",
        repoName: "r1",
        contextRepoPath: "/ctx/r1",
        teamContentPath: TEAM_CONTENT,
        hasRemote: false,
        remoteUrl: null,
        mainDoc: "",
      },
    ],
  });
}

// epic-a/a.md (todo) depends cross-epic on epic-b/B-1; readiness tracks B-1's
// status, resolved against the FULL index (scope-independent). `aTitle` lets a
// test produce a pure content edit (title is dropped from the structural
// fingerprint) without touching any status/structure.
function twoEpicTree(b1Status: string, aTitle: string | null = null) {
  return buildTree("", {
    subEpics: [
      buildTree("epic-a", {
        dirPath: "epic-a",
        stories: [
          buildStoryRef("a.md", {
            absolutePath: "/ctx/pipe/epic-a/a.md",
            status: "todo",
            title: aTitle,
            documentType: "story",
            dependsOn: ["../epic-b/B-1"],
          }),
        ],
      }),
      buildTree("epic-b", {
        dirPath: "epic-b",
        stories: [
          buildStoryRef("B-1.md", {
            absolutePath: "/ctx/pipe/epic-b/B-1.md",
            status: b1Status,
            documentType: "story",
          }),
        ],
      }),
    ],
  });
}

// Mirror of pipeline-viewer-panel's composition: full root scan → full index →
// transform. Returns the dependent node's readiness plus the committed tree
// reference (so churn can be observed).
function useDependentReady() {
  const { tree } = usePipelineTree("/r1", "");
  if (!tree) return { ready: undefined as boolean | undefined, treeRef: null as unknown };
  const fullGroupIndex = buildFullGroupIndex(tree);
  const { nodes } = derivedTreeToReactFlow(tree, { fullGroupIndex });
  const node = nodes.find((n) => n.id === "epic-a/a.md");
  return {
    ready: (node?.data as { isReady?: boolean } | undefined)?.isReady,
    treeRef: tree,
  };
}

// Faithful mirror of the DRILLED production path (pipeline-viewer-panel.tsx:295-299):
// the single full-root scan feeds BOTH the client-side scoped subtree
// (`findScopedSubtree`) the canvas renders AND the `fullGroupIndex`. Drilling into
// `epic-a` puts the blocking dependency `epic-b/B-1` OUTSIDE the rendered scope —
// the case the headline realtime behavior must still cover, since readiness
// resolves against the full index regardless of drill.
function useDrilledDependentReady(scopePath: string) {
  const { tree: fullTree } = usePipelineTree("/r1", "");
  if (!fullTree) return { ready: undefined as boolean | undefined, scopedRef: null as unknown };
  const scoped = findScopedSubtree(fullTree, scopePath);
  const fullGroupIndex = buildFullGroupIndex(fullTree);
  if (!scoped) return { ready: undefined as boolean | undefined, scopedRef: null as unknown };
  const { nodes } = derivedTreeToReactFlow(scoped, { fullGroupIndex });
  // When drilled, the node id is scope-root-relative ("a.md", not "epic-a/a.md").
  const node = nodes.find((n) => n.id === "a.md");
  return {
    ready: (node?.data as { isReady?: boolean } | undefined)?.isReady,
    scopedRef: scoped,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  scanMock.mockReset();
  setHydrated(false);
  useConfigStore.setState({ repos: [] });
  seedFileRefresh();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("SW-FX-2 realtime ready affordance", () => {
  it("AC-5: a dependency status→done flips the dependent node ready, live (no refetch)", async () => {
    seedRepo();
    setHydrated(true);
    scanMock
      .mockResolvedValueOnce(twoEpicTree("in-progress"))
      .mockResolvedValueOnce(twoEpicTree("done"));

    const { result } = renderHook(() => useDependentReady());
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    // Blocking dep is in-progress → dependent not ready.
    expect(result.current.ready).toBe(false);

    // Watcher fires on the dependency's frontmatter save → refresh key bumps.
    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    // Rescan committed a new tree (B-1 done) → full index rebuilt → re-derived
    // → dependent now ready. No refetch()/restart was called.
    expect(scanMock).toHaveBeenCalledTimes(2);
    expect(result.current.ready).toBe(true);
  });

  it("AC-6: a no-op content edit leaves readiness and the tree reference unchanged", async () => {
    seedRepo();
    setHydrated(true);
    const ready1 = twoEpicTree("done");
    // Same statuses/structure, only a story title differs — a pure content edit
    // that the structural fingerprint drops.
    const ready2 = twoEpicTree("done", "epic A, edited");
    scanMock.mockResolvedValueOnce(ready1).mockResolvedValueOnce(ready2);

    const { result } = renderHook(() => useDependentReady());
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.ready).toBe(true);
    const committed = result.current.treeRef;

    // A content-only rescan fires…
    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    // …the fingerprint short-circuit preserves the committed tree reference
    // (no downstream re-layout/re-derive in production), and readiness holds.
    expect(scanMock).toHaveBeenCalledTimes(2);
    expect(result.current.treeRef).toBe(committed);
    expect(result.current.ready).toBe(true);
  });

  it("AC-5 (drilled): an OUT-OF-SCOPE dependency flipping done flips the drilled node ready, live", async () => {
    // Drilled into epic-a; the blocking dep epic-b/B-1 is off-screen. Readiness
    // is resolved against the full index, so flipping B-1 done must still flip
    // the drilled epic-a/a.md node — even though B-1 never appears in the
    // rendered subtree. This is the scope-independent realtime path.
    seedRepo();
    setHydrated(true);
    scanMock
      .mockResolvedValueOnce(twoEpicTree("in-progress"))
      .mockResolvedValueOnce(twoEpicTree("done"));

    const { result } = renderHook(() => useDrilledDependentReady("epic-a"));
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });
    expect(result.current.ready).toBe(false);

    act(() => {
      seedFileRefresh({ "/r1": 1 });
    });
    await act(async () => {
      vi.advanceTimersByTime(150);
      await flushPromises();
    });

    expect(scanMock).toHaveBeenCalledTimes(2);
    expect(result.current.ready).toBe(true);
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, waitFor, act } from "@testing-library/react";
import type { Block } from "@/shared/lib/markdown-worker/types";

// RF-perf-12 (T3): markdown scroll survives an unmount→remount when a
// `viewStateKey` is provided — captured to the session slice on unmount, seeded
// back as the restore target on a fresh mount (via the existing blocks gate).

vi.mock("@/shared/lib/viewer-perf-config", () => ({
  VIEWER_PERF: {
    fileCacheMaxEntries: 25,
    keepAliveMaxViewers: 10,
    parseInWorker: true,
    renderMode: "blocks" as const,
  },
}));

const resolveMocks: { resolve?: (r: unknown) => void } = {};
vi.mock("@/shared/lib/markdown-worker/client", () => ({
  parseMarkdown: vi.fn(() => ({ promise: new Promise(() => {}), cancel: vi.fn() })),
  parseToBlocks: vi.fn(() => ({
    promise: new Promise((resolve) => {
      resolveMocks.resolve = resolve;
    }),
    cancel: vi.fn(),
  })),
}));

const mockReadFile = vi.fn<(path: string) => Promise<string>>();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => mockReadFile(...(args as [string])),
}));

// Real in-memory view-state map so we can assert the round-trip through the
// component (mirrors the production store's `viewStateByKey` semantics).
const viewState = new Map<string, { scrollTop?: number }>();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        settings: { contentWidth: "normal", appTheme: "dracula", viewerFontSize: "normal" },
        selectedRepoPath: "/repo",
      }),
    {
      getState: () => ({
        selectedRepoPath: "/repo",
        closeTab: vi.fn(),
        setOpenFileLifecycle: vi.fn(),
        readViewState: (k: string) => viewState.get(k),
        captureViewState: (k: string, snap: { scrollTop?: number }) => viewState.set(k, snap),
      }),
    },
  ),
}));

vi.mock("next-themes", () => ({ useTheme: () => ({ resolvedTheme: "dark" }) }));

import { MarkdownViewer } from "./markdown-viewer";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { parseResultCache } from "@/shared/lib/parse-result-cache";

const STUB_PERF = { perf: { parseMs: 1, runMs: 1, splitMs: 0, postAt: 0 }, receiveAt: 0 };
const BLOCKS: Block[] = [{ id: 0, kind: "html", html: "<h1>Doc</h1>" }];
const KEY = "acme/web:default::/repo/doc.md";

async function renderAndResolve() {
  const utils = await act(async () =>
    render(<MarkdownViewer filePath="/repo/doc.md" teamContentPath="" viewStateKey={KEY} />),
  );
  await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
  await act(async () => resolveMocks.resolve!({ blocks: BLOCKS, ...STUB_PERF }));
  await waitFor(() =>
    expect(utils.container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
  );
  return utils;
}

beforeEach(() => {
  vi.clearAllMocks();
  mockReadFile.mockResolvedValue("# Doc");
  fileContentCache.clear();
  parseResultCache.clear();
  resolveMocks.resolve = undefined;
  viewState.clear();
});

describe("MarkdownViewer view-state (RF-perf-12 T3)", () => {
  it("captures scroll on unmount and restores it on remount", async () => {
    const { container, unmount } = await renderAndResolve();
    const el = container.querySelector<HTMLElement>(".overflow-y-auto")!;
    el.scrollTop = 320;
    el.dispatchEvent(new Event("scroll"));

    unmount();
    // captured to the slice
    expect(viewState.get(KEY)).toEqual({ scrollTop: 320 });

    // Fresh mount seeds the restore target from the slice; after blocks land the
    // layout effect applies it (no flash). Clear caches so the parse path (and
    // thus the blocks-commit restore gate) runs again for the remount.
    resolveMocks.resolve = undefined;
    fileContentCache.clear();
    parseResultCache.clear();
    const { container: c2 } = await renderAndResolve();
    const el2 = c2.querySelector<HTMLElement>(".overflow-y-auto")!;
    expect(el2.scrollTop).toBe(320);
  });
});

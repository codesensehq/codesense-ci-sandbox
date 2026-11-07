// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useContextHistory } from "./use-context-history";
import type { HistoryPage } from "@/tauri/commands";

const read = vi.hoisted(() => vi.fn());
vi.mock("./repo-status-source", () => ({ readContextHistory: read }));
vi.mock("@/shared/hooks/use-window-focused", () => ({ useWindowVisible: () => true }));
const branches = { kind: "all" as const };
const privatePage = {
  commits: [{ sha: "private-commit" }],
  nextCursor: null,
  openPrs: [],
  branches: ["main"],
} as unknown as HistoryPage;
beforeEach(() => read.mockReset());

it("hides the previous repository's commits while the next read is pending or fails", async () => {
  read.mockResolvedValueOnce(privatePage);
  const hook = renderHook(({ root }) => useContextHistory(root, branches, null, 0), {
    initialProps: { root: "/private" },
  });
  await waitFor(() => expect(hook.result.current.page).toBe(privatePage));
  let reject!: (error: Error) => void;
  read.mockImplementationOnce(
    () =>
      new Promise((_resolve, fail) => {
        reject = fail;
      }),
  );
  hook.rerender({ root: "/clone" });
  expect(hook.result.current.page.commits).toEqual([]);
  await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  await act(async () => reject(new Error("read failed")));
  expect(hook.result.current.page.commits).toEqual([]);
  await waitFor(() => expect(hook.result.current.error).toContain("read failed"));
});

it("keeps the current repository's page during a background refresh", async () => {
  read.mockResolvedValueOnce(privatePage);
  const hook = renderHook(({ moved }) => useContextHistory("/private", branches, null, moved), {
    initialProps: { moved: 0 },
  });
  await waitFor(() => expect(hook.result.current.page).toBe(privatePage));
  read.mockImplementationOnce(() => new Promise(() => {}));
  hook.rerender({ moved: 1 });
  expect(hook.result.current.page).toBe(privatePage);
});

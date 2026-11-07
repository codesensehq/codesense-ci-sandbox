// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { useRunEvidence } from "./use-run-evidence";

const read = vi.hoisted(() => ({
  snapshot: undefined as HubSnapshot | undefined,
  error: undefined as string | undefined,
  reload: vi.fn(),
}));
vi.mock("./use-run-review", () => ({ useRunReview: () => read }));
const scope = { teamKey: "acme/ctx:team" };
beforeEach(() => {
  read.snapshot = undefined;
  read.error = undefined;
});
it("distinguishes loading, unavailable and source errors", () => {
  const { result, rerender } = renderHook(() => useRunEvidence("SW-A", "run-1", scope));
  expect(result.current.state.kind).toBe("loading");
  read.snapshot = { topic: "runEvidence", viewKey: "test", rev: 0, snapshot: [] };
  rerender();
  expect(result.current.state.kind).toBe("unavailable");
  read.snapshot = { ...read.snapshot, snapshot: [{ id: "error", error: "store unreadable" }] };
  rerender();
  expect(result.current.state).toEqual({ kind: "error", message: "store unreadable" });
});
it("requires the story as well as the run", () => {
  const { result } = renderHook(() => useRunEvidence(null, "run-1", scope));
  expect(result.current.state.kind).toBe("unavailable");
});
it("assembles node rows in source order from the same revision as the header", () => {
  read.snapshot = {
    topic: "runEvidence",
    viewKey: "test",
    rev: 2,
    snapshot: [
      { id: "evidence", evidence: { header: { summary: "Done" }, run: {}, authority: "durable" } },
      { id: "node:b", order: 1, result: { nodeId: "b" } },
      { id: "node:a", order: 0, result: { nodeId: "a" } },
    ],
  };
  const { result } = renderHook(() => useRunEvidence("SW-A", "run-1", scope));
  const state = result.current.state;
  expect(state.kind).toBe("ready");
  if (state.kind === "ready")
    expect(state.evidence.perNodeResults.map((n) => n.nodeId)).toEqual(["a", "b"]);
});

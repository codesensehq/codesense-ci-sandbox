// SPDX-License-Identifier: MIT
// SW-XS-10 AC4: an explicit operation (Step 3) lights the workspace pill's chip
// — `↓ Hydrating n files`, then `✓ Hydrated n files · 0.6 s`, which fades after
// 4 s; a failure sticks. Driven by the event sequence the XS-8 Rust test
// `explicit_operation_progress_golden_fixture` writes, so the chip and the
// emitter can't drift. Review r1: kept-modified results stick and their
// "Discard my edits" works from the chip, after the 4 s, through real events.

import { readFileSync } from "fs";
import { resolve } from "path";
import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { render } from "@/test-utils";
import {
  reducePrepareProgress,
  type AgentPrepareState,
} from "@/features/agents/lib/agent-prepare-progress";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { AgentPrepareProgressPayload, AgentPrepareSummary } from "@/tauri/events";

import { selectChipOperation, useContextOperationsStore } from "../stores/context-operations-store";
import { OperationChip } from "./operation-chip";

const cancel = vi.hoisted(() => vi.fn().mockResolvedValue(true));
const discard = vi.hoisted(() => vi.fn());
vi.mock("@/tauri/commands", () => ({
  cancelContextOperation: cancel,
  discardContextEdits: discard,
}));

const golden = JSON.parse(
  readFileSync(
    resolve(__dirname, "../../../tauri/fixtures/explicit-operation-progress.golden.json"),
    "utf-8",
  ),
) as { ready: AgentPrepareProgressPayload[]; cancelled: AgentPrepareProgressPayload[] };

const WS_PATH = "/wt/SW-1";
const ws = { id: "SW-1", kind: "story", cwd: WS_PATH, status: "ready" } as Workspace;

/** The fixture's hydrate, on this workspace, scaled to 18 files that took
 *  0.6 s (the AC's example) — same phases, same shape. */
function hydrate18(
  id = "op-1",
  summary: Partial<AgentPrepareSummary> = {},
): AgentPrepareProgressPayload[] {
  return golden.ready.map((e) => ({
    ...e,
    operationId: id,
    workspace: WS_PATH,
    agent: "session-7",
    operationKind: "hydrate" as const,
    files: e.files * 18,
    bytes: e.bytes * 18,
    ...(e.summary
      ? {
          summary: {
            ...e.summary,
            written: 18,
            bytesWritten: 108,
            elapsedMs: 600,
            ...summary,
          },
        }
      : {}),
  }));
}

const folded = new Map<string, AgentPrepareState>();
function emit(event: AgentPrepareProgressPayload): void {
  const key = event.operationId ?? "";
  const next = reducePrepareProgress(folded.get(key), event);
  folded.set(key, next);
  act(() => useContextOperationsStore.getState().apply(next));
}

const chipText = () => screen.queryByTestId("operation-chip-text")?.textContent ?? null;

beforeEach(() => {
  vi.useFakeTimers({ now: 5_000_000, toFake: ["setTimeout", "clearTimeout", "Date"] });
  useContextOperationsStore.getState().reset();
  folded.clear();
  cancel.mockReset().mockResolvedValue(true);
  discard.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("SW-XS-10 AC4: hydrate_worktree on the pill's chip", () => {
  it("shows ↓ Hydrating 18 files, then ✓ Hydrated 18 files · 0.6 s, and fades after 4 s", () => {
    render(<OperationChip workspace={ws} />);
    const seen: Array<string | null> = [];
    for (const event of hydrate18()) {
      emit(event);
      seen.push(chipText());
    }
    // Running: never per-file noise, and the count never runs backwards
    // (apply restarts its counters at zero).
    const first = seen.indexOf("Hydrating 18 files");
    expect(first).toBeGreaterThan(-1);
    expect(seen.slice(first).filter((t) => t === "Hydrating")).toEqual([]);
    expect(screen.getByTestId("operation-chip").getAttribute("data-tone")).toBe("done");
    expect(chipText()).toBe("Hydrated 18 files · 0.6 s");
    act(() => vi.advanceTimersByTime(3_000));
    expect(chipText()).toBe("Hydrated 18 files · 0.6 s");
    act(() => vi.advanceTimersByTime(800));
    expect(screen.getByTestId("operation-chip").className).toContain("opacity-0");
    act(() => vi.advanceTimersByTime(400));
    expect(chipText()).toBeNull();
  });

  it("a failure sticks until it is dismissed", () => {
    render(<OperationChip workspace={ws} />);
    const [diff] = hydrate18();
    emit(diff);
    emit({ ...diff, phase: "failed", note: "the machine went away" });
    act(() => vi.advanceTimersByTime(60_000));
    expect(chipText()).toBe("Hydrate failed");
    fireEvent.click(screen.getByTestId("operation-chip"));
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(chipText()).toBeNull();
  });

  it("is cancellable: Stop asks the backend to cancel that operation id", async () => {
    render(<OperationChip workspace={ws} />);
    const [diff, transfer] = hydrate18();
    emit(diff);
    emit(transfer);
    fireEvent.click(screen.getByTestId("operation-chip"));
    await act(async () => {
      fireEvent.click(screen.getByTestId("operation-stop"));
    });
    expect(cancel).toHaveBeenCalledWith("op-1");
    for (const event of golden.cancelled.slice(2)) emit({ ...event, workspace: WS_PATH });
    expect(chipText()).toBe("Hydrate stopped — nothing partial left");
  });

  it("Stop on an operation that already ended removes it instead of spinning on", async () => {
    cancel.mockResolvedValue(false);
    render(<OperationChip workspace={ws} />);
    const [diff, transfer] = hydrate18();
    emit(diff);
    emit(transfer);
    fireEvent.click(screen.getByTestId("operation-chip"));
    await act(async () => {
      fireEvent.click(screen.getByTestId("operation-stop"));
    });
    expect(useContextOperationsStore.getState().byId["op-1"]).toBeUndefined();
  });

  it("a publish that left conflicts unwritten is a partial result, and sticks", () => {
    render(<OperationChip workspace={ws} />);
    const base = {
      workspace: WS_PATH,
      agent: "Barb",
      operationId: "op-2",
      operationKind: "publish" as const,
      bytes: 0,
    };
    emit({ ...base, phase: "diff", files: 0, total: 5 });
    emit({
      ...base,
      phase: "ready",
      files: 3,
      total: 5,
      note: "Published 3 change(s); 2 conflict(s) not written.",
    });
    act(() => vi.advanceTimersByTime(10_000));
    expect(chipText()).toBe("Published 3 changes · 2 conflicts");
    expect(screen.getByTestId("operation-chip").getAttribute("data-tone")).toBe("partial");
    // The details don't repeat the counts in a second "(s)" line.
    fireEvent.click(screen.getByTestId("operation-chip"));
    expect(screen.queryByText(/conflict\(s\)/)).toBeNull();
  });

  it("a launch's Step 2 (no operation id) never lights the chip", () => {
    render(<OperationChip workspace={ws} />);
    const { operationId: _drop, ...launch } = hydrate18()[0];
    act(() =>
      useContextOperationsStore
        .getState()
        .apply(reducePrepareProgress(undefined, launch as AgentPrepareProgressPayload)),
    );
    expect(chipText()).toBeNull();
  });
});

describe("SW-XS-10 review r1 (MUST 1): Discard my edits from the chip", () => {
  const kept = { keptModified: ["CLAUDE.md"], keptModifiedCount: 1 };

  it("a result that kept modified files sticks past 4 s; the details stay pinned; discard runs and shows its result in place", async () => {
    render(<OperationChip workspace={ws} />);
    for (const event of hydrate18("op-1", kept)) emit(event);
    // Long after the fade would have run, it is still there to click.
    act(() => vi.advanceTimersByTime(30_000));
    expect(chipText()).toBe("Hydrated 18 files · 1 kept");
    fireEvent.click(screen.getByTestId("operation-chip"));
    expect(screen.getByTestId("operation-details-op-1").textContent).toContain(
      "1 file kept (modified)",
    );
    fireEvent.click(screen.getByRole("button", { name: "Discard my edits" }));
    expect(discard).not.toHaveBeenCalled();
    // The discard is itself an explicit operation: its real events arrive while
    // the popover is open and must not swap the details out from under it.
    let finish: (v: unknown) => void = () => {};
    discard.mockReturnValue(new Promise((r) => (finish = r)));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace with team's version" }));
    });
    expect(discard).toHaveBeenCalledWith(
      { kind: "local", codeRepoPath: WS_PATH, sessionId: "session-7" },
      ["CLAUDE.md"],
    );
    for (const event of hydrate18("op-discard", {
      written: 1,
      keptModified: [],
      keptModifiedCount: 0,
    }))
      emit(event);
    act(() => vi.advanceTimersByTime(5_000));
    expect(screen.getByTestId("operation-details-op-1")).toBeTruthy();
    await act(async () =>
      finish({ status: "ready", written: 1, unchanged: 0, keptModifiedCount: 0, failedCount: 0 }),
    );
    expect(screen.getByTestId("kept-modified-outcome").textContent).toBe(
      "Replaced 1 file with the team's version",
    );
    // Closing the details lets the chip move on: the original's "kept" is
    // resolved, so the newer operation (the discard itself) shows.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(chipText()).toBe("Hydrated 1 file · 0.6 s");
  });

  it("a partial discard rebuilds the list; a file already the team's version counts as done", async () => {
    render(<OperationChip workspace={ws} />);
    for (const event of hydrate18("op-1", {
      keptModified: ["CLAUDE.md", "AGENTS.md", "docs/a.md"],
      keptModifiedCount: 3,
    }))
      emit(event);
    fireEvent.click(screen.getByTestId("operation-chip"));
    discard.mockResolvedValue({
      status: "failed",
      written: 1,
      unchanged: 1,
      keptModified: [],
      keptModifiedCount: 0,
      failedCount: 1,
      failed: [["docs/a.md", "permission denied"]],
    });
    fireEvent.click(screen.getByRole("button", { name: "Discard my edits" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace with team's version" }));
    });
    expect(screen.getByRole("alert").textContent).toBe(
      "Replaced 2 of 3; 1 file still kept (docs/a.md: permission denied)",
    );
    expect(screen.getByTestId("kept-modified-label").textContent).toBe("1 file kept (modified)");
    // A second discard asks only for what is left.
    fireEvent.click(screen.getByRole("button", { name: "Discard my edits" }));
    discard.mockResolvedValue({ status: "ready", written: 1, failedCount: 0 });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace with team's version" }));
    });
    expect(discard).toHaveBeenLastCalledWith(expect.anything(), ["docs/a.md"]);
  });
});

describe("SW-XS-10 review r1: the operations store", () => {
  const ev = (id: string, phase: AgentPrepareProgressPayload["phase"], extra = {}) =>
    reducePrepareProgress(undefined, {
      workspace: WS_PATH,
      agent: "s",
      operationId: id,
      operationKind: "hydrate",
      phase,
      files: 0,
      bytes: 0,
      total: 1,
      ...extra,
    });
  const store = () => useContextOperationsStore.getState();

  it("two operations at once: the chip shows the running one; each settles on its own", () => {
    store().apply(ev("a", "diff"), 1);
    store().apply(ev("b", "diff"), 2);
    store().apply(ev("a", "ready"), 3);
    expect(selectChipOperation(store().byId, [WS_PATH], 4)?.id).toBe("b");
    store().apply(ev("b", "failed"), 5);
    expect(selectChipOperation(store().byId, [WS_PATH], 6)?.id).toBe("b");
    expect(store().byId.a.settledAt).toBe(3);
  });

  it("keeps at most 40, dropping the oldest settled", () => {
    for (let i = 0; i < 45; i++) store().apply(ev(`op-${i}`, "ready"), i);
    expect(Object.keys(store().byId)).toHaveLength(40);
    expect(store().byId["op-0"]).toBeUndefined();
    expect(store().byId["op-44"]).toBeDefined();
  });

  it("a repeated terminal event keeps it dismissed and does not restart its fade", () => {
    store().apply(ev("a", "failed"), 10);
    store().dismiss("a");
    store().apply(ev("a", "failed", { note: "again" }), 99);
    expect(store().byId.a).toMatchObject({ dismissed: true, settledAt: 10 });
    expect(store().byId.a.state.note).toBe("again");
  });

  it("a late tick after the settle is ignored; a new run (its diff) reopens the id", () => {
    store().apply(ev("a", "ready"), 10);
    store().apply(ev("a", "transfer", { files: 5 }), 11);
    expect(store().byId.a.settledAt).toBe(10);
    store().apply(ev("a", "diff"), 20);
    expect(store().byId.a.settledAt).toBeUndefined();
    expect(store().byId.a.startedAt).toBe(20);
  });
});

describe("SW-XS-10 review r1 (S2/S3/S5)", () => {
  it("S2: a Stop false-reply never removes a settled operation", () => {
    const store = useContextOperationsStore.getState();
    for (const event of hydrate18()) emit(event);
    store.remove("op-1");
    expect(useContextOperationsStore.getState().byId["op-1"]).toBeDefined();
  });

  it("S3: after a full discard the original operation no longer holds the chip with '· 1 kept'", async () => {
    render(<OperationChip workspace={ws} />);
    for (const event of hydrate18("op-1", { keptModified: ["CLAUDE.md"], keptModifiedCount: 1 }))
      emit(event);
    fireEvent.click(screen.getByTestId("operation-chip"));
    discard.mockResolvedValue({
      status: "ready",
      written: 1,
      failedCount: 0,
      keptModifiedCount: 0,
    });
    fireEvent.click(screen.getByRole("button", { name: "Discard my edits" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace with team's version" }));
    });
    expect(useContextOperationsStore.getState().byId["op-1"].keptResolved).toBe(true);
    // Its line drops the "kept" and the chip lets go once the details close.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(chipText()).toBe("Hydrated 18 files · 0.6 s");
    act(() => vi.advanceTimersByTime(3_800));
    act(() => vi.advanceTimersByTime(400));
    expect(chipText()).toBeNull();
  });

  it("S5: a file the team no longer has is reported as such", async () => {
    render(<OperationChip workspace={ws} />);
    for (const event of hydrate18("op-1", {
      keptModified: ["CLAUDE.md", "old.md"],
      keptModifiedCount: 2,
    }))
      emit(event);
    fireEvent.click(screen.getByTestId("operation-chip"));
    discard.mockResolvedValue({
      status: "ready",
      written: 1,
      failedCount: 0,
      keptModifiedCount: 0,
      notInTeam: ["old.md"],
    });
    fireEvent.click(screen.getByRole("button", { name: "Discard my edits" }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Replace with team's version" }));
    });
    expect(screen.getByTestId("kept-modified-outcome").textContent).toBe(
      "Replaced 1 file with the team's version; old.md is no longer in team content",
    );
  });
});

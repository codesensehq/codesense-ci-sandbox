// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";

const wvMarkApprovalSurfaced = vi.fn(() => Promise.resolve());
vi.mock("@/tauri/commands", () => ({
  wvMarkApprovalSurfaced: (...a: unknown[]) => wvMarkApprovalSurfaced(...(a as [])),
  // Reached transitively by EndorsementPill.
  wvRatify: vi.fn(() => Promise.resolve({ status: "ok" })),
  wvRevoke: vi.fn(() => Promise.resolve({ status: "ok" })),
  wvSettleApproval: vi.fn(() => Promise.resolve(true)),
}));

import {
  WeftBoardYouOweRow,
  approvalRowModel,
  assertionRowModel,
  type EndorsementRowModel,
} from "./weft-board-you-owe-row";

// A controllable IntersectionObserver stub — capture the callback so a test can fire it.
let ioCallback: IntersectionObserverCallback | null = null;
const observe = vi.fn();
const disconnect = vi.fn();
class MockIO {
  constructor(cb: IntersectionObserverCallback) {
    ioCallback = cb;
  }
  observe = observe;
  disconnect = disconnect;
  unobserve = vi.fn();
  takeRecords = vi.fn(() => []);
}

function fireIntersect(isIntersecting: boolean) {
  ioCallback?.([{ isIntersecting } as IntersectionObserverEntry], {} as IntersectionObserver);
}

function t2Approval(overrides: Partial<EndorsementRowModel> = {}): EndorsementRowModel {
  return {
    ...approvalRowModel({
      id: 5,
      tier: "T2",
      claim: "restart the worker",
      postedAt: "2026-09-02T00:00:00Z",
      source: "slack",
      windowState: "open",
      disposition: "pending",
      ttlS: 60,
      acquiredAt: 1000,
      pauseAccumulatedS: 0,
      surfacedSession: "sess-A",
      notSurfacedButExpired: false,
    }),
    ...overrides,
  };
}

describe("SW-WV-F5 · WeftBoardYouOweRow (AC-1, AC-4, AC-5)", () => {
  beforeEach(() => {
    ioCallback = null;
    observe.mockClear();
    disconnect.mockClear();
    wvMarkApprovalSurfaced.mockClear();
    vi.stubGlobal("IntersectionObserver", MockIO as unknown as typeof IntersectionObserver);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("renders the truncated claim, channel and the endorsement pill (AC-1)", () => {
    render(<WeftBoardYouOweRow row={t2Approval()} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    expect(screen.getByText("restart the worker")).toBeInTheDocument();
    expect(screen.getByText("slack")).toBeInTheDocument();
    expect(screen.getByTestId("endorsement-pill")).toBeInTheDocument();
  });

  it("truncates a long claim to ~140 chars", () => {
    const long = "x".repeat(300);
    render(
      <WeftBoardYouOweRow
        row={t2Approval({ claim: long })}
        onRatified={vi.fn()}
        onRevoked={vi.fn()}
      />,
    );
    const el = screen.getByTitle(long);
    expect(el.textContent?.length).toBeLessThanOrEqual(140);
    expect(el.textContent?.endsWith("…")).toBe(true);
  });

  it("marks a T2 approval surfaced once on first viewport entry (AC-4)", () => {
    render(<WeftBoardYouOweRow row={t2Approval()} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    expect(wvMarkApprovalSurfaced).not.toHaveBeenCalled();
    fireIntersect(true);
    expect(wvMarkApprovalSurfaced).toHaveBeenCalledTimes(1);
    expect(wvMarkApprovalSurfaced).toHaveBeenCalledWith({ id: 5, sessionId: "sess-A" });
    // A second scroll-into-view on the SAME mount does not re-fire (local surfaced ref).
    fireIntersect(true);
    expect(wvMarkApprovalSurfaced).toHaveBeenCalledTimes(1);
  });

  it("does not mark surfaced when the row leaves the viewport (isIntersecting=false)", () => {
    render(<WeftBoardYouOweRow row={t2Approval()} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    fireIntersect(false);
    expect(wvMarkApprovalSurfaced).not.toHaveBeenCalled();
  });

  it("re-arms the surfaced call after a remount (Rust idempotency handles cross-mount)", () => {
    const { unmount } = render(
      <WeftBoardYouOweRow row={t2Approval()} onRatified={vi.fn()} onRevoked={vi.fn()} />,
    );
    fireIntersect(true);
    expect(wvMarkApprovalSurfaced).toHaveBeenCalledTimes(1);
    unmount();
    render(<WeftBoardYouOweRow row={t2Approval()} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    fireIntersect(true);
    expect(wvMarkApprovalSurfaced).toHaveBeenCalledTimes(2);
  });

  it("does NOT observe / mark for an assertion row (no approval backing)", () => {
    const row = assertionRowModel(
      {
        id: 8,
        sessionId: "sess-B",
        runId: "run-1",
        claim: "posted to slack",
        channel: "slack",
        postedAt: "2026-09-02T00:00:00Z",
        disposition: "pending",
      },
      { seen: 1, acknowledged: 0, declined: 0 },
    );
    render(<WeftBoardYouOweRow row={row} onRatified={vi.fn()} onRevoked={vi.fn()} />);
    fireIntersect(true);
    expect(wvMarkApprovalSurfaced).not.toHaveBeenCalled();
    // The recipient-ack echo renders for the assertion row (AC-5).
    expect(screen.getByTestId("recipient-ack-echo")).toBeInTheDocument();
  });
});

// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";

import type { RunAuditHeader } from "@/shared/lib/run-evidence";
import type { WvPendingAssertionDto } from "@/tauri/commands";

const wvRatify = vi.fn<(id: number) => Promise<unknown>>();
const wvRevoke = vi.fn<(id: number, note?: string) => Promise<unknown>>();
vi.mock("@/tauri/commands", () => ({
  wvRatify: (id: number) => wvRatify(id),
  wvRevoke: (id: number, note?: string) => wvRevoke(id, note),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { WeftReviewHeader } from "./weft-review-header";

const ASSERTION: WvPendingAssertionDto = {
  id: 42,
  sessionId: "sess-1",
  runId: "run-1",
  claim: "PR opened",
  channel: "slack:#general",
  postedAt: "2026-09-19T00:00:00Z",
  disposition: "pending",
};

const HEADER: RunAuditHeader = {
  runId: "run-1",
  storyId: "SW-A",
  trigger: { kind: "attributed", source: "watch:ci" },
  status: "success",
  summary: "PR opened and tests green",
};

beforeEach(() => {
  wvRatify.mockReset().mockResolvedValue({ status: "ok", id: 42 });
  wvRevoke.mockReset().mockResolvedValue({ status: "ok", id: 42 });
});

describe("WeftReviewHeader (SW-WV-F4 AC-1/AC-5/AC-6)", () => {
  it("renders the chip, summary and both actions when a pending assertion exists (AC-1)", async () => {
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());
    // `success` maps to the `completed` status, which reads Ended.
    expect(screen.getByLabelText("Status: ended")).toBeInTheDocument();
    expect(screen.getByText("PR opened and tests green")).toBeInTheDocument();
    expect(screen.getByTestId("weft-review-ratify")).toBeInTheDocument();
    expect(screen.getByTestId("weft-review-send-back")).toBeInTheDocument();
  });

  it("renders nothing when there are no pending assertions (AC-1)", async () => {
    render(<WeftReviewHeader assertions={[]} header={HEADER} />);
    expect(screen.queryByTestId("weft-review-header")).not.toBeInTheDocument();
  });

  it("Ratify calls wv_ratify, disables the buttons + shows a spinner while in flight (AC-5)", async () => {
    const user = userEvent.setup();
    let resolveRatify: (v: unknown) => void = () => {};
    wvRatify.mockReturnValue(new Promise((r) => (resolveRatify = r)));
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());

    await user.click(screen.getByTestId("weft-review-ratify"));
    expect(wvRatify).toHaveBeenCalledWith(42);
    expect(screen.getByTestId("weft-review-spinner")).toBeInTheDocument();
    expect(screen.getByTestId("weft-review-ratify")).toBeDisabled();
    expect(screen.getByTestId("weft-review-send-back")).toBeDisabled();

    resolveRatify({ status: "ok", id: 42 });
    await waitFor(() =>
      expect(screen.queryByTestId("weft-review-spinner")).not.toBeInTheDocument(),
    );
  });

  it("Send back with no note reveals the note field then confirms with undefined (AC-6)", async () => {
    const user = userEvent.setup();
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());

    // First click reveals the inline note field (no command yet).
    await user.click(screen.getByTestId("weft-review-send-back"));
    expect(wvRevoke).not.toHaveBeenCalled();
    expect(screen.getByTestId("weft-review-note")).toBeInTheDocument();

    // Confirm with no typed note → undefined note argument.
    await user.click(screen.getByTestId("weft-review-send-back"));
    expect(wvRevoke).toHaveBeenCalledWith(42, undefined);
    // The "Sent back" toast shows only once the revoke resolves (not before).
    await waitFor(() => expect(screen.getByTestId("weft-review-sent-back")).toBeInTheDocument());
  });

  it("does not show the 'Sent back' toast when wv_revoke fails (AC-6)", async () => {
    const user = userEvent.setup();
    wvRevoke.mockRejectedValue(new Error("store locked"));
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());

    await user.click(screen.getByTestId("weft-review-send-back"));
    await user.click(screen.getByTestId("weft-review-send-back"));
    expect(wvRevoke).toHaveBeenCalledWith(42, undefined);
    // The revoke rejected — no false confirmation, and the buttons re-enable.
    await waitFor(() => expect(screen.getByTestId("weft-review-ratify")).not.toBeDisabled());
    expect(screen.queryByTestId("weft-review-sent-back")).not.toBeInTheDocument();
  });

  it("Send back with a typed note passes it to wv_revoke (AC-6)", async () => {
    const user = userEvent.setup();
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());

    await user.click(screen.getByTestId("weft-review-send-back"));
    await user.type(screen.getByTestId("weft-review-note"), "please fix the flaky test");
    await user.click(screen.getByTestId("weft-review-send-back"));
    expect(wvRevoke).toHaveBeenCalledWith(42, "please fix the flaky test");
  });

  it("Cancel collapses the note field without calling any command (AC-6)", async () => {
    const user = userEvent.setup();
    render(<WeftReviewHeader assertions={[ASSERTION]} header={HEADER} />);
    await waitFor(() => expect(screen.getByTestId("weft-review-header")).toBeInTheDocument());

    await user.click(screen.getByTestId("weft-review-send-back"));
    await user.click(screen.getByTestId("weft-review-cancel"));
    expect(screen.queryByTestId("weft-review-note")).not.toBeInTheDocument();
    expect(wvRevoke).not.toHaveBeenCalled();
    expect(wvRatify).not.toHaveBeenCalled();
  });
});

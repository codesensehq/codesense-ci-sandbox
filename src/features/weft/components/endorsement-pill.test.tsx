// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@/test-utils";

const wvRatify = vi.fn(() => Promise.resolve({ status: "ok" }));
const wvRevoke = vi.fn(() => Promise.resolve({ status: "ok" }));
const wvSettleApproval = vi.fn(() => Promise.resolve(true));
vi.mock("@/tauri/commands", () => ({
  wvRatify: (...a: unknown[]) => wvRatify(...(a as [])),
  wvRevoke: (...a: unknown[]) => wvRevoke(...(a as [])),
  wvSettleApproval: (...a: unknown[]) => wvSettleApproval(...(a as [])),
}));

import { EndorsementPill } from "./endorsement-pill";

describe("SW-WV-F5 · EndorsementPill (AC-1, AC-2, AC-8)", () => {
  beforeEach(() => {
    wvRatify.mockReset().mockResolvedValue({ status: "ok" });
    wvRevoke.mockReset().mockResolvedValue({ status: "ok" });
    wvSettleApproval.mockReset().mockResolvedValue(true);
  });

  it("renders the PENDING badge with Ratify + Revoke actions", () => {
    render(
      <EndorsementPill id={7} disposition="pending" onRatified={vi.fn()} onRevoked={vi.fn()} />,
    );
    expect(screen.getByText("Pending")).toBeInTheDocument();
    expect(screen.getByTestId("endorsement-ratify")).toBeInTheDocument();
    expect(screen.getByTestId("endorsement-revoke")).toBeInTheDocument();
  });

  it("Ratify calls wvRatify once, transitions to RATIFIED, and fires onRatified (AC-1)", async () => {
    const onRatified = vi.fn();
    render(
      <EndorsementPill id={7} disposition="pending" onRatified={onRatified} onRevoked={vi.fn()} />,
    );
    fireEvent.click(screen.getByTestId("endorsement-ratify"));
    await waitFor(() => expect(wvRatify).toHaveBeenCalledTimes(1));
    expect(wvRatify).toHaveBeenCalledWith(7);
    await waitFor(() =>
      expect(screen.getByTestId("endorsement-pill")).toHaveAttribute(
        "data-disposition",
        "ratified",
      ),
    );
    expect(onRatified).toHaveBeenCalledWith(7);
  });

  it("a second rapid click while in-flight is ignored (busy guard, AC-8)", async () => {
    let resolve!: () => void;
    wvRatify.mockReturnValue(
      new Promise<{ status: string }>((r) => (resolve = () => r({ status: "ok" }))),
    );
    render(
      <EndorsementPill id={7} disposition="pending" onRatified={vi.fn()} onRevoked={vi.fn()} />,
    );
    const btn = screen.getByTestId("endorsement-ratify");
    fireEvent.click(btn);
    fireEvent.click(btn);
    expect(wvRatify).toHaveBeenCalledTimes(1);
    resolve();
    await waitFor(() => expect(wvRatify).toHaveBeenCalledTimes(1));
  });

  it("a rejected call shows an inline error with Retry — no toast (AC-1)", async () => {
    wvRatify.mockRejectedValueOnce(new Error("network down"));
    render(
      <EndorsementPill id={7} disposition="pending" onRatified={vi.fn()} onRevoked={vi.fn()} />,
    );
    fireEvent.click(screen.getByTestId("endorsement-ratify"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("network down"));
    expect(screen.getByTestId("endorsement-retry")).toBeInTheDocument();
  });

  it("Revoke calls wvRevoke and tombstones via onRevoked", async () => {
    const onRevoked = vi.fn();
    render(
      <EndorsementPill id={9} disposition="pending" onRatified={vi.fn()} onRevoked={onRevoked} />,
    );
    fireEvent.click(screen.getByTestId("endorsement-revoke"));
    await waitFor(() => expect(wvRevoke).toHaveBeenCalledWith(9));
    expect(onRevoked).toHaveBeenCalledWith(9);
  });

  it("a T3 approval opens a confirm dialog; Confirm settles ratified via wvSettleApproval (AC-2)", async () => {
    render(
      <EndorsementPill
        id={3}
        disposition="pending"
        requiresDialog
        approvalId={42}
        claim="drop the prod table"
        onRatified={vi.fn()}
        onRevoked={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("endorsement-ratify"));
    expect(await screen.findByTestId("endorsement-confirm-dialog")).toBeInTheDocument();
    expect(screen.getByText("drop the prod table")).toBeInTheDocument();
    // Not settled until confirmed.
    expect(wvSettleApproval).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("endorsement-confirm-ratify"));
    await waitFor(() => expect(wvSettleApproval).toHaveBeenCalledWith(42, "ratified"));
  });

  it("a T3 Cancel closes the dialog without settling (AC-2)", async () => {
    render(
      <EndorsementPill
        id={3}
        disposition="pending"
        requiresDialog
        approvalId={42}
        claim="drop the prod table"
        onRatified={vi.fn()}
        onRevoked={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("endorsement-ratify"));
    expect(await screen.findByTestId("endorsement-confirm-dialog")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("endorsement-confirm-cancel"));
    await waitFor(() =>
      expect(screen.queryByTestId("endorsement-confirm-dialog")).not.toBeInTheDocument(),
    );
    expect(wvSettleApproval).not.toHaveBeenCalled();
  });

  it("a T3 Revoke settles revoked directly — no dialog (AC-2)", async () => {
    render(
      <EndorsementPill
        id={3}
        disposition="pending"
        requiresDialog
        approvalId={42}
        claim="drop the prod table"
        onRatified={vi.fn()}
        onRevoked={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("endorsement-revoke"));
    await waitFor(() => expect(wvSettleApproval).toHaveBeenCalledWith(42, "revoked"));
    expect(screen.queryByTestId("endorsement-confirm-dialog")).not.toBeInTheDocument();
  });
});

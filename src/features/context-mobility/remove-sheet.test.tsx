// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { personalContextId, teamAllContextId, type Context } from "@/shared/lib/context-model";

const removeSpy = vi.fn();
vi.mock("@/tauri/commands", () => ({
  removeContentFromContext: (...args: unknown[]) => removeSpy(...args),
}));

const toastSuccess = vi.fn();
const toastError = vi.fn();
const toastInfo = vi.fn();
vi.mock("sonner", () => ({
  toast: {
    success: (m: string) => toastSuccess(m),
    error: (m: string) => toastError(m),
    info: (m: string) => toastInfo(m),
  },
}));

import { RemoveSheet, type RemoveSheetProps } from "./remove-sheet";

const personal: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Private",
};
const teamAll: Context = {
  id: teamAllContextId("acme/repo", "default"),
  kind: "team-all",
  sharing: "shared",
  label: "All of default",
  teamName: "default",
};

function setup(overrides: Partial<RemoveSheetProps> = {}) {
  const onClose = vi.fn();
  const onRemoved = vi.fn();
  const props: RemoveSheetProps = {
    open: true,
    logicalName: "note.md",
    fileName: "note.md",
    source: personal,
    userId: 42,
    onClose,
    onRemoved,
    ...overrides,
  };
  render(<RemoveSheet {...props} />);
  return { props, onClose, onRemoved };
}

beforeEach(() => {
  removeSpy.mockReset().mockResolvedValue({ kind: "removed" });
  toastSuccess.mockReset();
  toastError.mockReset();
  toastInfo.mockReset();
});

describe("RemoveSheet — shared item (normal confirm)", () => {
  it("a shared item removes on a plain confirm", async () => {
    const { onRemoved } = setup({ source: teamAll });
    expect(screen.getByTestId("remove-gate-normal")).toBeInTheDocument();
    expect(screen.getByTestId("remove-sheet-confirm")).toHaveTextContent("Remove");
    fireEvent.click(screen.getByTestId("remove-sheet-confirm"));
    await waitFor(() => expect(removeSpy).toHaveBeenCalledTimes(1));
    expect(removeSpy.mock.calls[0][0].context).toEqual({
      kind: "teamAll",
      orgSlug: "acme/repo",
      teamName: "default",
    });
    expect(onRemoved).toHaveBeenCalled();
    expect(toastSuccess).toHaveBeenCalledWith("Removed 'note.md'");
  });
});

describe("RemoveSheet — scratch sole-copy gate (§J)", () => {
  it("warns 'only on this machine' with Delete permanently + Cancel", () => {
    setup();
    expect(screen.getByTestId("remove-gate-soleCopyBlock")).toBeInTheDocument();
    expect(screen.getByTestId("remove-sheet-header")).toHaveTextContent("only on this machine");
    // Delete permanently is the ONLY discarding path.
    expect(screen.getByTestId("remove-sheet-confirm")).toHaveTextContent("Delete permanently");
    expect(screen.getByTestId("remove-sheet-cancel")).toBeInTheDocument();
  });

  it("Delete permanently discards (calls the backend delete)", async () => {
    setup();
    fireEvent.click(screen.getByTestId("remove-sheet-confirm"));
    await waitFor(() => expect(removeSpy).toHaveBeenCalledTimes(1));
    expect(toastSuccess).toHaveBeenCalledWith("Removed 'note.md'");
  });

  it("Cancel leaves it — no delete fires", () => {
    const { onClose } = setup();
    fireEvent.click(screen.getByTestId("remove-sheet-cancel"));
    expect(removeSpy).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });
});

describe("RemoveSheet — outcomes never dead-end", () => {
  it("a source-missing remove is an informative confirmation", async () => {
    removeSpy.mockResolvedValue({ kind: "rejectedSourceMissing" });
    setup({ source: teamAll });
    fireEvent.click(screen.getByTestId("remove-sheet-confirm"));
    await waitFor(() => expect(toastInfo).toHaveBeenCalled());
    expect(toastInfo.mock.calls[0][0]).toContain("already gone");
  });

  it("a thrown I/O failure invites a retry (item intact)", async () => {
    removeSpy.mockRejectedValue(new Error("disk"));
    setup({ source: teamAll });
    fireEvent.click(screen.getByTestId("remove-sheet-confirm"));
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(toastError.mock.calls[0][0]).toContain("Try again");
  });
});

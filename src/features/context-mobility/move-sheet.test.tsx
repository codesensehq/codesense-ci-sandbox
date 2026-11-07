// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { personalContextId, teamAllContextId, type Context } from "@/shared/lib/context-model";

const moveSpy = vi.fn();
vi.mock("@/tauri/commands", () => ({
  moveContentBetweenContexts: (...args: unknown[]) => moveSpy(...args),
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

import { MoveSheet, type MoveSheetProps } from "./move-sheet";

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

function setup(overrides: Partial<MoveSheetProps> = {}) {
  const onClose = vi.fn();
  const onMoved = vi.fn();
  const props: MoveSheetProps = {
    open: true,
    logicalName: "note.md",
    fileName: "note.md",
    source: personal,
    target: teamAll,
    userId: 42,
    targetCollision: false,
    targetHas: () => false,
    onClose,
    onMoved,
    ...overrides,
  };
  render(<MoveSheet {...props} />);
  return { onClose, onMoved, props };
}

beforeEach(() => {
  moveSpy.mockReset();
  moveSpy.mockResolvedValue({ kind: "moved" });
  toastSuccess.mockReset();
  toastError.mockReset();
  toastInfo.mockReset();
});

describe("MoveSheet — §P stepped ladder", () => {
  it("a clean shared->Personal no-collision move fires with ZERO gates", async () => {
    const { onMoved } = setup({
      source: teamAll,
      target: personal,
    });
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(screen.queryByTestId("move-gate-privacy")).toBeNull();
    expect(onMoved).toHaveBeenCalled();
    expect(toastSuccess).toHaveBeenCalledWith("Moved 'note.md' to Private");
  });

  it("Personal->Team fires the privacy step even with no collision; Cancel leaves both untouched", async () => {
    const { onClose } = setup({ source: personal, target: teamAll });
    expect(screen.getByTestId("move-gate-privacy")).toBeTruthy();
    expect(moveSpy).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("move-sheet-cancel"));
    expect(moveSpy).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("Share it proceeds from the privacy gate straight to the move (no sole-copy step)", async () => {
    setup({ source: personal, target: teamAll });
    fireEvent.click(screen.getByTestId("move-gate-privacy-confirm"));
    expect(screen.queryByTestId("move-gate-sole-copy")).toBeNull();
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(toastSuccess).toHaveBeenCalledWith("Moved 'note.md' to All of default");
  });

  it("gate order is privacy -> collision; each renders one at a time", async () => {
    setup({
      source: personal,
      target: teamAll,
      targetCollision: true,
      targetHas: (c) => c === "note.md",
    });
    // Step 1: privacy.
    expect(screen.getByTestId("move-sheet-header").textContent).toContain("Step 1 of 2");
    expect(screen.getByTestId("move-gate-privacy")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-gate-privacy-confirm"));

    // Step 2: collision — Keep both surfaces the renamed result in the toast.
    expect(screen.getByTestId("move-sheet-header").textContent).toContain("Step 2 of 2");
    expect(screen.getByTestId("move-gate-collision")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-gate-collision-keep-both"));

    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    const call = moveSpy.mock.calls[0][0];
    expect(call.toLogicalName).toBe("note-2.md");
    expect(toastSuccess).toHaveBeenCalledWith("Moved to All of default as 'note-2.md'");
  });

  it("collision Replace passes force=true", async () => {
    setup({
      source: teamAll,
      target: personal,
      targetCollision: true,
    });
    expect(screen.getByTestId("move-gate-collision")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-gate-collision-replace"));
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(moveSpy.mock.calls[0][0].force).toBe(true);
  });

  it("§N: a thrown target-write failure surfaces the non-destructive retry toast", async () => {
    moveSpy.mockRejectedValueOnce(new Error("io"));
    setup({ source: teamAll, target: personal });
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(toastError.mock.calls[0][0]).toContain("still where it was");
  });

  it("Back at step 2 returns to step 1 without moving", async () => {
    setup({ source: personal, target: teamAll, targetCollision: true });
    fireEvent.click(screen.getByTestId("move-gate-privacy-confirm"));
    expect(screen.getByTestId("move-gate-collision")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-sheet-back"));
    expect(screen.getByTestId("move-gate-privacy")).toBeTruthy();
    expect(moveSpy).not.toHaveBeenCalled();
  });
});

describe("MoveSheet — SLW-PT-13 COPY (op:'copy')", () => {
  beforeEach(() => {
    moveSpy.mockResolvedValue({ kind: "copied" });
  });

  it("a clean shared->Personal copy fires with ZERO gates and retainSource=true", async () => {
    const { onMoved } = setup({
      op: "copy",
      source: teamAll,
      target: personal,
    });
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(moveSpy.mock.calls[0][0].retainSource).toBe(true);
    expect(onMoved).toHaveBeenCalled();
    expect(toastSuccess).toHaveBeenCalledWith("Copied 'note.md' to Private");
  });

  it("Personal->Team copy SKIPS the sole-copy gate entirely (source retained)", async () => {
    setup({ op: "copy", source: personal, target: teamAll });
    // Privacy is the ONLY gate — a MOVE here would insert sole-copy next.
    expect(screen.getByTestId("move-sheet-header").textContent).not.toContain("Step");
    expect(screen.getByTestId("move-gate-privacy")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-gate-privacy-confirm"));
    expect(screen.queryByTestId("move-gate-sole-copy")).toBeNull();
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(moveSpy.mock.calls[0][0].retainSource).toBe(true);
  });

  it("Personal->Team copy fires the privacy 'Share it' step WITHOUT the takes-it-out clause", async () => {
    setup({ op: "copy", source: personal, target: teamAll });
    const gate = screen.getByTestId("move-gate-privacy");
    expect(gate.textContent).toContain("visible to everyone on All of default");
    expect(gate.textContent).not.toContain("takes it out of Private");
    fireEvent.click(screen.getByTestId("move-gate-privacy-confirm"));
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(toastSuccess).toHaveBeenCalledWith("Copied 'note.md' to All of default");
  });

  it("collision Keep-both surfaces the renamed copy in the toast", async () => {
    setup({
      op: "copy",
      source: teamAll,
      target: personal,
      targetCollision: true,
      targetHas: (c) => c === "note.md",
    });
    expect(screen.getByTestId("move-gate-collision")).toBeTruthy();
    fireEvent.click(screen.getByTestId("move-gate-collision-keep-both"));
    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(1));
    expect(moveSpy.mock.calls[0][0].toLogicalName).toBe("note-2.md");
    expect(moveSpy.mock.calls[0][0].retainSource).toBe(true);
    expect(toastSuccess).toHaveBeenCalledWith("Copied to Private as 'note-2.md'");
  });
});

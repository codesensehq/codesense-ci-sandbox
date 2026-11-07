// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import type { StatusState } from "@/tauri/commands";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const mockSaveTeamConfig = vi.fn();
vi.mock("@/tauri/commands", async (importActual) => ({
  ...(await importActual<typeof import("@/tauri/commands")>()),
  saveTeamConfig: (...args: unknown[]) => mockSaveTeamConfig(...args),
}));

const { toast } = await import("sonner");
const { PhasesPanel } = await import("./phases-panel");

const states: StatusState[] = [
  { id: "todo", label: "Todo", color: "#94a3b8", icon: "CircleDot", order: 0, role: "not_started" },
  { id: "doing", label: "Doing", color: "#0ea5e9", icon: "Circle", order: 1, role: "active" },
  { id: "done", label: "Done", color: "#10b981", icon: "CheckCircle2", order: 2, role: "done" },
];

beforeEach(() => {
  mockSaveTeamConfig.mockReset().mockResolvedValue(undefined);
  vi.mocked(toast.success).mockClear();
  act(() => useStatusTaxonomyStore.getState().setStates(states));
});

function renderPanel() {
  return render(<PhasesPanel orgRoot="/root" team="backend" teamKey="acme:backend" />);
}

describe("PhasesPanel (SW-GT-6 · AC-1)", () => {
  it("lists states ordered by order with a read-only role chip", () => {
    renderPanel();
    expect(screen.getByTestId("settings-phase-row-todo")).toBeInTheDocument();
    // Role chip is present and shows the derived role, but there is no role
    // <select> — role is informational only.
    expect(screen.getByTestId("settings-phase-role-doing")).toHaveTextContent("Active");
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("shows the dropped toggle ONLY on terminal (done-role) states", () => {
    renderPanel();
    expect(screen.getByTestId("settings-phase-dropped-done")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-phase-dropped-todo")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-phase-dropped-doing")).not.toBeInTheDocument();
  });

  it("renaming + saving persists via saveTeamConfig with index-normalized order", async () => {
    renderPanel();
    fireEvent.change(screen.getByTestId("settings-phase-label-doing"), {
      target: { value: "Building" },
    });
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-phases-save"));
    });
    expect(mockSaveTeamConfig).toHaveBeenCalledWith("/root", "backend", {
      kind: "status_taxonomy",
      states: expect.arrayContaining([
        expect.objectContaining({ id: "doing", label: "Building", order: 1 }),
      ]),
    });
    expect(toast.success).toHaveBeenCalledWith("Saved phases");
  });

  it("toggling dropped writes delivered:false on the terminal state", async () => {
    renderPanel();
    fireEvent.click(screen.getByTestId("settings-phase-dropped-input-done"));
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-phases-save"));
    });
    const [, , write] = mockSaveTeamConfig.mock.calls[0];
    const doneState = (write.states as StatusState[]).find((s) => s.id === "done");
    expect(doneState?.delivered).toBe(false);
  });

  it("reorder via down button moves a state and normalizes order on save", async () => {
    renderPanel();
    fireEvent.click(screen.getByTestId("settings-phase-down-todo"));
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-phases-save"));
    });
    const [, , write] = mockSaveTeamConfig.mock.calls[0];
    const ids = (write.states as StatusState[]).map((s) => s.id);
    // todo moved after doing.
    expect(ids).toEqual(["doing", "todo", "done"]);
    expect((write.states as StatusState[]).map((s) => s.order)).toEqual([0, 1, 2]);
  });
});

// SPDX-License-Identifier: MIT
import { publishPolicyFixture } from "@/test/prefs-policy-fixture";
import { applyFieldPatch } from "@/shared/lib/field-patch";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({ raw: "{}" }));
vi.mock("@/tauri/commands", () => ({
  loadAppState: async () => state.raw,
  saveAppState: async (raw: string) => {
    state.raw = JSON.stringify(applyFieldPatch(JSON.parse(state.raw), JSON.parse(raw)));
    publishPolicyFixture(state.raw);
  },
}));
import { DashboardsSection } from "./dashboards-section";
afterEach(cleanup);
beforeEach(() => {
  state.raw = JSON.stringify({
    viewer: { preserve: true },
    dashboardPolicy: { preset: "open", perDashboard: { Board: { stopSession: "prompt-once" } } },
  });
  publishPolicyFixture(state.raw);
});
it("defaults open, documents both switches, and preserves per-dashboard overrides", async () => {
  render(<DashboardsSection />);
  const preset = screen.getByRole("combobox");
  await waitFor(() => expect(preset).not.toBeDisabled());
  expect(preset).toHaveValue("open");
  expect(screen.getByRole("checkbox", { name: "Confirm message actions" })).not.toBeChecked();
  fireEvent.change(preset, { target: { value: "team-safe" } });
  await waitFor(() => expect(JSON.parse(state.raw).dashboardPolicy.preset).toBe("team-safe"));
  expect(screen.getByRole("checkbox", { name: "Confirm team sends as you" })).toBeChecked();
  expect(JSON.parse(state.raw).dashboardPolicy.perDashboard.Board.stopSession).toBe("prompt-once");
  expect(JSON.parse(state.raw).viewer).toEqual({ preserve: true });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  fireEvent.change(preset, { target: { value: "open" } });
  await waitFor(() =>
    expect(screen.getByRole("checkbox", { name: "Confirm message actions" })).not.toBeChecked(),
  );
  fireEvent.click(screen.getByRole("checkbox", { name: "Confirm message actions" }));
  await waitFor(() =>
    expect(JSON.parse(state.raw).dashboardPolicy.switches.confirmMessageDestructive).toBe(true),
  );
  expect(JSON.parse(state.raw).dashboardPolicy.switches.confirmTeamSendAsYou).toBe(false);
});

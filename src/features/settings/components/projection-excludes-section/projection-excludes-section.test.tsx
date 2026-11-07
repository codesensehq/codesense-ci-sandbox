// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

// Mock the two IPC commands the section + its load hook use, so the test hits
// spies, not the bridge.
vi.mock("@/tauri/commands", () => ({
  getProjectionExcludes: vi.fn(),
  setProjectionExcludes: vi.fn(),
}));
// Controllable team target so a test can render the editor or the empty state.
vi.mock("@/features/settings/hooks/use-active-team-config-target", () => ({
  useActiveTeamConfigTarget: vi.fn(),
}));
// Active org so the section body renders the editor.
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: (sel: (s: { activeOrgSlug: string | null; stateLoaded: boolean }) => unknown) =>
    sel({ activeOrgSlug: "org", stateLoaded: true }),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const commands = await import("@/tauri/commands");
const getExcludes = vi.mocked(commands.getProjectionExcludes);
const setExcludes = vi.mocked(commands.setProjectionExcludes);
const targetMod = await import("@/features/settings/hooks/use-active-team-config-target");
const useTarget = vi.mocked(targetMod.useActiveTeamConfigTarget);

import {
  ProjectionExcludesSection,
  isDefaultExcluded,
  toggleDefault,
  customPatterns,
  addCustom,
  removeCustom,
} from "./projection-excludes-section";

describe("projection-excludes pure model", () => {
  it("treats a default as excluded unless negated, and toggles the negation", () => {
    expect(isDefaultExcluded([], "**/.env")).toBe(true);
    expect(isDefaultExcluded(["!**/.env"], "**/.env")).toBe(false);
    expect(toggleDefault([], "**/.env")).toEqual(["!**/.env"]);
    expect(toggleDefault(["!**/.env"], "**/.env")).toEqual([]);
  });

  it("splits custom adds from negations and add/remove are lossless", () => {
    const p = ["!**/.env", "**/*.tfstate"];
    expect(customPatterns(p)).toEqual(["**/*.tfstate"]);
    expect(addCustom(p, "**/*.log")).toEqual(["!**/.env", "**/*.tfstate", "**/*.log"]);
    expect(addCustom(p, "**/*.tfstate")).toEqual(p); // idempotent
    expect(removeCustom(p, "**/*.tfstate")).toEqual(["!**/.env"]);
  });
});

describe("ProjectionExcludesSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useTarget.mockReturnValue({ orgRoot: "/org", team: "alpha", teamKey: "org:alpha" });
    // Defaults come from the backend read — the FE must NOT hardcode them.
    getExcludes.mockResolvedValue({
      defaults: ["**/.env", "**/id_rsa"],
      patterns: [],
    });
    setExcludes.mockResolvedValue(undefined);
  });

  it("renders defaults from backend data (no FE literal)", async () => {
    render(<ProjectionExcludesSection query="" />);
    // A default toggle rendered from the backend-provided list, pre-checked.
    const envToggle = await screen.findByTestId("projection-excludes-default-**/.env");
    expect(envToggle).toBeChecked();
    expect(screen.getByTestId("projection-excludes-default-**/id_rsa")).toBeChecked();
    expect(getExcludes).toHaveBeenCalledWith("/org", "alpha");
  });

  it("says excludes match any case and a negation only its exact spelling (SW-XS-12)", async () => {
    render(<ProjectionExcludesSection query="" />);
    const hint = await screen.findByTestId("projection-excludes-case-hint");
    expect(hint).toHaveTextContent(/any letter case/);
    expect(hint).toHaveTextContent(/only the exact spelling/);
  });

  it("toggling a default + adding a custom glob writes the expected payload", async () => {
    render(<ProjectionExcludesSection query="" />);
    const envToggle = await screen.findByTestId("projection-excludes-default-**/.env");

    // Uncheck `.env` (negate the default → allow it).
    fireEvent.click(envToggle);
    expect(envToggle).not.toBeChecked();

    // Add a custom exclude.
    fireEvent.change(screen.getByTestId("projection-excludes-custom-input"), {
      target: { value: "**/*.tfstate" },
    });
    fireEvent.click(screen.getByTestId("projection-excludes-custom-add"));

    // Save invokes the team-config write with the layered payload.
    fireEvent.click(screen.getByTestId("projection-excludes-save"));
    await waitFor(() =>
      expect(setExcludes).toHaveBeenCalledWith("/org", "alpha", ["!**/.env", "**/*.tfstate"]),
    );
  });

  it("shows the pick-a-team empty state when no team is active", async () => {
    useTarget.mockReturnValue({ orgRoot: "", team: null, teamKey: "" });
    render(<ProjectionExcludesSection query="" />);
    expect(screen.getByTestId("settings-projection-excludes-no-team")).toBeInTheDocument();
    expect(getExcludes).not.toHaveBeenCalled();
  });
});

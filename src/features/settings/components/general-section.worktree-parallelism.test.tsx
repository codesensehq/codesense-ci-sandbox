// SPDX-License-Identifier: MIT
/**
 * RF-perf-14d → RF-perf-23: the "Worktrees set up in parallel" settings row — a
 * segmented radio (Auto | 1 | 2 | 3 | 4 | 6 | 8 | 12 | Off | Custom…), default
 * Auto, writing `updateSettings({ worktreeSetupParallelism })`. RF-perf-23 adds
 * the raised presets, the "Off" (unbounded) option, and the "Custom…" reveal.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  readMachineCores,
  resolveBackgroundSetupCap,
} from "@/shared/lib/worktree/background-setup-fanout";

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

import { GeneralSection, GENERAL_ROW_SPEC_LIST } from "./general-section";

describe("GeneralSection — worktree parallelism row (RF-perf-14d)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, worktreeSetupParallelism: "auto" },
    });
  });

  it("reflects the setting (default Auto) and writes the chosen value", () => {
    render(<GeneralSection query="worktree" />);
    const group = screen.getByTestId("settings-worktree-parallelism");
    expect(group).toBeInTheDocument();

    expect(screen.getByTestId("settings-worktree-parallelism-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );

    // Pick an explicit override → writes a number.
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-2"));
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(2);

    // Back to Auto → writes the "auto" sentinel (not a number).
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-auto"));
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe("auto");
  });

  it("shows the resolved Auto cap in the option label", () => {
    render(<GeneralSection query="worktree" />);
    // Auto button label surfaces what Auto picked on this machine, e.g. "Auto (3)".
    expect(screen.getByTestId("settings-worktree-parallelism-auto").textContent).toMatch(
      /Auto \(\d+\)/,
    );
  });

  it("the Auto (N) hint number EQUALS the applied cap — same resolver (RF-perf-23 AC-4)", () => {
    render(<GeneralSection query="worktree" />);
    // The hint and the live gate both read resolveBackgroundSetupCap("auto", cores),
    // so the displayed N must equal what the gate would apply — they can't disagree.
    const applied = resolveBackgroundSetupCap("auto", readMachineCores());
    expect(screen.getByTestId("settings-worktree-parallelism-auto").textContent).toBe(
      `Auto (${applied})`,
    );
  });

  it("renders the state-specific footer copy for default / Off / Custom (RF-perf-23 AC-6)", () => {
    render(<GeneralSection query="worktree" />);

    // Default: explains the light-clone + Off semantics.
    expect(screen.getByText(/Setup is now a light copy/)).toBeInTheDocument();
    expect(screen.getByText(/Off = no limit — all set up at once/)).toBeInTheDocument();

    // Off: names the tradeoff.
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-off"));
    expect(
      screen.getByText(/Off — no limit\. Every queued worktree sets up immediately/),
    ).toBeInTheDocument();

    // Custom: the reveal helper copy.
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-custom"));
    expect(screen.getByText(/Blank or invalid keeps your last value/)).toBeInTheDocument();
  });

  it("shows Auto selected when the setting is unset (default)", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, worktreeSetupParallelism: undefined },
    });
    render(<GeneralSection query="worktree" />);
    expect(screen.getByTestId("settings-worktree-parallelism-auto")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("reflects an explicit numeric override", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, worktreeSetupParallelism: 3 },
    });
    render(<GeneralSection query="worktree" />);
    expect(screen.getByTestId("settings-worktree-parallelism-3")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("offers the raised presets and writes them (RF-perf-23 AC-2)", () => {
    render(<GeneralSection query="worktree" />);
    for (const [testId, expected] of [
      ["settings-worktree-parallelism-6", 6],
      ["settings-worktree-parallelism-8", 8],
      ["settings-worktree-parallelism-12", 12],
    ] as const) {
      fireEvent.click(screen.getByTestId(testId));
      expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(expected);
    }
  });

  it('writes the "off" sentinel when Off is chosen (RF-perf-23 AC-1)', () => {
    render(<GeneralSection query="worktree" />);
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-off"));
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe("off");
    expect(screen.getByTestId("settings-worktree-parallelism-off")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("Custom… reveals a numeric input and commits a floored, ≥1 value (RF-perf-23 AC-3)", () => {
    render(<GeneralSection query="worktree" />);
    // The reveal is hidden until Custom… is selected.
    expect(screen.queryByTestId("settings-worktree-parallelism-custom-input")).toBeNull();

    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-custom"));
    const input = screen.getByTestId("settings-worktree-parallelism-custom-input");
    expect(input).toBeInTheDocument();

    // Enter 10 → applies cap 10.
    fireEvent.change(input, { target: { value: "10" } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(10);
    // Custom… stays selected (a11y: always exactly one selection).
    expect(screen.getByTestId("settings-worktree-parallelism-custom")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("Custom… coerces 0 / negative to 1, floors fractionals, clamps absurd to MAX (RF-perf-23 AC-3)", () => {
    render(<GeneralSection query="worktree" />);
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-custom"));
    const input = screen.getByTestId("settings-worktree-parallelism-custom-input");

    fireEvent.change(input, { target: { value: "0" } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(1);

    fireEvent.change(input, { target: { value: "7.9" } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(7);

    // An absurd/pasted value clamps to the sane ceiling (Off is the unlimited path).
    fireEvent.change(input, { target: { value: "5000" } });
    fireEvent.blur(input);
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(999);
  });

  it("selecting Custom… preserves the CURRENT committed cap, not a stale seed (RF-perf-23)", () => {
    // Regression: mount at Auto, pick preset 12, then open Custom… — the reveal
    // must seed from the live value (12), never silently knock the cap back.
    render(<GeneralSection query="worktree" />);
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-12"));
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(12);

    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-custom"));
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(12);
    expect(screen.getByTestId("settings-worktree-parallelism-custom-input")).toHaveValue(12);
  });

  it("Custom… keeps the last valid value when blank/garbage is entered (RF-perf-23 AC-3)", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, worktreeSetupParallelism: 10 },
    });
    render(<GeneralSection query="worktree" />);
    // Setting is a non-preset number → Custom… is auto-selected, input shows 10.
    const input = screen.getByTestId("settings-worktree-parallelism-custom-input");
    expect(input).toHaveValue(10);

    fireEvent.change(input, { target: { value: "" } });
    fireEvent.blur(input);
    // Stays at the last valid value — never empties, never silently reverts to Auto.
    expect(useViewerStore.getState().settings.worktreeSetupParallelism).toBe(10);
    expect(screen.getByTestId("settings-worktree-parallelism-custom")).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("a persisted non-preset number selects Custom… on render (RF-perf-23)", () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, worktreeSetupParallelism: 999 },
    });
    render(<GeneralSection query="worktree" />);
    expect(screen.getByTestId("settings-worktree-parallelism-custom")).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByTestId("settings-worktree-parallelism-custom-input")).toHaveValue(999);
  });

  it("exactly one option is selected across Auto / preset / Off / Custom (a11y invariant)", () => {
    render(<GeneralSection query="worktree" />);
    const group = screen.getByTestId("settings-worktree-parallelism");
    const checkedCount = () =>
      within(group)
        .getAllByRole("radio")
        .filter((b) => b.getAttribute("aria-checked") === "true").length;
    expect(checkedCount()).toBe(1); // Auto (default)
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-8"));
    expect(checkedCount()).toBe(1);
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-off"));
    expect(checkedCount()).toBe(1);
    fireEvent.click(screen.getByTestId("settings-worktree-parallelism-custom"));
    expect(checkedCount()).toBe(1);
  });

  it("is discoverable via its keywords (search wiring)", () => {
    const spec = GENERAL_ROW_SPEC_LIST.find((s) => s.key === "worktreeSetupParallelism");
    expect(spec).toBeDefined();
    expect(spec?.keywords).toContain("parallel");
    render(<GeneralSection query="parallel" />);
    expect(screen.getByTestId("settings-worktree-parallelism")).toBeInTheDocument();
  });
});

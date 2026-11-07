// SPDX-License-Identifier: MIT
/**
 * SW-XS-17 (AC-3): Settings has no worktree-cap or idle-threshold controls. The
 * opt-in "remove merged worktrees" row is unaffected.
 */
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

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

describe("GeneralSection — no worktree cap (SW-XS-17 AC-3)", () => {
  it("lists no worktree-limit or idle-threshold row", () => {
    const keys = GENERAL_ROW_SPEC_LIST.map((s) => s.key);
    expect(keys).not.toContain("maxWorktrees");
    expect(keys).not.toContain("idleThreshold");
    const labels = GENERAL_ROW_SPEC_LIST.map((s) => s.label);
    expect(labels).not.toContain("Worktree limit");
    expect(labels).not.toContain("Reap idle after");
  });

  it.each(["", "worktree", "limit", "idle", "reap"])(
    "renders no cap or idle-threshold control for query %j",
    (query) => {
      render(<GeneralSection query={query} />);
      expect(screen.queryByTestId("settings-max-worktrees")).toBeNull();
      expect(screen.queryByTestId("settings-idle-threshold")).toBeNull();
      expect(screen.queryByText("Worktree limit")).toBeNull();
      expect(screen.queryByText("Reap idle after")).toBeNull();
    },
  );
});

// SPDX-License-Identifier: MIT
/**
 * SW-XS-3 / SW-XS-4: there is no sync cadence setting at all — no Down (hydrate)
 * cadence, no hydration mode, and no Up (publish) cadence. Context never flows into
 * a workspace on its own, and edits are published only by an explicit operation.
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

import { GeneralSection } from "./general-section";

describe("GeneralSection — no sync cadence (SW-XS-3, SW-XS-4)", () => {
  it.each(["sync", "reverse", "publish", ""])(
    "offers neither an Up nor a Down cadence (query %j)",
    (query) => {
      render(<GeneralSection query={query} />);
      expect(screen.queryByTestId("settings-sync-cadence")).toBeNull();
      expect(screen.queryByTestId("settings-reverse-sync-default-on-demand")).toBeNull();
      expect(screen.queryByTestId("settings-forward-sync-default-on-demand")).toBeNull();
    },
  );

  it("offers no hydration mode setting", () => {
    render(<GeneralSection query="hydration" />);
    expect(screen.queryByTestId("settings-hydration-mode")).toBeNull();
  });
});

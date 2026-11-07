// SPDX-License-Identifier: MIT
import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const openRoutines = vi.fn();
vi.mock("../routines-virtual-tab", () => ({
  openRoutines: (...args: unknown[]) => openRoutines(...args),
}));
const closeTab = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { getState: () => ({ closeTab }) },
}));
vi.mock("@/shared/lib/team-key", () => ({ getActiveTeamKey: () => "acme:default" }));

import { RoutinesRedirect } from "./routines-redirect";

describe("RoutinesRedirect", () => {
  beforeEach(() => vi.clearAllMocks());

  it("swaps the retired tab for Agents → Schedule without leaving full-screen CLI", () => {
    render(<RoutinesRedirect filePath="virtual://routines?agent=acme/default/barb" />);
    expect(openRoutines).toHaveBeenCalledWith(
      { agentKey: "acme/default/barb", name: "barb", routineKeys: [] },
      { keepLayout: true },
    );
    expect(closeTab).toHaveBeenCalledWith(
      "acme:default",
      "",
      "virtual://routines?agent=acme/default/barb",
    );
  });

  it("with no agent filter, still keeps full-screen CLI", () => {
    render(<RoutinesRedirect filePath="virtual://routines" />);
    expect(openRoutines).toHaveBeenCalledWith(null, { keepLayout: true });
    expect(closeTab).toHaveBeenCalledWith("acme:default", "", "virtual://routines");
  });
});

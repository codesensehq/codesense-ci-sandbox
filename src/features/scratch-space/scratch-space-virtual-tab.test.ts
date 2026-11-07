// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  registerVirtualTab: vi.fn(),
  openManagementTab: vi.fn(),
  closeSettings: vi.fn(),
}));
vi.mock("@/shared/lib/virtual-viewer-registry", () => ({
  registerVirtualTab: h.registerVirtualTab,
}));
vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: h.openManagementTab }));
vi.mock("@/shared/stores/ui-store", () => ({
  useUiStore: { getState: () => ({ closeSettings: h.closeSettings }) },
}));
vi.mock("./components/scratch-space-tab", () => ({ ScratchSpaceTab: () => null }));

import { openScratchSpace, SCRATCH_SPACE_VIRTUAL_PATH } from "./scratch-space-virtual-tab";

beforeEach(() => vi.clearAllMocks());

describe("openScratchSpace", () => {
  it("registers the tab once and opens it at virtual://scratch", () => {
    openScratchSpace();
    openScratchSpace();
    expect(h.registerVirtualTab).toHaveBeenCalledTimes(1);
    expect(h.registerVirtualTab).toHaveBeenCalledWith(
      SCRATCH_SPACE_VIRTUAL_PATH,
      "Private",
      expect.any(Function),
    );
    expect(SCRATCH_SPACE_VIRTUAL_PATH).toBe("virtual://scratch");
    expect(h.openManagementTab).toHaveBeenCalledWith("virtual://scratch");
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";

const openFolderDialog = vi.fn();
const openOrFocusGenericWorkspace = vi.fn();

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openFolderDialog(...args),
}));
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { getState: () => ({ openOrFocusGenericWorkspace }) },
}));
vi.mock("@/shared/lib/team-key", () => ({
  getActiveTeamKey: () => "org:team",
}));

import { openGenericWorkspace } from "./open-generic-workspace";

describe("openGenericWorkspace (SW-GW-1 'Open workspace here')", () => {
  beforeEach(() => {
    openFolderDialog.mockReset();
    openOrFocusGenericWorkspace.mockReset();
  });

  it("opens the native folder picker in directory mode", async () => {
    openFolderDialog.mockResolvedValue("/Users/me/scratch");
    await openGenericWorkspace();
    expect(openFolderDialog).toHaveBeenCalledWith(expect.objectContaining({ directory: true }));
  });

  it("opens a generic workspace at the picked dir on the active team (AC-2)", async () => {
    openFolderDialog.mockResolvedValue("/Users/me/scratch");
    await openGenericWorkspace();
    expect(openOrFocusGenericWorkspace).toHaveBeenCalledWith("org:team", "/Users/me/scratch");
  });

  it("is a quiet no-op when the picker is cancelled (null)", async () => {
    openFolderDialog.mockResolvedValue(null);
    await openGenericWorkspace();
    expect(openOrFocusGenericWorkspace).not.toHaveBeenCalled();
  });

  it("ignores a multi-select array result (only a single path string opens)", async () => {
    openFolderDialog.mockResolvedValue(["/a", "/b"]);
    await openGenericWorkspace();
    expect(openOrFocusGenericWorkspace).not.toHaveBeenCalled();
  });
});

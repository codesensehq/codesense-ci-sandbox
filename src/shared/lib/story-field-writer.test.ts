// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/tauri/commands", () => ({
  applyMetadataEdit: vi.fn(() => Promise.resolve()),
}));

import { applyMetadataEdit } from "@/tauri/commands";
import { writeStoryPoints, writeStorySprint, clearStorySprint } from "./story-field-writer";

const applyMetadataEditMock = vi.mocked(applyMetadataEdit);

describe("writeStoryPoints", () => {
  beforeEach(() => applyMetadataEditMock.mockClear());

  it("routes through the shipped apply_metadata_edit SetField path (AC-8)", async () => {
    await writeStoryPoints("/repo/s.md", 5);
    expect(applyMetadataEditMock).toHaveBeenCalledTimes(1);
    expect(applyMetadataEditMock).toHaveBeenCalledWith("/repo/s.md", [
      { kind: "setField", field: "points", value: "5" },
    ]);
  });

  it("stringifies the numeric estimate (frontmatter scalars are strings)", async () => {
    await writeStoryPoints("/repo/s.md", 13);
    expect(applyMetadataEditMock).toHaveBeenCalledWith("/repo/s.md", [
      { kind: "setField", field: "points", value: "13" },
    ]);
  });

  it("propagates a write rejection to the caller (optimistic revert path)", async () => {
    applyMetadataEditMock.mockRejectedValueOnce(new Error("disk full"));
    await expect(writeStoryPoints("/repo/s.md", 3)).rejects.toThrow("disk full");
  });
});

describe("writeStorySprint / clearStorySprint (SW-SB-15)", () => {
  beforeEach(() => applyMetadataEditMock.mockClear());

  it("writeStorySprint routes a SetField sprint (no numeric quoting — sprint is a string)", async () => {
    await writeStorySprint("/repo/s.md", "2026-08-A");
    expect(applyMetadataEditMock).toHaveBeenCalledWith("/repo/s.md", [
      { kind: "setField", field: "sprint", value: "2026-08-A" },
    ]);
  });

  it("clearStorySprint routes a ClearField sprint", async () => {
    await clearStorySprint("/repo/s.md");
    expect(applyMetadataEditMock).toHaveBeenCalledWith("/repo/s.md", [
      { kind: "clearField", field: "sprint" },
    ]);
  });

  it("propagates a write rejection to the caller", async () => {
    applyMetadataEditMock.mockRejectedValueOnce(new Error("read-only"));
    await expect(writeStorySprint("/repo/s.md", "2026-08-A")).rejects.toThrow("read-only");
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { libraryEnvelope, packBlob } from "./canvas-library-merge";

describe("libraryEnvelope (SW-SC-8 D3)", () => {
  it("serializes the whole bag as a v2 envelope, keeping every item", () => {
    const items = [{ id: "a" }, { id: "b" }, { id: "c" }];
    const parsed = JSON.parse(libraryEnvelope(items));
    expect(parsed).toMatchObject({ type: "excalidrawlib", version: 2 });
    expect(parsed.libraryItems.map((i: { id: string }) => i.id)).toEqual(["a", "b", "c"]);
  });

  it("serializes an empty panel to an empty bag, never null/delete", () => {
    // Whole-bag persist: deleting the last item must write `[]`, not signal a
    // file delete — otherwise the seed marker is orphaned and delete-stickiness
    // breaks.
    const parsed = JSON.parse(libraryEnvelope([]));
    expect(parsed).toMatchObject({ type: "excalidrawlib", version: 2, libraryItems: [] });
  });
});

describe("packBlob", () => {
  it("wraps raw content as an application/json Blob for Excalidraw's loader", () => {
    const blob = packBlob('{"libraryItems":[]}');
    expect(blob).toBeInstanceOf(Blob);
    expect(blob.type).toBe("application/json");
  });
});

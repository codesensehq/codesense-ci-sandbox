// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { retargetDraft } from "./file-buffer-model";

// The disk rules moved into the buffer state machine (buffer-machine.test.ts
// model-checks them). What stays here is the re-target handoff.

describe("retargetDraft", () => {
  it("a re-target carries only unsaved edits", () => {
    expect(retargetDraft(true, "mine", "base")).toEqual({ buffer: "mine", lastSaved: "base" });
    expect(retargetDraft(true, "same", "same")).toBeNull();
    expect(retargetDraft(false, "mine", "base")).toBeNull();
  });
});

// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import {
  validateListChipValue,
  isListField,
  isFieldPresent,
  fieldScalarValue,
  fieldListValue,
  EPIC_ONLY_FIELDS,
} from "./metadata-schema";
import type { MetadataFields } from "@/tauri/commands";

describe("validateListChipValue (AC-3 comma parity)", () => {
  it("rejects a value containing a comma", () => {
    expect(validateListChipValue("a,b")).toMatch(/comma/i);
    expect(validateListChipValue("owner/repo, other")).toMatch(/comma/i);
  });
  it("rejects an empty value", () => {
    expect(validateListChipValue("   ")).toMatch(/empty/i);
  });
  it("accepts a normal value", () => {
    expect(validateListChipValue("SLW-RI-1")).toBeNull();
    expect(validateListChipValue("./spike-notes.md")).toBeNull();
  });
});

describe("schema helpers", () => {
  it("classifies list vs scalar fields", () => {
    expect(isListField("depends_on")).toBe(true);
    expect(isListField("scope_install")).toBe(true);
    expect(isListField("type")).toBe(false);
  });

  it("marks epic-only fields", () => {
    expect(EPIC_ONLY_FIELDS.has("repos")).toBe(true);
    expect(EPIC_ONLY_FIELDS.has("type")).toBe(false);
    // SW-OR-1b: `branch` is the unified story branch (and epic integration
    // branch) — offered on both doc kinds, so it is NOT epic-only.
    expect(EPIC_ONLY_FIELDS.has("branch")).toBe(false);
  });

  it("reads scalar + list values off the wire model", () => {
    const fields: MetadataFields = {
      type: "story",
      depends_on: ["a", "b"],
    };
    expect(fieldScalarValue(fields, "type")).toBe("story");
    expect(fieldScalarValue(fields, "status")).toBeNull();
    expect(fieldListValue(fields, "depends_on")).toEqual(["a", "b"]);
    expect(fieldListValue(fields, "supporting_docs")).toEqual([]);
  });

  it("detects field presence (empty list = absent)", () => {
    expect(isFieldPresent({ type: "story" }, "type")).toBe(true);
    expect(isFieldPresent({ depends_on: ["x"] }, "depends_on")).toBe(true);
    expect(isFieldPresent({ depends_on: [] }, "depends_on")).toBe(false);
    expect(isFieldPresent({}, "status")).toBe(false);
  });
});

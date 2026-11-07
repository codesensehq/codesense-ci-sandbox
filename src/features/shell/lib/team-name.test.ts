// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { teamNameError } from "./team-name";

describe("teamNameError", () => {
  it("returns null for an empty name (no error shown, submit stays disabled)", () => {
    expect(teamNameError("", [])).toBeNull();
    expect(teamNameError("   ", [])).toBeNull();
  });

  it("rejects uppercase / spaces / leading hyphen", () => {
    expect(teamNameError("Platform", [])).toMatch(/lowercase/);
    expect(teamNameError("has space", [])).toMatch(/lowercase/);
    expect(teamNameError("-lead", [])).toMatch(/lowercase/);
  });

  it("rejects the reserved -meta suffix", () => {
    expect(teamNameError("payments-meta", [])).toMatch(/meta/);
  });

  it("rejects a duplicate name", () => {
    expect(teamNameError("platform", ["platform"])).toMatch(/already exists/);
  });

  it("rejects an over-long name", () => {
    expect(teamNameError("a".repeat(41), [])).toMatch(/40 characters or fewer/);
  });

  it("accepts a valid new slug", () => {
    expect(teamNameError("payments", ["platform"])).toBeNull();
    expect(teamNameError("team-2", ["platform"])).toBeNull();
  });
});

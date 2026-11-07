// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { createCollection, mintId } from "./mock-store";

describe("mintId", () => {
  it("derives a stable slug from a name so an edit-flow can re-find a just-created record", () => {
    expect(mintId("My First Automation")).toBe("my-first-automation");
    expect(mintId("My First Automation")).toBe(mintId("My First Automation"));
  });

  it("disambiguates a colliding slug with a numeric suffix", () => {
    const taken = new Set(["release-bot"]);
    expect(mintId("Release Bot", taken)).toBe("release-bot-2");
    taken.add("release-bot-2");
    expect(mintId("Release Bot", taken)).toBe("release-bot-3");
  });

  it("falls back to a non-empty id when the name has no slug characters", () => {
    expect(mintId("!!!")).not.toBe("");
  });
});

describe("createCollection", () => {
  it("creates and lists records", () => {
    const c = createCollection<{ id: string; name: string }>();
    c.put({ id: "a", name: "Alpha" });
    c.put({ id: "b", name: "Beta" });
    expect(c.list().map((r) => r.id)).toEqual(["a", "b"]);
  });

  it("gets a record by id and returns null for a miss", () => {
    const c = createCollection<{ id: string; name: string }>();
    c.put({ id: "a", name: "Alpha" });
    expect(c.get("a")).toEqual({ id: "a", name: "Alpha" });
    expect(c.get("missing")).toBeNull();
  });

  it("put overwrites an existing record in place (edit), preserving order", () => {
    const c = createCollection<{ id: string; name: string }>();
    c.put({ id: "a", name: "Alpha" });
    c.put({ id: "b", name: "Beta" });
    c.put({ id: "a", name: "Alpha edited" });
    expect(c.list()).toEqual([
      { id: "a", name: "Alpha edited" },
      { id: "b", name: "Beta" },
    ]);
  });

  it("removes a record and reports whether anything was removed", () => {
    const c = createCollection<{ id: string; name: string }>();
    c.put({ id: "a", name: "Alpha" });
    expect(c.remove("a")).toBe(true);
    expect(c.remove("a")).toBe(false);
    expect(c.list()).toEqual([]);
  });

  it("seeds initial records", () => {
    const c = createCollection<{ id: string; name: string }>([{ id: "seed", name: "Seed" }]);
    expect(c.get("seed")).toEqual({ id: "seed", name: "Seed" });
  });
});

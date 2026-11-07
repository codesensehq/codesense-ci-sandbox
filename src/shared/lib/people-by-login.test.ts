// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import { buildPeopleByLogin, personByLogin } from "./people-by-login";
import type { Person } from "@/shared/types/domain";

vi.mock("@/shared/lib/logger", () => ({
  logger: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));
import { logger } from "@/shared/lib/logger";

function person(id: number, login: string, name = login): Person {
  return { github_user_id: id, github_username: login, display_name: name, avatar_url: null };
}

const roster: Person[] = [person(1, "alice"), person(2, "Bob"), person(3, "carol")];

describe("buildPeopleByLogin", () => {
  beforeEach(() => vi.clearAllMocks());

  it("keys the index by the lowercased login", () => {
    const index = buildPeopleByLogin(roster);
    expect(index.get("alice")?.github_user_id).toBe(1);
    // Stored login "Bob" is folded to the lowercase key.
    expect(index.get("bob")?.github_user_id).toBe(2);
    expect(index.has("Bob")).toBe(false);
  });

  it("empty roster yields an empty index", () => {
    expect(buildPeopleByLogin([]).size).toBe(0);
  });

  it("first-wins on a duplicate case-folded login, and warns once", () => {
    const index = buildPeopleByLogin([person(1, "octo", "One"), person(2, "OCTO", "Two")]);
    expect(index.get("octo")?.github_user_id).toBe(1);
    expect(index.size).toBe(1);
    expect(logger.warn).toHaveBeenCalledTimes(1);
  });

  it("skips a blank login so it can't shadow real lookups", () => {
    const index = buildPeopleByLogin([person(1, "  ", "blank"), person(2, "real")]);
    expect(index.has("")).toBe(false);
    expect(index.get("real")?.github_user_id).toBe(2);
  });

  it("skips a handle-less person (null login) without throwing", () => {
    // SW-FG-4b (AC-7): a handle-less GitHub teammate serializes github_username as
    // null. buildPeopleByLogin must tolerate that (usernameKey throws on null) and
    // simply leave them out of the login index — present, not correlatable.
    const handleLess: Person = {
      github_user_id: 500,
      github_username: null,
      display_name: "Grace",
      avatar_url: null,
    };
    const index = buildPeopleByLogin([person(1, "alice"), handleLess]);
    expect(index.size).toBe(1);
    expect(index.get("alice")?.github_user_id).toBe(1);
    expect(personByLogin(index, "grace")).toBeNull();
  });
});

describe("personByLogin", () => {
  const index = buildPeopleByLogin(roster);

  it("resolves an exact login", () => {
    expect(personByLogin(index, "alice")?.github_user_id).toBe(1);
  });

  it("normalizes the lookup: @-strip, trim, case-fold", () => {
    expect(personByLogin(index, "@ALICE")?.github_user_id).toBe(1);
    expect(personByLogin(index, "  Bob  ")?.github_user_id).toBe(2);
  });

  it("returns null for an unknown login", () => {
    expect(personByLogin(index, "nobody")).toBeNull();
  });

  it("returns null for a blank / whitespace / @-only lookup", () => {
    // usernameKey("") === "", which buildPeopleByLogin never stores as a key,
    // so a blank lookup can't shadow a real entry (parity with Rust's
    // is_empty guard).
    expect(personByLogin(index, "")).toBeNull();
    expect(personByLogin(index, "   ")).toBeNull();
    expect(personByLogin(index, "@")).toBeNull();
  });

  it("resolves a padded stored login (stored key is normalized too)", () => {
    const padded = buildPeopleByLogin([person(1, "  alice  ", "Alice")]);
    expect(personByLogin(padded, "alice")?.github_user_id).toBe(1);
  });

  it("returns null against an empty index", () => {
    expect(personByLogin(buildPeopleByLogin([]), "alice")).toBeNull();
  });
});

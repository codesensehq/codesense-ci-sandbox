// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  validateGitHubUsername,
  isValidGitHubUsername,
  stripUsernameInput,
  usernameKey,
} from "./github-username";

describe("validateGitHubUsername", () => {
  it("accepts a plain alphanumeric username", () => {
    expect(validateGitHubUsername("octocat")).toEqual({ ok: true, username: "octocat" });
  });

  it("strips a leading @", () => {
    expect(validateGitHubUsername("@octocat")).toEqual({ ok: true, username: "octocat" });
  });

  it("trims whitespace", () => {
    expect(validateGitHubUsername("  octocat  ")).toEqual({ ok: true, username: "octocat" });
  });

  it("rejects empty", () => {
    expect(validateGitHubUsername("")).toEqual({ ok: false, reason: "empty" });
    expect(validateGitHubUsername("   ")).toEqual({ ok: false, reason: "empty" });
    expect(validateGitHubUsername("@")).toEqual({ ok: false, reason: "empty" });
  });

  it("rejects >39 chars", () => {
    expect(validateGitHubUsername("a".repeat(40))).toEqual({ ok: false, reason: "too-long" });
  });

  it("accepts hyphens between alphanumerics", () => {
    expect(validateGitHubUsername("alpha-corp")).toEqual({
      ok: true,
      username: "alpha-corp",
    });
  });

  it("rejects leading/trailing hyphens and double hyphens", () => {
    expect(validateGitHubUsername("-octo")).toMatchObject({ ok: false });
    expect(validateGitHubUsername("octo-")).toMatchObject({ ok: false });
    expect(validateGitHubUsername("octo--cat")).toMatchObject({ ok: false });
  });

  it("rejects underscores and other special chars", () => {
    expect(validateGitHubUsername("octo_cat")).toMatchObject({ ok: false, reason: "invalid" });
    expect(validateGitHubUsername("octo.cat")).toMatchObject({ ok: false, reason: "invalid" });
  });

  it("isValidGitHubUsername is the boolean alias", () => {
    expect(isValidGitHubUsername("octocat")).toBe(true);
    expect(isValidGitHubUsername("octo_cat")).toBe(false);
  });
});

describe("stripUsernameInput", () => {
  it("strips a leading @ and trims, preserving case", () => {
    expect(stripUsernameInput("  @OctoCat ")).toBe("OctoCat");
  });
});

describe("usernameKey", () => {
  it("strips @, trims, and case-folds to a lookup key", () => {
    expect(usernameKey("  @OctoCat ")).toBe("octocat");
  });

  it("collapses mixed-case variants to the same key", () => {
    expect(usernameKey("OCTO")).toBe(usernameKey("octo"));
  });

  it("folds ASCII only, leaving non-ASCII bytes untouched (Rust parity)", () => {
    // Mirrors Rust `eq_ignore_ascii_case`: ASCII A-Z fold, non-ASCII verbatim —
    // so a hand-edited non-ASCII login keys identically on both sides.
    expect(usernameKey("ÀBC")).toBe("Àbc");
  });

  it("trims ASCII whitespace only, leaving exotic blanks verbatim (Rust parity)", () => {
    // Strips the ASCII set (here VT, U+000B) but not U+FEFF (BOM) — matching
    // Rust `normalize_login`, so neither resolver strips what the other keeps.
    expect(usernameKey("\u000balice\u000b")).toBe("alice");
    expect(usernameKey("\ufeffalice")).toBe("\ufeffalice");
  });

  it("strips exactly one leading @, preserving interior spacing", () => {
    // Mirrors Rust `strip_prefix('@')` (one, after trim): a doubled @ keeps the
    // second, and a space between @ and the login is interior, not trimmed.
    expect(usernameKey("@@alice")).toBe("@alice");
    expect(usernameKey("@  alice")).toBe("  alice");
  });

  it("does not fold non-ASCII case, keeping an accented login distinct (Rust parity)", () => {
    // ASCII folds, É stays É — so `CAFÉ` and `café` are different keys, exactly
    // as Rust `eq_ignore_ascii_case` treats them.
    expect(usernameKey("CAFÉ")).toBe("cafÉ");
    expect(usernameKey("CAFÉ")).not.toBe(usernameKey("café"));
  });
});

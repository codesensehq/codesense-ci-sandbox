// SPDX-License-Identifier: MIT
/**
 * GitHub username rules (documented at
 * https://docs.github.com/en/get-started/signing-up-for-github/personal-accounts):
 * - 1-39 characters
 * - Only alphanumerics and single hyphens
 * - Cannot begin or end with a hyphen
 * - Cannot contain consecutive hyphens
 */
export const GITHUB_USERNAME_RE = /^[a-zA-Z0-9](?:[a-zA-Z0-9]|-(?=[a-zA-Z0-9])){0,38}$/;

export type UsernameValidation =
  | { ok: true; username: string }
  | { ok: false; reason: "empty" | "too-long" | "invalid" };

/** ASCII whitespace at either end — space, tab, LF, VT, FF, CR. Deliberately
 *  NOT `String.trim` (Unicode White_Space): pinned to this fixed set so it
 *  matches Rust `normalize_login` codepoint-for-codepoint. `String.trim` and
 *  Rust `str::trim` disagree on exotic blanks (U+0085 NEL, U+FEFF BOM), which
 *  would let the two login resolvers diverge on a hand-edited roster. */
const ASCII_WS_EDGES = /^[ \t\n\v\f\r]+|[ \t\n\v\f\r]+$/g;

/**
 * Strip a leading `@` and surrounding ASCII whitespace — the shared prefix of
 * username validation and login-key normalization. Preserves case (GitHub
 * displays logins case-sensitively even though they resolve case-insensitively).
 */
export function stripUsernameInput(raw: string): string {
  return raw.replace(ASCII_WS_EDGES, "").replace(/^@/, "");
}

/**
 * Case-folded lookup key for a GitHub login. The single key discipline behind
 * SW-ID-1's `people.yml` login index — both the index build and every lookup
 * route through here so a mixed-case `@Login` and a stored `login` collapse to
 * the same key.
 *
 * Folds **ASCII only** (`[A-Z]`), deliberately NOT `.toLowerCase()`: this
 * mirrors the Rust twin `Org::person_by_username` (`eq_ignore_ascii_case`)
 * byte-for-byte, so a hand-edited non-ASCII `github_username` can never key
 * differently across the two resolvers. GitHub logins are ASCII, so for every
 * real login this is identical to `.toLowerCase()`.
 */
export function usernameKey(raw: string): string {
  return stripUsernameInput(raw).replace(/[A-Z]/g, (c) => c.toLowerCase());
}

/**
 * Normalize + validate a user-typed GitHub username. Strips a leading
 * `@`, trims whitespace, and checks the rules above.
 *
 * Returns a discriminated result so callers can surface specific
 * inline messages (empty / too-long / invalid format).
 */
export function validateGitHubUsername(raw: string): UsernameValidation {
  const trimmed = stripUsernameInput(raw);
  if (trimmed === "") return { ok: false, reason: "empty" };
  if (trimmed.length > 39) return { ok: false, reason: "too-long" };
  if (!GITHUB_USERNAME_RE.test(trimmed)) return { ok: false, reason: "invalid" };
  return { ok: true, username: trimmed };
}

/** Convenience boolean — good for guards where the reason doesn't matter. */
export function isValidGitHubUsername(raw: string): boolean {
  return validateGitHubUsername(raw).ok;
}

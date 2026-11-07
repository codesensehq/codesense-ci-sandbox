// SPDX-License-Identifier: MIT
/**
 * The terminal caption's path, the way a good shell prompt (starship, p10k)
 * reads it: `$HOME` said as `~`, then only the last two segments, with an
 * ellipsis standing in for everything above them.
 *
 *   /Users/me                              → ~
 *   /Users/me/code                         → ~/code
 *   /Users/me/code/app                     → ~/code/app
 *   /Users/me/.codesense/…/default/demo    → ~/…/default/demo
 *   /usr/local/bin                         → /…/local/bin
 *   /                                      → /
 *
 * The caption renders `head` and `parent` muted and `leaf` emphasized, so the
 * directory you are in is what the eye lands on.
 */
export interface PromptPath {
  /** Everything before `parent`, ending in its separator: `~/`, `~/…/`,
   *  `/`, `/…/` — or empty when the whole path is `~` or `/`. */
  head: string;
  /** The segment above the current directory, when shown. */
  parent: string | null;
  /** The current directory (`~` or `/` at those roots). */
  leaf: string;
  /** Whether segments were dropped behind the ellipsis. */
  elided: boolean;
  /** The path as given, for the tooltip and Copy path. */
  full: string;
}

/** How many trailing segments the caption keeps (parent + current). */
export const PROMPT_PATH_SEGMENTS = 2;

/** Collapse doubled separators and strip trailing ones, keeping a lone `/`. */
function trimSlashes(path: string): string {
  const trimmed = path.replace(/\/{2,}/g, "/").replace(/\/+$/, "");
  return trimmed === "" && path.startsWith("/") ? "/" : trimmed;
}

/**
 * Abbreviate `path` for the caption. `home` is the home of the account the
 * session runs as — This Mac's for a local tab, the Machine's for a remote
 * one — and is only applied on a segment boundary (`/Users/me` never
 * abbreviates `/Users/meg`).
 */
export function abbreviatePromptPath(
  path: string,
  home?: string | null,
  segments: number = PROMPT_PATH_SEGMENTS,
): PromptPath {
  const full = path;
  const clean = trimSlashes(path.trim());
  const homeClean = home ? trimSlashes(home.trim()) : null;

  let root: "~" | "/" | "";
  let rest: string;
  if (
    homeClean &&
    homeClean !== "/" &&
    (clean === homeClean || clean.startsWith(homeClean + "/"))
  ) {
    root = "~";
    rest = clean.slice(homeClean.length).replace(/^\/+/, "");
  } else if (clean.startsWith("/")) {
    root = "/";
    rest = clean.replace(/^\/+/, "");
  } else {
    root = "";
    rest = clean;
  }

  const parts = rest.split("/").filter(Boolean);
  if (parts.length === 0) {
    return { head: "", parent: null, leaf: root || clean || full, elided: false, full };
  }

  const keep = Math.max(1, segments);
  const elided = parts.length > keep;
  const shown = elided ? parts.slice(-keep) : parts;
  const leaf = shown[shown.length - 1];
  const parentParts = shown.slice(0, -1);
  const parent = parentParts.length > 0 ? parentParts.join("/") : null;

  let head = root === "~" ? "~/" : root === "/" ? "/" : "";
  if (elided) head += "…/";
  return { head, parent, leaf, elided, full };
}

/** The abbreviated path as one string (`~/…/default/demo-child`). */
export function formatPromptPath(path: PromptPath): string {
  return `${path.head}${path.parent ? `${path.parent}/` : ""}${path.leaf}`;
}

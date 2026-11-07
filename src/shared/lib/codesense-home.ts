// SPDX-License-Identifier: MIT
import { homeDir } from "@tauri-apps/api/path";

import { LAYER_MARKER_DIRS } from "@/shared/lib/team-content-path";
import { logger } from "@/shared/lib/logger";

/** The CodeSense home, `~/.codesense`: the same `dirs::home_dir()` join as
 *  Rust's `codesense_home_dir()` (Tauri's `homeDir()` reads the same $HOME).
 *  The one source of truth — never parsed from a context repo path, which can
 *  live anywhere on disk. Resolved once by {@link initCodesenseHome} before the
 *  app's first render, so every synchronous read sees it. */
let home: string | null = null;
let homeRequest: Promise<string | null> | null = null;

/** Reads `~/.codesense` once. Idempotent. Resolves `null` (and logs) when the
 *  OS home cannot be read; callers already treat a null home as unavailable. */
export function initCodesenseHome(): Promise<string | null> {
  if (!homeRequest) {
    homeRequest = homeDir()
      .then((dir) => {
        home = `${dir.replace(/\/+$/, "")}/.codesense`;
        return home;
      })
      .catch((e: unknown) => {
        logger.warn("codesense-home", `cannot read the home directory: ${String(e)}`);
        return null;
      });
  }
  return homeRequest;
}

/** The CodeSense home, or `null` when it could not be read. */
export function getCodesenseHome(): string | null {
  return home;
}

/** Tests: set (or clear) the home without a Tauri call. */
export function setCodesenseHomeForTests(value: string | null): void {
  home = value;
  homeRequest = value === null ? null : Promise.resolve(value);
}

/** Strip the trailing `{team}/{marker}/...` from a flat context-repo or
 *  per-team-content path to get the org root (SLW-IN-7). Mirror of Rust
 *  `derive_org_root`: post-flatten a team dir sits directly at the org root and
 *  directly contains a layer marker (`install`/`content`/`repos`/`libraries`),
 *  so the org root is everything before the team segment that precedes the FIRST
 *  such marker.
 *
 *  Returns the path unchanged if no layer marker is found, with a warn log for
 *  visibility (callers that pass an org root already are common, but a path
 *  that's neither org root nor a team-scoped path indicates a wiring bug).
 *
 *  Uses the FIRST marker (not the last) to minimize the residual risk of a
 *  marker name appearing inside the org slug; a team named like a marker is
 *  reserved at creation (Rust `validate_team_name_full` D-1 Class B). */
export function deriveOrgRoot(contextRepoPath: string): string {
  const parts = contextRepoPath.split("/");
  const markerIdx = parts.findIndex((p) => (LAYER_MARKER_DIRS as readonly string[]).includes(p));
  // Need `[…org…, team, marker]` — the team is `parts[markerIdx - 1]` and the
  // org root (everything before it) must be non-empty.
  if (markerIdx >= 2) {
    const orgRoot = parts.slice(0, markerIdx - 1).join("/");
    if (orgRoot !== "" && orgRoot !== "/") return orgRoot;
  }
  // Most callers pass an already-stripped org root, so don't warn for paths
  // that look like one (`.../orgs/{slug}` at the tail). Only warn when neither
  // pattern holds — that's the genuinely suspicious case worth surfacing.
  if (!/\/orgs\/[^/]+\/?$/.test(contextRepoPath)) {
    logger.warn(
      "codesense-home",
      `deriveOrgRoot: path "${contextRepoPath}" has no layer marker nor a trailing "orgs/{slug}" — returning unchanged`,
    );
  }
  return contextRepoPath;
}

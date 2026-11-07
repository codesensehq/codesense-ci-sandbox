// SPDX-License-Identifier: MIT
/**
 * The team starter page, as the file menus offer it: "Set as team starter page"
 * on a file in the active team's content, "Team starter page" + "Clear starter
 * page" on the one already set. The page itself is `teams.yml` `landing_page:`
 * (see `team-landing-page.ts`); setting it is a registry edit
 * (`SetTeamLandingPage`), so nobody edits `teams.yml` by hand.
 */
import { CODESENSE_DOCS_DISPLAY_NAME, isCodesenseDocs } from "@/shared/lib/codesense-docs";
import type { Org, Team } from "@/shared/types/domain";

import { contentRelativePath, isContentRelative, teamContentRoot } from "./team-landing-page";

export type StarterPageChoice =
  /** Not a file of any team's content (a source repo file, a virtual tab): no item. */
  | { kind: "hidden" }
  /** A file the starter page can't be: the item shows, unavailable, with why. */
  | { kind: "blocked"; reason: string }
  /** A file of the team's content that can become its starter page. */
  | { kind: "settable"; rel: string }
  /** The team's starter page: `declared` when set in `teams.yml`, else the
   *  README/index the team opens by default. */
  | { kind: "current"; rel: string; declared: boolean };

/** A file node by its name: a basename with an extension, not a dotfile. Folders
 *  (epic and plain directories) carry no starter page item. */
export function looksLikeFile(path: string): boolean {
  const name = path.replace(/\/+$/, "").split("/").pop() ?? "";
  return /^[^.].*\.[^./]+$/.test(name);
}

/**
 * What the file menus offer for `path` in `team`. `effectivePath` is the page the
 * team opens when nothing is open (declared or by convention), as resolved on
 * disk, or `null`. `isFile` is the row's real kind when the caller knows it; a
 * name with an extension counts as a file otherwise.
 */
export function starterPageChoice(input: {
  org: Pick<Org, "slug" | "org_root">;
  team: Pick<Team, "name" | "landing_page">;
  path: string;
  effectivePath: string | null;
  isFile?: boolean;
}): StarterPageChoice {
  const { org, team, path, effectivePath } = input;
  const contentRoot = teamContentRoot(org.org_root, team.name);
  const rel = contentRelativePath(path, contentRoot);
  const name = path.replace(/\/+$/, "").split("/").pop() ?? "";
  const isFile = input.isFile ?? (looksLikeFile(path) || name.startsWith("."));
  if (rel === null) {
    if (!isFile) return { kind: "hidden" };
    // Another team's file (a parent team's, shown inherited): say whose it is.
    const owner = contentRelativePath(path, org.org_root)?.match(/^([^/]+)\/content\//)?.[1];
    return owner && owner !== team.name
      ? {
          kind: "blocked",
          reason: `This file is ${owner}'s. A starter page must be one of ${team.name}'s own files.`,
        }
      : { kind: "hidden" };
  }
  if (!isFile) return { kind: "hidden" };
  if (isCodesenseDocs(org.slug)) {
    return { kind: "blocked", reason: `${CODESENSE_DOCS_DISPLAY_NAME} are read-only.` };
  }
  if (!isContentRelative(rel)) {
    return {
      kind: "blocked",
      reason: name.startsWith(".")
        ? "Hidden files can't be a starter page."
        : "Files in hidden folders can't be a starter page.",
    };
  }
  const declared = team.landing_page?.trim() || null;
  if (declared !== null && declared === rel) return { kind: "current", rel, declared: true };
  const effective = effectivePath ? contentRelativePath(effectivePath, contentRoot) : null;
  if (declared === null && effective !== null && effective === rel) {
    return { kind: "current", rel, declared: false };
  }
  return { kind: "settable", rel };
}

/** Is `path` the page the team opens when nothing is open? Drives the nav marker. */
export function isStarterPagePath(
  path: string,
  effectivePath: string | null,
  contentRoot: string,
): boolean {
  if (!effectivePath) return false;
  const rel = contentRelativePath(path, contentRoot);
  return rel !== null && rel === contentRelativePath(effectivePath, contentRoot);
}

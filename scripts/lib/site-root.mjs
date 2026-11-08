// SPDX-License-Identifier: MIT
// The website source the docs tools read (the handbook MDX, the generated agent releases): site/ in
// the checkout, or $CODESENSE_SITE_ROOT, an absolute path to a site checked out somewhere else. A set
// variable must name a folder holding src/content/handbook; anything else throws. The resolved root is
// printed to stderr once per process, so a run always says which site it read.
import { existsSync, realpathSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

export const HANDBOOK_DIR = join("src", "content", "handbook");
const told = new Set();

/** The site folder for the checkout at `root`. */
export function siteRoot(root, env = process.env) {
  const set = env.CODESENSE_SITE_ROOT;
  if (!set) return join(root, "site");
  if (!isAbsolute(set)) throw new Error(`CODESENSE_SITE_ROOT must be an absolute path, not ${set}`);
  const site = resolve(set);
  if (!existsSync(join(site, HANDBOOK_DIR)))
    throw new Error(`CODESENSE_SITE_ROOT ${site} holds no ${HANDBOOK_DIR}`);
  if (!told.has(site)) {
    told.add(site);
    console.error(`site: ${site} (CODESENSE_SITE_ROOT)`);
  }
  return site;
}

const real = (p) => (existsSync(p) ? realpathSync(p) : resolve(p));

/** Whether `site` lies inside the checkout at `root`, by real path (`..x` is a name, not a parent). */
export function siteInside(root, site) {
  const rel = relative(real(root), real(site));
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

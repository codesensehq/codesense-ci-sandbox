// SPDX-License-Identifier: MIT
import type { SourceRepoSlug } from "@/shared/types/domain";

/**
 * SLW-PT-11 — the ONE "Context" abstraction and its stable logical id.
 *
 * A Context is any place content lives. There are exactly four shapes:
 *
 *   - **Scratch space**  — private to you, kept for the open org (a move/copy
 *                          TARGET only; it is browsed in the Scratch space tab,
 *                          never picked in the nav dropdown).
 *   - **All of <Team>**  — the team's repo-agnostic content (`{team}/content/`).
 *   - **<Team> · <Repo>** — team content scoped to one source repo: the repo's
 *                          content tier `repos/{host}/{owner}/{repo}/content/`
 *                          (SW-XS-15; never the retired `install/` tier).
 *   - **View-only repo** — a source repo you can see but haven't shared into.
 *
 * The user NEVER sees provider / overlay / layer / TeamWorkspace / TeamRepo / a
 * bare "Workspace" — only "Scratch space" vs team names. The WORDS of every label
 * disambiguate on their own (§G/§T).
 *
 * This module owns the ONE stable logical id (`ContextId`) that every
 * move/copy/remove op (SLW-PT-12/13/14/17) accepts.
 * The id is a string address, not a path — no path-vs-logical ambiguity. It
 * round-trips: `parseContextId(contextId(ctx))` reconstructs the same address,
 * and re-enumerating after a refresh yields the same id for the same item.
 */

/** The kind discriminant of a {@link Context}. */
export type ContextKind = "personal" | "team-all" | "team-repo" | "view-only-repo";

/** Whether content in a context is private to you or shared with a team. */
export type ContextSharing = "personal" | "shared";

/** A resolved place content lives. `id` is the stable logical address; every
 *  other field is display/scoping metadata derived from it. */
export interface Context {
  /** The stable logical id — the address space the whole epic shares. */
  id: ContextId;
  kind: ContextKind;
  /** Private-to-you vs shared-with-a-team. Scratch ⇒ "personal"; any team
   *  prefix ⇒ "shared". Drives the badge's private/lock affordance. */
  sharing: ContextSharing;
  /** The self-sufficient label whose WORDS alone convey the context: "Scratch
   *  space", "All of <Team>", or "<Team> · <Repo>". Never a bare "Workspace". */
  label: string;
  /** The team this context belongs to (absent for scratch and view-only). */
  teamName?: string;
  /** The source repo this context is scoped to (present for team-repo and
   *  view-only). */
  repoSlug?: SourceRepoSlug;
}

/** The stable logical id string. Opaque to consumers — build it with
 *  {@link contextId} / the section builders and read it with
 *  {@link parseContextId}; never hand-concatenate. */
export type ContextId = string;

// ---------------------------------------------------------------------------
// Stable logical id — the address space.
//
// Encodings (":" separates fields; the repo slug keeps its own "/"):
//   personal
//   team:<orgSlug>:<teamName>
//   team-repo:<orgSlug>:<teamName>:<owner>/<repo>
//   repo:<owner>/<repo>              (view-only; org/team-agnostic)
//
// orgSlug is always the FIRST field after the prefix so it can never collide
// with a team name that itself contains a colon: teamName is the remainder up
// to the (optional) trailing slug, and the slug is the final "/"-joined pair.
// ---------------------------------------------------------------------------

export function personalContextId(): ContextId {
  return "personal";
}

/** One named org's scratch space (`personal:<orgSlug>`) — the same id the MCP
 *  tools take. Unlike the bare id it never depends on the org on screen. */
export function scratchSpaceContextId(orgSlug: string): ContextId {
  return [personalContextId(), orgSlug].join(":");
}

/** The scratch folder of inert items that apply in every org. */
export function allOrgsContextId(): ContextId {
  return "all-orgs";
}

export function teamAllContextId(orgSlug: string, teamName: string): ContextId {
  return `team:${orgSlug}:${teamName}`;
}

export function teamRepoContextId(
  orgSlug: string,
  teamName: string,
  slug: SourceRepoSlug,
): ContextId {
  return `team-repo:${orgSlug}:${teamName}:${slug.owner}/${slug.repo}`;
}

export function viewOnlyRepoContextId(slug: SourceRepoSlug): ContextId {
  return `repo:${slug.owner}/${slug.repo}`;
}

/** The single id builder — dispatches on kind so callers (and future ops) mint
 *  ids one way and can't drift from the encoding. */
export function contextId(ctx: {
  kind: ContextKind;
  orgSlug?: string;
  teamName?: string;
  repoSlug?: SourceRepoSlug;
}): ContextId {
  switch (ctx.kind) {
    case "personal":
      return personalContextId();
    case "team-all":
      return teamAllContextId(ctx.orgSlug ?? "", ctx.teamName ?? "");
    case "team-repo":
      return teamRepoContextId(ctx.orgSlug ?? "", ctx.teamName ?? "", ctx.repoSlug ?? emptySlug());
    case "view-only-repo":
      return viewOnlyRepoContextId(ctx.repoSlug ?? emptySlug());
  }
}

/** The parsed components of a {@link ContextId}. `null` for a malformed id. */
export interface ParsedContextId {
  kind: ContextKind;
  /** For a scratch id: the all-orgs folder rather than one org's space. */
  allOrgs?: boolean;
  orgSlug?: string;
  teamName?: string;
  repoSlug?: SourceRepoSlug;
}

/** Round-trip a {@link ContextId} back to its components. `parseContextId` ∘
 *  `contextId` is the identity on every well-formed address. Returns `null`
 *  for anything it can't decode (defensive — a persisted id from a future
 *  encoding must never crash enumeration). */
export function parseContextId(id: ContextId): ParsedContextId | null {
  if (id === personalContextId()) return { kind: "personal" };
  if (id === allOrgsContextId()) return { kind: "personal", allOrgs: true };
  const scratchPrefix = `${personalContextId()}:`;
  if (id.startsWith(scratchPrefix)) {
    const orgSlug = id.slice(scratchPrefix.length);
    return orgSlug ? { kind: "personal", orgSlug } : null;
  }

  if (id.startsWith("repo:")) {
    const slug = parseSlug(id.slice("repo:".length));
    return slug ? { kind: "view-only-repo", repoSlug: slug } : null;
  }

  if (id.startsWith("team-repo:")) {
    const rest = id.slice("team-repo:".length);
    const parts = splitOrgTeamSlug(rest);
    if (!parts) return null;
    return {
      kind: "team-repo",
      orgSlug: parts.orgSlug,
      teamName: parts.teamName,
      repoSlug: parts.repoSlug,
    };
  }

  if (id.startsWith("team:")) {
    const rest = id.slice("team:".length);
    const idx = rest.indexOf(":");
    if (idx === -1) return null;
    return {
      kind: "team-all",
      orgSlug: rest.slice(0, idx),
      teamName: rest.slice(idx + 1),
    };
  }

  return null;
}

/** Split `<orgSlug>:<teamName>:<owner>/<repo>` — orgSlug is the first field,
 *  the slug is the final "/"-joined pair, teamName is everything between (so a
 *  team name may itself contain colons). */
function splitOrgTeamSlug(
  rest: string,
): { orgSlug: string; teamName: string; repoSlug: SourceRepoSlug } | null {
  const firstColon = rest.indexOf(":");
  if (firstColon === -1) return null;
  const orgSlug = rest.slice(0, firstColon);
  const afterOrg = rest.slice(firstColon + 1);
  // The slug is the trailing `owner/repo` after the LAST colon.
  const lastColon = afterOrg.lastIndexOf(":");
  if (lastColon === -1) return null;
  const teamName = afterOrg.slice(0, lastColon);
  const slug = parseSlug(afterOrg.slice(lastColon + 1));
  if (!teamName || !slug) return null;
  return { orgSlug, teamName, repoSlug: slug };
}

function parseSlug(raw: string): SourceRepoSlug | null {
  const parts = raw.split("/");
  if (parts.length !== 2 || !parts[0] || !parts[1]) return null;
  return { owner: parts[0], repo: parts[1] };
}

function emptySlug(): SourceRepoSlug {
  return { owner: "", repo: "" };
}

// ---------------------------------------------------------------------------
// Vocabulary — the ONLY place these strings are minted (§G/§T/§M).
// ---------------------------------------------------------------------------

/** The repo-agnostic bucket reads "All of <Team>" EVERYWHERE (dropdown, badge,
 *  subtitle). Never "Workspace", never "· all repos". */
export function teamAllLabel(teamName: string): string {
  return `All of ${teamName}`;
}

/** A repo-scoped team context reads "<Team> · <Repo>". */
export function teamRepoLabel(teamName: string, repoName: string): string {
  return `${teamName} · ${repoName}`;
}

/** The scratch space's name wherever it is a move/copy target. */
export const SCRATCH_SPACE_LABEL = "Private";

/** The all-orgs folder as a move target ("Use in all orgs"). */
export const ALL_ORGS_LABEL = "All orgs";

/** Another org's scratch space as a copy target. */
export function scratchSpaceInLabel(orgName: string): string {
  return `${SCRATCH_SPACE_LABEL} in ${orgName}`;
}

// ---------------------------------------------------------------------------
// The team's own content as a Context: the SOURCE of every move/copy/remove the
// nav's ⋯ action makes (SW-XS-15: the nav always browses the team's content; the
// left-nav pivot picks whose context it overlays, never a content tier).
// ---------------------------------------------------------------------------

/** "All of <Team>": the team's repo-agnostic content (`{team}/content/`). */
export function teamAllContext(orgSlug: string, teamName: string): Context {
  return {
    id: teamAllContextId(orgSlug, teamName),
    kind: "team-all",
    sharing: "shared",
    label: teamAllLabel(teamName),
    teamName,
  };
}

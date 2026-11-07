// SPDX-License-Identifier: MIT
import {
  personalContextId,
  SCRATCH_SPACE_LABEL,
  teamAllContextId,
  teamAllLabel,
  teamRepoContextId,
  teamRepoLabel,
  type Context,
  type ContextId,
} from "@/shared/lib/context-model";
import type { ContextMobilityOp } from "@/shared/lib/move-gate-ladder";
import type { Org, SourceRepoSlug } from "@/shared/types/domain";

/**
 * SLW-PT-16 — the org-scoped move/copy TARGET PICKER (§E) that supplies step 1
 * of the SLW-PT-12/13 stepped sheet. Given the (already-known) SOURCE context
 * plus the loaded org list, it enumerates the places the item can move/copy to:
 *
 *   - **Scratch space** — always listed: the open org's scratch space.
 *   - **Same-org targets** — every OTHER context in the ACTIVE org (All of
 *     <Team> + each <Team> · <Repo>), listed normally and selectable.
 *   - **Cross-org targets** — the OTHER orgs' team buckets, shown DISABLED WITH A
 *     REASON ("<OtherOrg> — switch orgs to place content here"), never silently
 *     omitted. Choosing one drives the carry-switch-resume flow (cross-org-resume).
 *
 * Empty/omitted cases are always explained (§E) so the picker never dead-ends on
 * a bare empty list: zero teams offers Create-a-team; no other repos states the
 * reason; the source itself is filtered out (you never move a thing onto itself).
 *
 * PURE: no React, no store reads — the caller passes the source, the active org,
 * and the full org list. The precedence + reason strings live here so they're
 * unit-tested and can't drift, and no banned internal word ever reaches a label.
 */

/** A single row in the target picker. */
export interface TargetPickerRow {
  /** The stable logical id of this target context. */
  context: Context;
  /** Whether this row can be chosen directly (same-org) or is a cross-org row
   *  that first requires an org switch. */
  disabled: boolean;
  /** The org slug this target lives in — used by the carry-switch-resume flow to
   *  know which org to switch into. `null` for the scratch space (the open org). */
  orgSlug: string | null;
  /** When disabled, the human reason shown greyed on the row (§E). */
  reason?: string;
}

/** An explained empty/next-step case for a picker section that has no rows. */
export interface TargetPickerNextStep {
  kind: "createTeam" | "noOtherRepos";
  /** The labeled call-to-action / explanation shown in place of an empty list. */
  label: string;
}

/** The full picker model consumed by the target sheet. */
export interface TargetPicker {
  /** The scratch space — always present (unless the source IS the scratch space). */
  personal: TargetPickerRow | null;
  /** Same-org, selectable target rows (excludes the source). */
  sameOrg: TargetPickerRow[];
  /** Cross-org rows, disabled-with-reason, grouped after same-org. */
  crossOrg: TargetPickerRow[];
  /** A labeled next step when a section would otherwise be an empty dead-end
   *  (zero teams ⇒ Create a team; no other repos ⇒ the reason). `null` when the
   *  picker has real same-org targets. */
  nextStep: TargetPickerNextStep | null;
}

export interface BuildTargetPickerInput {
  /** The item's source context (already known — never asked). */
  source: Context;
  /** The active org slug (same-org targets are enabled; others disabled). */
  activeOrgSlug: string;
  /** Every loaded org, used to enumerate same-org + cross-org targets. */
  orgs: readonly Org[];
  /** MOVE (default) or COPY — only affects downstream verb copy, not the rows. */
  op?: ContextMobilityOp;
}

/** The greyed reason on a cross-org row (§E) — never omit the row, name the org
 *  and the one action that unlocks it. */
export function crossOrgReason(orgDisplayName: string): string {
  return `${orgDisplayName} — switch orgs to place content here`;
}

/** The zero-teams "Move to Shared" create-a-team next step (§E) — never an empty
 *  picker. */
export const CREATE_TEAM_NEXT_STEP: TargetPickerNextStep = {
  kind: "createTeam",
  label: "Create a team to share into",
};

/** The no-other-repos explanation for an org with no additional shared repos. */
export function noOtherReposLabel(orgDisplayName: string): TargetPickerNextStep {
  return { kind: "noOtherRepos", label: `No other repos in ${orgDisplayName} yet.` };
}

/** Build the target picker rows for a move/copy from `source`. Same-org targets
 *  are selectable; cross-org targets are disabled-with-reason; the scratch space always
 *  lists; the source is filtered from its own picker; empty cases resolve to a
 *  labeled next step. */
export function buildTargetPicker(input: BuildTargetPickerInput): TargetPicker {
  const { source, activeOrgSlug, orgs } = input;

  const sourceIsPersonal = source.kind === "personal";

  // The scratch space — always a valid target. Omitted only when the source
  // itself IS the scratch space (you never move a thing onto itself).
  const personal: TargetPickerRow | null = sourceIsPersonal
    ? null
    : {
        context: {
          id: personalContextId(),
          kind: "personal",
          sharing: "personal",
          label: SCRATCH_SPACE_LABEL,
        },
        disabled: false,
        orgSlug: null,
      };

  const sameOrg: TargetPickerRow[] = [];
  const crossOrg: TargetPickerRow[] = [];

  for (const org of orgs) {
    const isActiveOrg = org.slug === activeOrgSlug;
    for (const team of visibleTeamsForTargets(org)) {
      for (const row of teamTargetRows(org.slug, team.name, team.source_repos)) {
        // Never list the source as a target of itself.
        if (row.context.id === source.id) continue;
        if (isActiveOrg) {
          sameOrg.push(row);
        } else {
          crossOrg.push({
            ...row,
            disabled: true,
            reason: crossOrgReason(org.display_name),
          });
        }
      }
    }
  }

  // Explained empty case (§E): the ACTIVE org has no selectable same-org team
  // target at all (zero teams / all filtered). Offer Create-a-team rather than a
  // bare empty list. The scratch space + cross-org rows may still render alongside.
  const activeOrg = orgs.find((o) => o.slug === activeOrgSlug);
  const activeOrgHasTeams = (activeOrg?.teams ?? []).some((t) => t.archived !== true);
  const nextStep: TargetPickerNextStep | null =
    sameOrg.length === 0 ? (activeOrgHasTeams ? null : CREATE_TEAM_NEXT_STEP) : null;

  return { personal, sameOrg, crossOrg, nextStep };
}

/** Archived teams are excluded from target rows (a retired team is not a place
 *  to put content), mirroring the nav's `visibleTeams` fold. */
function visibleTeamsForTargets(org: Org) {
  return org.teams.filter((t) => t.archived !== true);
}

/** The target rows for one team: "All of <Team>" + each "<Team> · <Repo>". */
function teamTargetRows(
  orgSlug: string,
  teamName: string,
  sourceRepos: readonly SourceRepoSlug[],
): TargetPickerRow[] {
  const rows: TargetPickerRow[] = [];
  rows.push({
    context: {
      id: teamAllContextId(orgSlug, teamName),
      kind: "team-all",
      sharing: "shared",
      label: teamAllLabel(teamName),
      teamName,
    },
    disabled: false,
    orgSlug,
  });
  const seen = new Set<string>();
  for (const slug of sourceRepos) {
    const key = `${slug.owner}/${slug.repo}`;
    if (seen.has(key)) continue;
    seen.add(key);
    rows.push({
      context: {
        id: teamRepoContextId(orgSlug, teamName, slug),
        kind: "team-repo",
        sharing: "shared",
        label: teamRepoLabel(teamName, slug.repo),
        teamName,
        repoSlug: slug,
      },
      disabled: false,
      orgSlug,
    });
  }
  return rows;
}

/** All selectable (enabled) target ids in the picker — the scratch space + same-org. Used
 *  by tests and by the sheet to validate a chosen id is directly placeable. */
export function selectableTargetIds(picker: TargetPicker): ContextId[] {
  const ids: ContextId[] = [];
  if (picker.personal) ids.push(picker.personal.context.id);
  for (const r of picker.sameOrg) ids.push(r.context.id);
  return ids;
}

// SPDX-License-Identifier: MIT
import { Waypoints } from "lucide-react";

import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";
import { useActiveTeamConfigTarget } from "@/features/settings/hooks/use-active-team-config-target";
import { useStatusTaxonomy } from "@/features/viewer/hooks/use-status-taxonomy";
import { EmptyState } from "@/shared/components/empty-state";
import { useOrgStore } from "@/shared/stores/org-store";
import { SectionHeader } from "../section-header";
import { PhasesPanel } from "./phases-panel";

/** The nav/search spec for the Phases section. */
export const PHASES_SECTION_SPEC: SettingRowSpec = {
  key: "phases",
  label: "Phases",
  keywords: ["phases", "phase", "status", "statuses", "taxonomy", "states", "dropped"],
};

interface PhasesSectionProps {
  query: string;
}

/**
 * Settings → Phases: the team's status taxonomy (order, label, color, dropped).
 * Story `status` is plain frontmatter; this only names and orders the states the
 * nav groups by.
 */
export function PhasesSection({ query }: PhasesSectionProps) {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const stateLoaded = useOrgStore((s) => s.stateLoaded);
  const { team, orgRoot, teamKey } = useActiveTeamConfigTarget();
  // Per-team hydration — always called (Rules of Hooks); no-ops for a null team.
  useStatusTaxonomy(teamKey);

  if (!rowMatches(PHASES_SECTION_SPEC, query)) return null;

  return (
    <section className="pt-6" data-testid="settings-phases-section">
      <SectionHeader id="phases">Phases</SectionHeader>
      {stateLoaded && activeOrgSlug === null ? (
        <p className="py-4 text-sm text-muted-foreground" data-testid="settings-phases-no-active">
          No active workspace — use the workspace switcher to pick one.
        </p>
      ) : team === null || orgRoot === "" ? (
        // No team selected, or the active org's root hasn't resolved (cold-boot
        // window) — never render the editor against an unrooted path, so a save
        // can't write nowhere.
        <EmptyState
          icon={<Waypoints />}
          title="No team selected"
          body="Pick a team to edit its phases."
          testId="settings-phases-no-team"
        />
      ) : (
        <div className="p-2">
          <PhasesPanel orgRoot={orgRoot} team={team} teamKey={teamKey} />
        </div>
      )}
    </section>
  );
}

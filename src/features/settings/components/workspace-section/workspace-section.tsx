// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";
import { SectionHeader } from "../section-header";
import { ConfigCard } from "./config-card";
import { MembersCard } from "./members-card";
import { TeamsCard } from "./teams-card";
import { rowMatches } from "@/features/settings/lib/row-matches";
import { findRowSpec } from "./lib/workspace-row-specs";

/** SW-XS-11 (SPEC §8.4): how context reaches an agent, stated once. There is
 *  nothing to configure, so it is plain text, not a row with a control. */
export const CONTEXT_DELIVERY_NOTE =
  "Agents get the context their definition declares when they start, and pull or publish more through CodeSense tools. Nothing syncs in the background.";

interface WorkspaceSectionProps {
  query: string;
}

/**
 * Settings -> Workspace section. Per ux-spec Surface 3 (Q5 resolution):
 * row 1 holds Config + Members at half-width each; row 2 holds Teams at
 * full-width.
 * SW-XS-11: one muted line under the header says how agents get context; the
 * sync cadence and hydration mode rows it replaced are gone.
 */
export function WorkspaceSection({ query }: WorkspaceSectionProps) {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const stateLoaded = useOrgStore((s) => s.stateLoaded);
  const activeOrg = useOrgStore((s) =>
    s.activeOrgSlug !== null ? (s.orgs.find((o) => o.slug === s.activeOrgSlug) ?? null) : null,
  );

  return (
    <section className="pt-6">
      <SectionHeader id="workspace">Workspace</SectionHeader>
      {rowMatches(findRowSpec("context-delivery"), query) && (
        <p
          className="py-2 text-sm text-muted-foreground"
          data-testid="settings-workspace-context-note"
        >
          {CONTEXT_DELIVERY_NOTE}
        </p>
      )}
      <Body
        activeOrgSlug={activeOrgSlug}
        stateLoaded={stateLoaded}
        activeOrg={activeOrg}
        query={query}
      />
    </section>
  );
}

function Body({
  activeOrgSlug,
  stateLoaded,
  activeOrg,
  query,
}: {
  activeOrgSlug: string | null;
  stateLoaded: boolean;
  activeOrg: ReturnType<typeof useOrgStore.getState>["orgs"][number] | null;
  query: string;
}) {
  if (stateLoaded && activeOrgSlug === null) {
    return (
      <p className="py-4 text-sm text-muted-foreground" data-testid="settings-workspace-no-active">
        No active workspace — use the workspace switcher to pick one.
      </p>
    );
  }

  if (activeOrg === null) {
    return (
      <div className="py-4 text-xs text-muted-foreground" data-testid="settings-workspace-skeleton">
        <div className="h-4 w-32 animate-pulse rounded bg-muted" />
      </div>
    );
  }

  return (
    <div className="mt-4 flex flex-col gap-4">
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <ConfigCard org={activeOrg} query={query} />
        <MembersCard org={activeOrg} query={query} />
      </div>
      <TeamsCard
        org={activeOrg}
        query={query}
        onCreateTeamClick={() => openManageTeams("create")}
      />
    </div>
  );
}

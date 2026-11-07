// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { toast } from "sonner";
import { VirtualList } from "@/shared/components/virtual-list";
import { useMapToggle } from "@/shared/hooks/use-map-toggle";
import { pruneMapByKeys } from "@/shared/lib/prune-map-by-keys";
import type { Attachment, Org } from "@/shared/types/domain";
import { useAttachmentsByRemote } from "@/features/attachment/hooks/use-attachments-by-remote";
import { rowMatches } from "@/features/settings/lib/row-matches";
import { PermissionGate } from "@/features/settings/components/permission-gate";
import { findRowSpec } from "./lib/workspace-row-specs";
import { SettingsCard } from "./settings-card";
import { TeamTile } from "./team-tile";

/** Threshold above which the team list switches to a vertical virtualized
 *  scroll container with variable-row measurement (each TeamTile collapses
 *  to ~40px but grows arbitrarily large when expanded). */
const VIRTUALIZATION_THRESHOLD = 15;
const TEAM_ROW_ESTIMATE = 44;

interface TeamsCardProps {
  org: Org;
  query: string;
  onCreateTeamClick?: () => void;
}

export function TeamsCard({ org, query, onCreateTeamClick }: TeamsCardProps) {
  const spec = findRowSpec("teams");
  const sourceReposSpec = findRowSpec("source-repos");
  const showTeams = rowMatches(spec, query);
  const showSourceRepos = rowMatches(sourceReposSpec, query);

  if (!showTeams && !showSourceRepos) return null;

  const teams = org.teams;

  const createTeamButton = (
    <PermissionGate required="any-writer">
      <button
        type="button"
        onClick={onCreateTeamClick ?? defaultCreateTeam}
        data-testid="settings-teams-create-button"
        className="rounded border border-border bg-background px-2 py-1 text-xs hover:bg-accent"
      >
        + Create team
      </button>
    </PermissionGate>
  );

  return (
    <SettingsCard
      title="Teams"
      count={teams.length}
      headerAction={createTeamButton}
      data-testid="settings-card-teams"
    >
      {teams.length === 0 && (
        <p className="text-xs text-muted-foreground" data-testid="settings-teams-empty">
          No teams yet. Create your first team.
        </p>
      )}
      {teams.length > 0 && <TeamsCardBody teams={teams} org={org} />}
    </SettingsCard>
  );
}

/** Inner body that uses hooks for attachment fetching — separated because
 *  the parent returns null early for search filtering (hooks can't be
 *  called conditionally). */
function TeamsCardBody({ teams, org }: { teams: Org["teams"]; org: Org }) {
  // Shared attachment loader (SW-HB-3).
  const attachmentsByRemote = useAttachmentsByRemote();

  // NB-7 / AC-5: Hoisted per-team expanded + track-dialog open state.
  // Lives at the TeamsCardBody level so scrolling a TeamTile past the
  // virtualizer's overscan window (eviction) does NOT lose its expand
  // state. Keyed by `team.name` (the same key the virtualizer uses).
  const [expandedMap, setTeamExpanded, setExpandedMap] = useMapToggle<string>();
  const [trackDialogMap, setTeamTrackDialogOpen, setTrackDialogMap] = useMapToggle<string>();

  // NB-7 / code-review patch: GC Map entries whose team no longer exists
  // (team deleted via registry edit, org/workspace switch). Without this the
  // Maps grow unbounded across long sessions and a future team re-created
  // with the same name would mount with stale state. Set-state-in-effect is
  // intentional — these maps track user interaction state (expanded/dialog),
  // which can't be a useMemo, so we sync them with the live team list via a
  // pure updater with reference-equality bail-out.
  useEffect(() => {
    const liveNames = new Set(teams.map((t) => t.name));
    setExpandedMap((prev) => pruneMapByKeys(prev, liveNames));
    setTrackDialogMap((prev) => pruneMapByKeys(prev, liveNames));
  }, [teams, setExpandedMap, setTrackDialogMap]);

  if (teams.length > VIRTUALIZATION_THRESHOLD) {
    return (
      <VirtualizedTeamList
        teams={teams}
        org={org}
        attachmentsByRemote={attachmentsByRemote}
        expandedMap={expandedMap}
        trackDialogMap={trackDialogMap}
        onExpandedChange={setTeamExpanded}
        onTrackDialogOpenChange={setTeamTrackDialogOpen}
      />
    );
  }

  return (
    <div role="list" className="flex flex-col gap-1.5" data-testid="settings-teams-list">
      {teams.map((team) => (
        <TeamTile
          key={team.name}
          team={team}
          org={org}
          attachmentsByRemote={attachmentsByRemote}
          expanded={expandedMap.get(team.name) ?? false}
          trackDialogOpen={trackDialogMap.get(team.name) ?? false}
          onExpandedChange={setTeamExpanded}
          onTrackDialogOpenChange={setTeamTrackDialogOpen}
        />
      ))}
    </div>
  );
}

// ---------- Virtualized team list (above threshold) ----------

function VirtualizedTeamList({
  teams,
  org,
  attachmentsByRemote,
  expandedMap,
  trackDialogMap,
  onExpandedChange,
  onTrackDialogOpenChange,
}: {
  teams: Org["teams"];
  org: Org;
  attachmentsByRemote: Map<string, Attachment>;
  expandedMap: Map<string, boolean>;
  trackDialogMap: Map<string, boolean>;
  onExpandedChange: (name: string, open: boolean) => void;
  onTrackDialogOpenChange: (name: string, open: boolean) => void;
}) {
  return (
    <VirtualList<Org["teams"][number]>
      items={teams}
      estimateSize={TEAM_ROW_ESTIMATE}
      getKey={(t) => t.name}
      overscan={6}
      // TeamTile expands to varying heights — measureElement adapts on render.
      measureElement={(el) => el.getBoundingClientRect().height}
      className="max-h-[400px] overflow-y-auto"
      data-testid="settings-teams-list"
      renderItem={(team) => (
        <div style={{ paddingBottom: 6 }}>
          <TeamTile
            team={team}
            org={org}
            attachmentsByRemote={attachmentsByRemote}
            expanded={expandedMap.get(team.name) ?? false}
            trackDialogOpen={trackDialogMap.get(team.name) ?? false}
            onExpandedChange={onExpandedChange}
            onTrackDialogOpenChange={onTrackDialogOpenChange}
          />
        </div>
      )}
    />
  );
}

function defaultCreateTeam() {
  toast.info("Create team is coming in C.10a");
}

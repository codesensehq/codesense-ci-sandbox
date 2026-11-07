// SPDX-License-Identifier: MIT
import { memo, useCallback, useId, useRef, useState } from "react";
import { ChevronDown, ChevronRight, House, Plus } from "lucide-react";
import type { Attachment, Org, Team } from "@/shared/types/domain";
import { PermissionGate } from "@/features/settings/components/permission-gate";
import { AttachRepoDialog } from "@/features/dashboard/components/attach-repo-dialog";
import { SourceRepoList } from "@/features/attachment/components/source-repo-list";
import { Input } from "@/components/ui/input";
import {
  contentRelativePath,
  isContentRelative,
  teamContentRoot,
} from "@/features/viewer/lib/team-landing-page";
import { useActiveTeamStarterPage } from "@/features/viewer/hooks/use-team-starter-page";
import { changeTeamStarterPage } from "@/features/viewer/components/starter-page-menu-items";
import { CODESENSE_DOCS_DISPLAY_NAME, isCodesenseDocs } from "@/shared/lib/codesense-docs";

interface TeamTileProps {
  team: Team;
  org: Org;
  attachmentsByRemote: Map<string, Attachment>;
  /** NB-7 / AC-5: controlled expand state — hoisted to teams-card so
   *  virtualizer overscan eviction does not lose user-driven state. */
  expanded: boolean;
  trackDialogOpen: boolean;
  onExpandedChange: (name: string, open: boolean) => void;
  onTrackDialogOpenChange: (name: string, open: boolean) => void;
}

export const TeamTile = memo(function TeamTile({
  team,
  org,
  attachmentsByRemote,
  expanded,
  trackDialogOpen,
  onExpandedChange,
  onTrackDialogOpenChange,
}: TeamTileProps) {
  const handleToggleExpanded = useCallback(
    () => onExpandedChange(team.name, !expanded),
    [onExpandedChange, team.name, expanded],
  );
  const handleTrackDialogOpenChange = useCallback(
    (open: boolean) => onTrackDialogOpenChange(team.name, open),
    [onTrackDialogOpenChange, team.name],
  );
  const handleOpenTrackDialog = useCallback(
    () => onTrackDialogOpenChange(team.name, true),
    [onTrackDialogOpenChange, team.name],
  );

  return (
    <div role="listitem" data-testid={`settings-team-pill-${team.name}`}>
      <button
        type="button"
        onClick={handleToggleExpanded}
        className="flex w-full items-center gap-2 rounded border border-border bg-background px-3 py-2 text-xs hover:bg-accent"
        aria-expanded={expanded}
      >
        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <span className="font-medium">{team.name}</span>
        <span className="text-muted-foreground">{team.source_repos.length} repos</span>
      </button>

      {expanded && (
        <div
          className="mt-1 flex flex-col gap-3 border-l-2 border-border pl-4 pt-2 text-xs"
          data-testid={`settings-team-expanded-${team.name}`}
        >
          <StarterPageRow team={team} org={org} />

          {/* Source repos */}
          <div data-testid={`settings-source-repos-${team.name}`}>
            <span className="mb-1 block font-medium text-muted-foreground">Source Repos</span>
            {team.source_repos.length === 0 ? (
              <p className="text-muted-foreground" data-testid="settings-source-repos-empty">
                No tracked repos.
              </p>
            ) : (
              // Shared source-repo list (SW-HB-3).
              <SourceRepoList
                slugs={team.source_repos}
                attachmentsByRemote={attachmentsByRemote}
                orgSlug={org.slug}
                teamName={team.name}
                workspaceRootDir={org.config_flags.workspace_root_dir ?? null}
                data-testid="settings-source-repos-list"
              />
            )}
            <div className="mt-2">
              <PermissionGate required="any-writer">
                <AttachRepoDialog
                  open={trackDialogOpen}
                  onOpenChange={handleTrackDialogOpenChange}
                  workspaceTarget={{ orgSlug: org.slug, teamName: team.name }}
                />
                <button
                  type="button"
                  onClick={handleOpenTrackDialog}
                  data-testid={`settings-track-repo-${team.name}`}
                  className="inline-flex items-center gap-1 rounded border border-border bg-background px-2 py-1 text-xs hover:bg-accent"
                >
                  <Plus size={12} />
                  Track repo
                </button>
              </PermissionGate>
            </div>
          </div>
        </div>
      )}
    </div>
  );
});

const SMALL_BUTTON =
  "inline-flex items-center gap-1 rounded border border-border bg-background px-2 py-1 text-xs hover:bg-accent disabled:opacity-50 disabled:hover:bg-background";

/**
 * The team's starter page (`teams.yml` `landing_page:`): the file it opens when
 * nothing is open. Shows the declared page (or the README default) with Change
 * and Clear — the same registry edit as the file menus' "Set as team starter page".
 */
function StarterPageRow({ team, org }: { team: Team; org: Org }) {
  const declared = team.landing_page?.trim() || null;
  // For the team on screen the page the app really opens is known: show it (a
  // README.html / index.html default, or none), and say when a declared page is
  // missing on disk.
  const active = useActiveTeamStarterPage();
  const resolved =
    active && active.org.slug === org.slug && active.team.name === team.name
      ? active.effectivePath
      : undefined;
  const resolvedRel =
    resolved === undefined
      ? undefined
      : resolved === null
        ? null
        : contentRelativePath(resolved, teamContentRoot(org.org_root, team.name));
  const missing = declared !== null && resolvedRel !== undefined && resolvedRel !== declared;
  const defaultLabel =
    resolvedRel === undefined
      ? "Default (README or index)"
      : resolvedRel === null
        ? "None: the team has no README"
        : `${resolvedRel} (default)`;
  const [draft, setDraft] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const errorId = useId();
  const changeRef = useRef<HTMLButtonElement>(null);
  // Leaving the editor puts focus back on Change…, where it came from.
  const closeDraft = () => {
    setDraft(null);
    requestAnimationFrame(() => changeRef.current?.focus());
  };
  const readOnly = isCodesenseDocs(org.slug);
  const trimmed = draft?.trim() ?? "";
  const draftError =
    draft !== null && trimmed !== "" && !isContentRelative(trimmed)
      ? "Use a path inside the team's content folder, like guide/start.md. Hidden files and .. aren't allowed."
      : null;

  const save = async (page: string | null) => {
    setSaving(true);
    try {
      if (await changeTeamStarterPage(team.name, page, org.slug)) closeDraft();
    } finally {
      setSaving(false);
    }
  };

  return (
    <div data-testid={`settings-starter-page-${team.name}`}>
      <span className="mb-1 block font-medium text-muted-foreground">Starter page</span>
      <div className="flex items-center gap-1.5">
        <House size={12} className="shrink-0 text-muted-foreground" aria-hidden="true" />
        {declared ? (
          <span className="truncate" data-testid="settings-starter-page-current">
            {declared}
          </span>
        ) : (
          <span className="text-muted-foreground" data-testid="settings-starter-page-current">
            {defaultLabel}
          </span>
        )}
      </div>
      {missing && (
        <p className="mt-1 text-destructive" data-testid="settings-starter-page-missing">
          {resolvedRel
            ? `This file isn't there any more, so ${resolvedRel} opens instead.`
            : "This file isn't there any more, so nothing opens."}
        </p>
      )}
      <p className="mt-1 text-muted-foreground">
        {readOnly
          ? `${CODESENSE_DOCS_DISPLAY_NAME} are read-only.`
          : "Opens whenever the team has nothing open. You can also right-click any of the team's files and choose Set as team starter page."}
      </p>
      {!readOnly && (
        <PermissionGate required="any-writer">
          {draft === null ? (
            <div className="mt-2 flex gap-1.5">
              <button
                ref={changeRef}
                type="button"
                className={SMALL_BUTTON}
                data-testid={`settings-starter-page-change-${team.name}`}
                onClick={() => setDraft(declared ?? "")}
              >
                Change…
              </button>
              {declared && (
                <button
                  type="button"
                  className={SMALL_BUTTON}
                  disabled={saving}
                  data-testid={`settings-starter-page-clear-${team.name}`}
                  onClick={() => void save(null)}
                >
                  Clear
                </button>
              )}
            </div>
          ) : (
            <form
              className="mt-2 flex flex-col gap-1"
              onSubmit={(e) => {
                e.preventDefault();
                if (trimmed && !draftError) void save(trimmed);
              }}
            >
              <div className="flex items-center gap-1.5">
                <Input
                  autoFocus
                  aria-label={`Starter page for ${team.name}, relative to its content folder`}
                  placeholder="guide/start.md"
                  className="h-7 rounded px-2 text-xs md:text-xs"
                  value={draft}
                  disabled={saving}
                  aria-invalid={draftError ? true : undefined}
                  aria-describedby={draftError ? errorId : undefined}
                  data-testid={`settings-starter-page-input-${team.name}`}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") closeDraft();
                  }}
                />
                <button
                  type="submit"
                  className={SMALL_BUTTON}
                  disabled={saving || !trimmed || !!draftError}
                  data-testid={`settings-starter-page-save-${team.name}`}
                >
                  {saving ? "Saving…" : "Save"}
                </button>
                <button type="button" className={SMALL_BUTTON} onClick={closeDraft}>
                  Cancel
                </button>
              </div>
              {draftError && (
                <p id={errorId} className="text-destructive">
                  {draftError}
                </p>
              )}
            </form>
          )}
        </PermissionGate>
      )}
    </div>
  );
}

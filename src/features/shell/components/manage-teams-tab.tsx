// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Archive, ArchiveRestore, Check, Plus, Users } from "lucide-react";
import { toast } from "sonner";

import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { AppButton as Button } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { visibleTeams, activeTeams, archivedTeams } from "@/shared/lib/user-teams";
import { teamNameError } from "@/features/shell/lib/team-name";
import { TEAM_EXPLAINER } from "@/shared/lib/onboarding-copy";
import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { userFacingErrorMessage } from "@/shared/lib/error-message";

/**
 * SW-UX (app-flow redesign) — the ONE team-management surface, hosted as a VIRTUAL
 * TAB (`virtual://manage-teams`), never a dialog. It folds which teams show in your
 * switcher + create a team + archive into one home reached from the team switcher.
 *
 * Tab, not modal: team curation is a place you return to. Visibility toggles
 * live-commit (no "Done" — there's no modal to close on); create + archive/restore
 * are inline. Deep per-team config (rename, source repos, members) still lives in
 * Settings → Workspace.
 */
export function ManageTeamsTab() {
  const activeOrg = useOrgStore((s) => s.orgs.find((o) => o.slug === s.activeOrgSlug));
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const shownByOrg = useOrgStore((s) => s.shownTeamsByOrgSlug);
  const setShownTeams = useOrgStore((s) => s.setShownTeams);
  const setTeamArchived = useOrgStore((s) => s.setTeamArchived);
  const intent = useUiStore((s) => s.manageTeamsIntent);
  const setManageTeamsIntent = useUiStore((s) => s.setManageTeamsIntent);

  const createInputRef = useRef<HTMLInputElement>(null);

  // "create" intent focuses the name field (the "Create team" entry points).
  useEffect(() => {
    if (intent === "create") createInputRef.current?.focus();
    if (intent !== null) setManageTeamsIntent(null);
  }, [intent, setManageTeamsIntent]);

  if (!activeOrg) {
    // SW-FU-B7: teams belong to a workspace — with none open, the one action is to
    // open (or create / join) one, never a dead line of text.
    return (
      <div
        data-testid="manage-teams-tab"
        className="flex h-full min-h-0 flex-col items-center justify-center gap-3 bg-background text-sm text-muted-foreground"
      >
        <p>Open a context repo to manage the teams inside it.</p>
        <Button
          size="sm"
          data-testid="manage-teams-open-workspaces"
          onClick={() => openWorkspaces()}
        >
          Open context repos
        </Button>
      </div>
    );
  }

  const shownNames = new Set(visibleTeams(activeOrg, shownByOrg).map((t) => t.name));
  const active = activeTeams(activeOrg);
  const archived = archivedTeams(activeOrg);

  const toggleShown = (name: string) => {
    // Live-commit via the one writer (reconciles the active team if you hide the
    // one you're on). Never hide the LAST shown team out from under the switcher.
    const next = new Set(shownNames);
    if (next.has(name)) {
      if (next.size <= 1) return;
      next.delete(name);
    } else {
      next.add(name);
    }
    setShownTeams(
      activeOrg.slug,
      active.filter((t) => next.has(t.name)).map((t) => t.name),
    );
  };

  const archive = async (name: string, archivedNext: boolean) => {
    try {
      await setTeamArchived(name, archivedNext);
      toast.success(archivedNext ? `Archived ${name}` : `Restored ${name}`);
    } catch (e) {
      toast.error(`Couldn't ${archivedNext ? "archive" : "restore"} ${name}: ${errMsg(e)}`);
    }
  };

  return (
    <div
      data-testid="manage-teams-tab"
      className="flex h-full min-h-0 flex-col overflow-y-auto bg-background text-foreground"
    >
      {/* text-foreground: virtual tabs render in the team pane, which inherits the
          terminal theme's near-white color — re-establish the theme fg. */}
      <div className="mx-auto flex w-full max-w-2xl flex-col p-6">
        <header className="flex flex-col gap-1">
          <h2 className="text-lg font-semibold">Teams · {activeOrg.display_name}</h2>
          <p className="text-sm text-muted-foreground">
            Choose which teams appear in your switcher, create new ones, or archive teams you no
            longer use. Hiding a team is a personal, local view — it never removes your access.
          </p>
        </header>

        <div className="flex flex-col gap-2 py-5">
          <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Teams
          </h3>
          {active.length === 0 ? (
            <p className="text-xs text-muted-foreground">No teams yet. Create your first below.</p>
          ) : (
            <ul className="flex flex-col gap-1.5" data-testid="manage-teams-list">
              {active.map((team) => {
                const shown = shownNames.has(team.name);
                const isActive = team.name === activeTeamName;
                return (
                  <li
                    key={team.name}
                    data-testid={`manage-teams-row-${team.name}`}
                    className="flex items-center gap-3 rounded-md border border-border px-3 py-2"
                  >
                    <button
                      type="button"
                      role="checkbox"
                      aria-checked={shown}
                      aria-label={`Show ${team.name} in the switcher`}
                      data-testid={`manage-teams-shown-${team.name}`}
                      onClick={() => toggleShown(team.name)}
                      className={cn(
                        "flex size-4 shrink-0 items-center justify-center rounded border",
                        shown
                          ? // A checkbox's checked fill, not a hand-rolled button (Rule 2).
                            // eslint-disable-next-line no-restricted-syntax
                            "border-primary bg-primary text-primary-foreground"
                          : "border-border",
                      )}
                    >
                      {shown && <Check size={12} aria-hidden="true" />}
                    </button>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <Users
                          size={14}
                          className="shrink-0 text-muted-foreground"
                          aria-hidden="true"
                        />
                        <span className="truncate text-sm font-medium">{team.name}</span>
                        {isActive && (
                          <span className="text-[11px] text-green-600 dark:text-green-400">
                            · active
                          </span>
                        )}
                      </div>
                    </div>
                    <button
                      type="button"
                      data-testid={`manage-teams-archive-${team.name}`}
                      onClick={() => void archive(team.name, true)}
                      className="flex shrink-0 items-center gap-1 rounded px-2 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                    >
                      <Archive size={12} aria-hidden="true" />
                      Archive
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        {archived.length > 0 && (
          <div className="flex flex-col gap-2 pb-5">
            <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
              Archived
            </h3>
            <ul className="flex flex-col gap-1.5">
              {archived.map((team) => (
                <li
                  key={team.name}
                  data-testid={`manage-teams-archived-row-${team.name}`}
                  className="flex items-center gap-3 rounded-md border border-dashed border-border px-3 py-2 text-muted-foreground"
                >
                  <Users size={14} className="shrink-0" aria-hidden="true" />
                  <span className="min-w-0 flex-1 truncate text-sm">{team.name}</span>
                  <button
                    type="button"
                    data-testid={`manage-teams-restore-${team.name}`}
                    onClick={() => void archive(team.name, false)}
                    className="flex shrink-0 items-center gap-1 rounded px-2 py-1 text-xs hover:bg-accent hover:text-foreground"
                  >
                    <ArchiveRestore size={12} aria-hidden="true" />
                    Restore
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}

        <CreateTeamRow org={activeOrg} inputRef={createInputRef} />
      </div>
    </div>
  );
}

/** Inline "create a team" — the retired CreateTeamDialog's body, always visible at
 *  the bottom of the tab (the "Create team" entry points just focus the field). */
function CreateTeamRow({
  org,
  inputRef,
}: {
  org: { slug: string; teams: { name: string }[] };
  inputRef: React.RefObject<HTMLInputElement | null>;
}) {
  const [name, setName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const existingNames = org.teams.map((t) => t.name);
  const validationError = teamNameError(name, existingNames);
  const canSubmit = !!name.trim() && !submitting && !validationError;

  const create = async () => {
    if (!canSubmit) return;
    setSubmitError(null);
    setSubmitting(true);
    try {
      await useOrgStore.getState().createTeam(name);
      toast.success(`Created team ${name.trim()}`);
      setName("");
    } catch (e) {
      setSubmitError(errMsg(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex flex-col gap-2 border-t border-border pt-5">
      <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Create a team
      </h3>
      <p className="text-xs text-muted-foreground">{TEAM_EXPLAINER}</p>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor="create-team-name">Team name</Label>
        <div className="flex items-start gap-2">
          <div className="flex flex-1 flex-col gap-1">
            <Input
              id="create-team-name"
              ref={inputRef}
              placeholder="platform"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                setSubmitError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") void create();
              }}
              disabled={submitting}
            />
            {(submitError ?? validationError) && (
              <p className="text-xs text-destructive">{submitError ?? validationError}</p>
            )}
          </div>
          <Button onClick={() => void create()} disabled={!canSubmit}>
            <Plus size={14} className="mr-1.5" />
            {submitting ? "Creating…" : "Create team"}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** SW-FU-B8: the reason in words, never the raw thrown value. */
function errMsg(e: unknown): string {
  return userFacingErrorMessage(e);
}

// SPDX-License-Identifier: MIT
import { Check, House, RotateCcw } from "lucide-react";
import { toast } from "sonner";

import { useActiveTeamStarterPage } from "@/features/viewer/hooks/use-team-starter-page";
import { starterPageChoice } from "@/features/viewer/lib/team-starter-page";
import { PositionedMenuItem } from "@/shared/components/positioned-context-menu";
import { userFacingErrorMessage } from "@/shared/lib/error-message";
import { useOrgStore } from "@/shared/stores/org-store";

/** Set or clear the active team's starter page, saying how it went. Resolves
 *  `true` once it is saved. */
export async function changeTeamStarterPage(
  teamName: string,
  rel: string | null,
  orgSlug?: string,
): Promise<boolean> {
  try {
    await useOrgStore.getState().setTeamLandingPage(teamName, rel, orgSlug);
    toast.success(
      rel
        ? `${rel} is now ${teamName}'s starter page`
        : `Cleared ${teamName}'s starter page. It opens its README, if there is one.`,
    );
    return true;
  } catch (e) {
    toast.error(`Couldn't change the starter page: ${userFacingErrorMessage(e)}`);
    return false;
  }
}

/**
 * The file menus' starter page items for `path` (the left nav's file menu and a
 * file tab's menu): "Set as team starter page" on a file of the active team's
 * content; "Team starter page" + "Clear starter page" on the page already set.
 * A file that can't be the page keeps the item, unavailable, with why under it.
 * Renders nothing for a folder or a file outside every team's content.
 */
export function StarterPageMenuItems({
  path,
  onClose,
  isFile,
  separator = "before",
}: {
  path: string;
  onClose: () => void;
  /** The row's real kind, when known (the nav knows it; a tab is a file). */
  isFile?: boolean;
  /** Where the group's divider goes, so it sits between it and its neighbours. */
  separator?: "before" | "after" | "none";
}) {
  const active = useActiveTeamStarterPage();
  if (!active) return null;
  const { org, team, effectivePath } = active;
  // Until the team's page is known, "Set" on its default README would be wrong.
  if (effectivePath === undefined) return null;
  const choice = starterPageChoice({ org, team, path, effectivePath, isFile });
  if (choice.kind === "hidden") return null;

  const divider = <div role="separator" className="my-1 h-px bg-border" />;
  return (
    <>
      {separator === "before" && divider}
      <div role="group" aria-label="Team starter page" data-testid="starter-page-menu">
        {choice.kind === "settable" && (
          <PositionedMenuItem
            className="flex items-center gap-2"
            data-testid="starter-page-set"
            onClick={() => {
              onClose();
              void changeTeamStarterPage(team.name, choice.rel, org.slug);
            }}
          >
            <House size={14} className="flex-shrink-0" aria-hidden="true" />
            Set as team starter page
          </PositionedMenuItem>
        )}
        {choice.kind === "blocked" && (
          <PositionedMenuItem
            className="flex items-start gap-2 text-muted-foreground"
            aria-disabled
            data-focusable-disabled
            data-testid="starter-page-set"
          >
            <House size={14} className="mt-0.5 flex-shrink-0" aria-hidden="true" />
            <span className="min-w-0">
              <span className="block">Set as team starter page</span>
              <span className="block max-w-64 text-xs">{choice.reason}</span>
            </span>
          </PositionedMenuItem>
        )}
        {choice.kind === "current" && (
          <>
            <PositionedMenuItem
              role="menuitemcheckbox"
              aria-checked
              className="flex items-start gap-2 text-muted-foreground"
              aria-disabled
              data-focusable-disabled
              data-testid="starter-page-current"
            >
              <Check size={14} className="mt-0.5 flex-shrink-0" aria-hidden="true" />
              <span className="min-w-0">
                <span className="block text-foreground">Team starter page</span>
                <span className="block max-w-64 text-xs">
                  {choice.declared
                    ? `Opens whenever ${team.name} has nothing open.`
                    : `Opens by default when ${team.name} has nothing open.`}
                </span>
              </span>
            </PositionedMenuItem>
            {choice.declared && (
              <PositionedMenuItem
                className="flex items-start gap-2"
                data-testid="starter-page-clear"
                onClick={() => {
                  onClose();
                  void changeTeamStarterPage(team.name, null, org.slug);
                }}
              >
                <RotateCcw size={14} className="mt-0.5 flex-shrink-0" aria-hidden="true" />
                <span className="min-w-0">
                  <span className="block">Clear starter page</span>
                  <span className="block max-w-64 text-xs text-muted-foreground">
                    Falls back to the README, if there is one.
                  </span>
                </span>
              </PositionedMenuItem>
            )}
          </>
        )}
      </div>
      {separator === "after" && divider}
    </>
  );
}

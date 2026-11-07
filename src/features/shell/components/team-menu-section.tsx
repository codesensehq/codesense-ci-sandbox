// SPDX-License-Identifier: MIT
import { Plus, X, Settings2 } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  MENU_ITEM,
  MENU_ITEM_CURRENT,
  MENU_ROW_ACTION,
  MenuActiveMark,
  MenuLeadIcon,
  MenuSection,
  MenuTreeGuide,
} from "@/shared/components/menu-section";
import { toastFailure } from "@/shared/lib/with-error-toast";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useOrgStore } from "@/shared/stores/org-store";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import type { Org } from "@/shared/types/domain";
import { visibleTeams, resolveActiveTeam } from "@/shared/lib/user-teams";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";
import { switchContextToTeamName } from "@/features/viewer/lib/context-switch";
import {
  scratchTeamKey,
  useScratchTeamUse,
  useScratchTeamUseStore,
} from "@/shared/stores/scratch-team-use-store";

/**
 * SLW-PT-20 — "Use my private items in ‹Team›": the real per-team switch, stored in
 * the user's scratch space and honoured by every backend fold (files, agents,
 * routines). Off means none of your private items apply in this team;
 * your private space itself is untouched.
 */
export function ScratchItemsSwitch({
  orgSlug,
  team,
  inMenu = true,
}: {
  orgSlug: string;
  team: string;
  inMenu?: boolean;
}) {
  const on = useScratchTeamUse(orgSlug, team);
  const saving = useScratchTeamUseStore((s) => s.saving[scratchTeamKey(orgSlug, team)] === true);
  const setOn = useScratchTeamUseStore((s) => s.set);
  // A menu item that toggles in place (the menu stays open); the switch is its picture.
  return (
    <button
      type="button"
      role={inMenu ? "menuitemcheckbox" : "switch"}
      aria-checked={on}
      disabled={saving}
      data-testid="team-switcher-scratch-items"
      title={`Covers ${team}'s work: your private files in its worktrees, your agents in ${team}, and your Only me routines fired by its sessions. Scheduled routines keep their own schedule.`}
      onClick={() => {
        setOn(orgSlug, team, !on).catch((err: unknown) =>
          toastFailure("Couldn't change your private items for this team", err),
        );
      }}
      className={MENU_ITEM}
    >
      {inMenu && <MenuTreeGuide />}
      <span className="min-w-0 flex-1 truncate">Use my private items in {team}</span>
      <span
        aria-hidden="true"
        data-slot="switch"
        className={cn(
          "relative inline-flex h-[14px] w-[24px] shrink-0 items-center rounded-full p-px transition-colors",
          on
            ? // A switch's on fill, not a hand-rolled button (Rule 2).
              // eslint-disable-next-line no-restricted-syntax
              "bg-primary"
            : "bg-input dark:bg-input/80",
        )}
      >
        <span
          className={cn(
            "block size-3 rounded-full bg-background transition-transform",
            on ? "translate-x-[10px] dark:bg-primary-foreground" : "dark:bg-foreground",
          )}
        />
      </span>
    </button>
  );
}

/**
 * The identity menu's Team section: the active org's shown teams (switch on click
 * or Enter; hide one with its row action), the "Use my private items" switch, Browse all teams…, and Create team.
 * SW-OB-7: present whenever the active org has ≥1 shown team, even a single-team
 * org, so Create team stays reachable. Switching changes only what you view.
 */
export function TeamMenuSection({
  org,
  activeTeamName,
  onDone,
}: {
  org: Org;
  activeTeamName: string | null;
  /** Close the hosting menu after a row acts. */
  onDone: () => void;
}) {
  const setShownTeams = useOrgStore((s) => s.setShownTeams);
  // SW-OB-9: subscribe to the shown-set map so the section re-renders when the user
  // curates visibility, and derive the list in render (a `getState()` read inside
  // `visibleTeams` would be invisible to React and leave these rows stale).
  const shownByOrg = useOrgStore((s) => s.shownTeamsByOrgSlug);
  const teamShortcut = useEffectiveShortcut("nav.switch-team");

  const userTeams = visibleTeams(org, shownByOrg);
  const activeTeam = resolveActiveTeam(userTeams, activeTeamName);
  if (!activeTeam) return null;

  const activate = (name: string) => {
    switchContextToTeamName(name, "switcher");
    onDone();
  };

  // SW-OB-9 (AC-4): quick-hide a team from the local shown set through the one
  // `setShownTeams` writer — it reconciles the active team if you hide the one you're
  // on. A local hide only: no access or data change, re-addable via Browse all teams….
  const hideTeam = (name: string) => {
    setShownTeams(
      org.slug,
      userTeams.filter((t) => t.name !== name).map((t) => t.name),
    );
  };

  return (
    <MenuSection
      id="team"
      label={`Teams in ${org.display_name}`}
      shortcut={formatKeystrokeDisplay(teamShortcut)}
    >
      {userTeams.map((team) => {
        const isActive = team.name === activeTeam.name;
        return (
          <div key={team.name} className="group relative flex items-center">
            <button
              type="button"
              role="menuitemradio"
              aria-checked={isActive}
              data-testid={`team-switcher-row-${team.name}`}
              onClick={() => activate(team.name)}
              className={cn(MENU_ITEM, "pr-8", isActive && MENU_ITEM_CURRENT)}
            >
              {/* A guide under the repo's mark: the teams read as its children. */}
              <MenuTreeGuide />
              <span className="truncate">{team.name}</span>
              <MenuActiveMark
                active={isActive}
                className="absolute right-2.5 group-hover:invisible group-has-[[role=menuitem]:focus-visible]:invisible"
              />
            </button>
            {/* Hide from your view: its own item, shown on hover or keyboard focus. */}
            <button
              type="button"
              role="menuitem"
              data-testid={`team-switcher-hide-${team.name}`}
              aria-label={`Hide ${team.name} from your teams`}
              title="Hide from your teams"
              onClick={(e) => {
                e.stopPropagation();
                hideTeam(team.name);
              }}
              className={MENU_ROW_ACTION}
            >
              <X size={12} aria-hidden="true" />
            </button>
          </div>
        );
      })}
      <ScratchItemsSwitch orgSlug={org.slug} team={activeTeam.name} />
      <button
        type="button"
        role="menuitem"
        data-testid="team-switcher-browse-all"
        onClick={() => {
          onDone();
          openManageTeams();
        }}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Settings2 size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Browse all teams…</span>
      </button>
      <button
        type="button"
        role="menuitem"
        data-testid="team-switcher-create"
        onClick={() => {
          onDone();
          openManageTeams("create");
        }}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Plus size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Create team</span>
      </button>
    </MenuSection>
  );
}

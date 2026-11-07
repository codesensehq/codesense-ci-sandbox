// SPDX-License-Identifier: MIT
import { StatusMark } from "@/shared/components/status-mark";
import { useEffect, useId, useRef, useState } from "react";
import {
  Bot,
  ChevronsUpDown,
  Keyboard,
  LayoutDashboard,
  Monitor,
  NotebookPen,
  Plus,
  Repeat,
  RotateCcw,
  Settings,
  X,
} from "lucide-react";

import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { ViewerIdentityFace } from "./viewer-identity-face";
import { cn } from "@/lib/utils";
import {
  MENU_ITEM,
  MENU_ITEM_CURRENT,
  MENU_ROW_ACTION,
  MenuActiveMark,
  MenuLeadIcon,
  MenuLeadSpacer,
  MenuNeedsCount,
  MenuSection,
  MenuSeparator,
  MenuShortcut,
  needsYouText,
} from "@/shared/components/menu-section";
import { menuItems, useMenuRoving } from "@/shared/hooks/use-menu-roving";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { switchContextToOrg } from "@/features/viewer/lib/context-switch";
import { useOrgStore } from "@/shared/stores/org-store";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import type { Org, OrgSummary } from "@/shared/types/domain";
import { visibleTeams, resolveActiveTeam } from "@/shared/lib/user-teams";
import { isCodesenseDocs, orderContextRepos } from "@/shared/lib/codesense-docs";
import {
  ContextRepoFlow,
  IDLE_STEP,
  type ContextRepoStep,
} from "@/features/org/components/context-repo-flow";
import {
  MissingContextRepos,
  useMissingContextRepos,
} from "@/features/org/components/missing-context-repos";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { openRoutines } from "@/features/routines/routines-virtual-tab";
import { useRoutinesStore, watchRoutineCounts } from "@/features/routines/stores/routines-store";
import { openScratchSpace } from "@/features/scratch-space/scratch-space-virtual-tab";
import { openAppSettings } from "@/features/settings/lib/open-settings";
import { UpdateReadyDot } from "@/features/license/UpdateReadyDot";
import { useTeamMachines } from "@/features/machines/stores/machines-store";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { openDashboards } from "@/features/dashboard-publishing/dashboards-virtual-tab";
import { dashboardPublishingEnabled } from "@/features/dashboard-publishing/capability";
import { openMachines } from "../machines-virtual-tab";
import { TeamMenuSection } from "./team-menu-section";
import { ContextRepoMark } from "./context-repo-mark";

/** The section a switch shortcut opens the menu at. */
type MenuSectionId = "context-repo" | "team";

/** Focus where you are in `section` (its checked row), else its first row. A click
 *  (no section) lands on the context repo you're in. */
function focusMenuSection(root: HTMLElement | null, section: MenuSectionId | null): void {
  if (!root) return;
  const scope = root.querySelector(`[data-menu-section="${section ?? "context-repo"}"]`) ?? root;
  const target =
    scope.querySelector<HTMLElement>('[aria-checked="true"]') ??
    menuItems(scope)[0] ??
    menuItems(root)[0];
  target?.focus();
}

/**
 * SW-OC-28 — the identity menu: ONE trigger after the macOS traffic lights naming
 * whose space you're in (`context repo / team ▾`), and one menu holding everything
 * about that space — switch context repo (or connect / create one), switch team,
 * what's yours (Private, agent definitions, routines; agent accounts have no menu row
 * yet, SV-ENG-78), Machines,
 * Settings, and keyboard shortcuts.
 *
 * A click opens it at the top; `⌘⇧G` / `⌘⌥⇧T` open it focused in their section
 * (the ui-store's two section flags). Arrows roam every row, typing jumps,
 * Esc closes. Switching here changes only what you view — it never gates what fires.
 *
 * The trigger carries an amber dot when something inside needs you (routines whose
 * latest run waits on you, agents waiting on the team's machines). The org segment drops out
 * when the title bar is narrow (`@container/titlebar`), so the team name persists.
 */
export function IdentityMenu() {
  const orgs = useOrgStore((s) => s.orgs);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const currentUser = useCurrentUser();
  const inSpace = currentUser !== null && orgs.some((o) => o.slug === activeOrgSlug);
  // The machine registry is per team: read (and load) it only inside a team's space.
  return inSpace ? <IdentityMenuInSpace /> : <IdentityMenuBody machinesNeedYou={0} />;
}

function IdentityMenuInSpace() {
  const machines = useTeamMachines(useActiveTeamKey());
  // Agents waiting on you across the team's listed machines (the old "This Mac ⚑3").
  const waiting = (machines ?? [])
    .filter((m) => !m.binding?.hidden)
    .reduce((sum, m) => sum + m.attention, 0);
  return <IdentityMenuBody machinesNeedYou={waiting} />;
}

function IdentityMenuBody({ machinesNeedYou }: { machinesNeedYou: number }) {
  const orgs = useOrgStore((s) => s.orgs);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const shownByOrg = useOrgStore((s) => s.shownTeamsByOrgSlug);
  const currentUser = useCurrentUser();
  const activeOrg = orgs.find((o) => o.slug === activeOrgSlug) ?? null;
  // SW-OB-7: team visibility is identity-independent; `currentUser` stays only as an
  // auth gate (no team flash before sign-in).
  const activeTeam =
    activeOrg && currentUser
      ? resolveActiveTeam(visibleTeams(activeOrg, shownByOrg), activeTeamName)
      : null;

  const clickOpen = useUiStore((s) => s.identityMenuOpen);
  const orgOpen = useUiStore((s) => s.orgSwitcherOpen);
  const teamOpen = useUiStore((s) => s.teamSwitcherOpen);
  const open = clickOpen || orgOpen || teamOpen;
  const section: MenuSectionId | null = orgOpen ? "context-repo" : teamOpen ? "team" : null;
  const close = () => useUiStore.getState().closeIdentityMenu();
  const setOpen = (next: boolean) =>
    next ? useUiStore.getState().setIdentityMenuOpen(true) : close();

  // SW-OC-16: the add-a-context-repo flow lives here, not in the popover content, so
  // a confirm survives the popover closing around the OS folder picker.
  const [step, setStep] = useState<ContextRepoStep>(IDLE_STEP);
  const flowActive = step.kind !== "idle";
  const missing = useMissingContextRepos(open);
  // The OS picker returned: reopen where it was launched from.
  const reopen = () => useUiStore.getState().setOrgSwitcherOpen(true);

  useEffect(() => watchRoutineCounts(), []);
  const routinesNeedYou = useRoutinesStore((s) => s.needsYouCount);
  const needsYou = routinesNeedYou + machinesNeedYou;

  // Clear the flags on unmount so the menu doesn't reopen unbidden on remount.
  useEffect(() => () => useUiStore.getState().closeIdentityMenu(), []);

  const contentRef = useRef<HTMLDivElement>(null);
  const roving = useMenuRoving();
  // Land focus on open (in `onOpenAutoFocus`: the portaled content mounts a render
  // after `open` flips), and again when a shortcut moves an open menu to a section.
  useEffect(() => {
    if (open) focusMenuSection(contentRef.current, section);
  }, [open, section]);

  const activeIsDocs = activeOrg !== null && isCodesenseDocs(activeOrg.slug);
  const where = activeOrg
    ? [activeOrg.display_name, activeTeam?.name].filter(Boolean).join(" / ")
    : "No context repo";
  const triggerLabel = needsYou > 0 ? `${where} — ${needsYouText(needsYou)}` : where;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          data-testid="identity-menu-trigger"
          data-context-repo={activeIsDocs ? "codesense-docs" : "own"}
          aria-haspopup="menu"
          aria-label={triggerLabel}
          title={triggerLabel}
          className={cn(
            // A quiet chip, not menu-bar text: a hairline edge and a faint fill name it
            // as a control without competing with the tabs beside it.
            "relative flex h-7 min-w-0 max-w-full items-center gap-2 rounded-md border border-border/70 bg-foreground/[0.025] pl-1 pr-1.5 text-xs text-muted-foreground transition-colors duration-150 hover:border-border hover:bg-foreground/[0.07] hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            open && "border-border bg-foreground/10 text-foreground",
          )}
        >
          {activeOrg ? (
            <>
              <ContextRepoMark slug={activeOrg.slug} name={activeOrg.display_name} size={18} />
              <span className="flex min-w-0 items-center gap-1">
                <span
                  data-testid="identity-menu-org"
                  className={cn(
                    "min-w-0 truncate",
                    // Narrow bar: the team alone names the space (the org is in the menu).
                    activeTeam && "hidden @[75rem]/titlebar:inline",
                  )}
                >
                  {activeOrg.display_name}
                </span>
                {activeTeam && (
                  <>
                    <span
                      aria-hidden="true"
                      className="hidden text-muted-foreground/50 @[75rem]/titlebar:inline"
                    >
                      /
                    </span>
                    <span
                      data-testid="identity-menu-team"
                      className="min-w-0 truncate font-medium text-foreground"
                    >
                      {activeTeam.name}
                    </span>
                  </>
                )}
              </span>
            </>
          ) : (
            <>
              <span
                aria-hidden="true"
                className="flex size-[18px] shrink-0 items-center justify-center rounded-[5px] border border-dashed border-muted-foreground/50"
              >
                <Plus size={11} />
              </span>
              <span className="truncate">No context repo</span>
            </>
          )}
          {needsYou > 0 && (
            // The shared needs-you aggregate glyph (the `question` mark
            // stands in for the three distinct needs kinds — question / permission /
            // blocked — at the aggregate level, where one count rolls them up). Amber,
            // never alarm-red: "you have items" is not an alarm. Inside the chip, so
            // it never rides the chip's edge.
            <StatusMark data-testid="identity-menu-attention" mark="question" size="sm" />
          )}
          <ChevronsUpDown className="shrink-0 opacity-60" size={13} aria-hidden="true" />
        </button>
      </PopoverTrigger>
      {flowActive ? (
        <PopoverContent
          align="start"
          side="bottom"
          role="dialog"
          aria-label="Add a context repo"
          className="w-80 gap-0 p-1"
          data-testid="identity-menu-content"
        >
          <ContextRepoFlow step={step} setStep={setStep} onDone={close} onPickerSettled={reopen} />
        </PopoverContent>
      ) : (
        <PopoverContent
          ref={contentRef}
          align="start"
          side="bottom"
          role="menu"
          aria-label="Context repo, team, and your things"
          className="max-h-[min(80vh,44rem)] w-[18.5rem] gap-0 overflow-y-auto p-1.5 pb-0"
          data-testid="identity-menu-content"
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            focusMenuSection(contentRef.current, section);
          }}
          onKeyDown={roving.onKeyDown}
          onPointerMove={roving.onPointerMove}
        >
          <ContextRepoSection
            orgs={orgs}
            activeOrg={activeOrg}
            missing={missing}
            footer={
              <ContextRepoFlow
                step={step}
                setStep={setStep}
                onDone={close}
                onPickerSettled={reopen}
              />
            }
            onPickerSettled={reopen}
            onDone={close}
          />
          {/* No separator: the teams hang off the active repo above them. */}
          {activeOrg && currentUser && activeTeam && (
            <>
              <TeamMenuSection org={activeOrg} activeTeamName={activeTeamName} onDone={close} />
            </>
          )}
          <MenuSeparator />
          <YouSection routinesNeedYou={routinesNeedYou} onDone={close} />
          <AppSection machinesNeedYou={machinesNeedYou} onDone={close} />
        </PopoverContent>
      )}
    </Popover>
  );
}

/**
 * Context repo: every connected one (the bundled CodeSense docs pinned first,
 * read-only; the rest alphabetical), the disabled ones (one click back), any whose
 * folder went missing, and always Connect a context repo… / Create a new context
 * repo… (SW-OC-16). The active one is marked in place, never hoisted: reordering on
 * pick makes the row you just clicked jump away.
 */
function ContextRepoSection({
  orgs,
  activeOrg,
  missing,
  footer,
  onPickerSettled,
  onDone,
}: {
  orgs: Org[];
  activeOrg: Org | null;
  missing: OrgSummary[];
  footer: React.ReactNode;
  onPickerSettled: () => void;
  onDone: () => void;
}) {
  const disabledOrgSlugs = useOrgStore((s) => s.disabledOrgSlugs);
  const setOrgDisabled = useOrgStore((s) => s.setOrgDisabled);
  const shortcut = useEffectiveShortcut("nav.switch-org");
  const disabledHeadingId = useId();

  const disabled = new Set(disabledOrgSlugs);
  const enabled = activeOrg ? orderContextRepos(orgs.filter((o) => !disabled.has(o.slug))) : [];
  const disabledOrgs = activeOrg ? orderContextRepos(orgs.filter((o) => disabled.has(o.slug))) : [];
  // The ≥1 invariant: only offer "disable" while more than one stays enabled.
  const canDisable = enabled.length > 1;

  const activate = (slug: string) => {
    if (slug !== activeOrg?.slug) switchContextToOrg(slug, "switcher");
    onDone();
  };

  return (
    <MenuSection
      id="context-repo"
      label="Context repos"
      shortcut={formatKeystrokeDisplay(shortcut)}
    >
      {enabled.map((org) => {
        const isActive = org.slug === activeOrg?.slug;
        const isDocs = isCodesenseDocs(org.slug);
        const disableable = canDisable && !isDocs;
        return (
          <div key={org.slug} className="group relative flex items-center">
            <button
              type="button"
              role="menuitemradio"
              aria-checked={isActive}
              data-testid={`org-switcher-row-${org.slug}`}
              onClick={() => activate(org.slug)}
              className={cn(MENU_ITEM, isActive && MENU_ITEM_CURRENT, disableable && "pr-8")}
            >
              <ContextRepoMark slug={org.slug} name={org.display_name} size={20} />
              <span className="truncate">{org.display_name}</span>
              {isDocs && (
                <span className="ml-auto shrink-0 rounded border border-border/70 px-1 py-px text-[10.5px] font-normal leading-tight text-muted-foreground">
                  Read-only
                </span>
              )}
              <MenuActiveMark
                active={isActive}
                className={cn(
                  isDocs && "ml-1.5",
                  disableable &&
                    "absolute right-2.5 group-hover:invisible group-has-[[role=menuitem]:focus-visible]:invisible",
                )}
              />
            </button>
            {/* Disable (soft-remove): never the pinned docs, and only while more than one
                stays enabled. Its own item, shown on hover or keyboard focus. */}
            {disableable && (
              <button
                type="button"
                role="menuitem"
                data-testid={`org-switcher-disable-${org.slug}`}
                aria-label={`Disable ${org.display_name}`}
                title="Disable"
                onClick={(e) => {
                  e.stopPropagation();
                  setOrgDisabled(org.slug, true);
                }}
                className={MENU_ROW_ACTION}
              >
                <X size={12} aria-hidden="true" />
              </button>
            )}
          </div>
        );
      })}

      {disabledOrgs.length > 0 && (
        <div role="group" aria-labelledby={disabledHeadingId} className="flex flex-col">
          <p
            id={disabledHeadingId}
            role="presentation"
            className="px-2 pb-0.5 pt-1.5 text-[11px] text-muted-foreground"
          >
            Disabled
          </p>
          {disabledOrgs.map((org) => (
            <button
              key={org.slug}
              type="button"
              role="menuitem"
              data-testid={`org-switcher-enable-${org.slug}`}
              aria-label={`Enable ${org.display_name}`}
              onClick={() => setOrgDisabled(org.slug, false)}
              className={cn(MENU_ITEM, "text-muted-foreground")}
            >
              <MenuLeadSpacer />
              <span className="min-w-0 flex-1 truncate">{org.display_name}</span>
              <span className="flex shrink-0 items-center gap-1 text-xs">
                <RotateCcw size={12} aria-hidden="true" />
                Enable
              </span>
            </button>
          ))}
        </div>
      )}
      <MissingContextRepos rows={missing} onPickerSettled={onPickerSettled} itemRole="menuitem" />
      {footer}
    </MenuSection>
  );
}

/** What's yours in this space: Private, agent definitions, routines (with how many
 *  wait on you). Agent accounts have no menu row yet (SV-ENG-78). */
function YouSection({ routinesNeedYou, onDone }: { routinesNeedYou: number; onDone: () => void }) {
  const go = (open: () => void) => () => {
    onDone();
    open();
  };
  // Routines live in the Agents tab (SPEC §10): this opens Agents → Schedule.
  const routinesLabel =
    routinesNeedYou > 0 ? `Schedule — ${needsYouText(routinesNeedYou)}` : "Schedule";
  return (
    <MenuSection id="you" label="You" heading={<YouHeading />}>
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-private"
        onClick={go(() => openScratchSpace())}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <NotebookPen size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Private</span>
        <span className="ml-auto shrink-0 text-[11px] text-muted-foreground">Only you see it</span>
      </button>
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-agents"
        onClick={go(() => openAgents())}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Bot size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Agent definitions</span>
      </button>
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-routines"
        aria-label={routinesLabel}
        onClick={go(() => openRoutines())}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Repeat size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Schedule</span>
        <MenuNeedsCount count={routinesNeedYou} testId="identity-menu-routines-count" />
      </button>
    </MenuSection>
  );
}

/** The You group's heading: who "you" is, by face and name. */
function YouHeading() {
  const currentUser = useCurrentUser();
  if (!currentUser) return <>You</>;
  return (
    <>
      <ViewerIdentityFace size={14} className="shrink-0" />
      <span className="truncate">{currentUser.display_name}</span>
    </>
  );
}

/** App-level destinations, each with the shortcut that reaches it directly. */
function AppSection({ machinesNeedYou, onDone }: { machinesNeedYou: number; onDone: () => void }) {
  const machinesKey = formatKeystrokeDisplay(useEffectiveShortcut("nav.open-machines"));
  const settingsKey = formatKeystrokeDisplay(useEffectiveShortcut("settings.open"));
  const go = (open: () => void) => () => {
    onDone();
    open();
  };
  return (
    <div
      role="group"
      aria-label="App"
      // The app's own destinations sit in a footer band, apart from your space.
      className="-mx-1.5 mt-1.5 flex flex-col border-t border-border/80 bg-foreground/[0.025] px-1.5 py-1.5"
    >
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-machines"
        aria-label={machinesNeedYou > 0 ? `Machines — ${needsYouText(machinesNeedYou)}` : undefined}
        onClick={go(openMachines)}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Monitor size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Machines</span>
        <MenuNeedsCount count={machinesNeedYou} testId="identity-menu-machines-count" />
        <MenuShortcut className={machinesNeedYou > 0 ? "ml-2" : undefined}>
          {machinesKey}
        </MenuShortcut>
      </button>
      {dashboardPublishingEnabled() && (
        <button
          type="button"
          role="menuitem"
          data-testid="identity-menu-dashboards"
          onClick={go(openDashboards)}
          className={MENU_ITEM}
        >
          <MenuLeadIcon>
            <LayoutDashboard size={15} strokeWidth={1.75} />
          </MenuLeadIcon>
          <span>Dashboards</span>
        </button>
      )}
      {/* SW-OC-B7: Settings stays discoverable — a visible row with its ⌘, beside it. */}
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-settings"
        onClick={go(() => openAppSettings("general"))}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Settings size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Settings</span>
        <UpdateReadyDot />
        <MenuShortcut>{settingsKey}</MenuShortcut>
      </button>
      <button
        type="button"
        role="menuitem"
        data-testid="identity-menu-shortcuts"
        onClick={go(() => openAppSettings("shortcuts"))}
        className={MENU_ITEM}
      >
        <MenuLeadIcon>
          <Keyboard size={15} strokeWidth={1.75} />
        </MenuLeadIcon>
        <span>Keyboard shortcuts</span>
      </button>
    </div>
  );
}

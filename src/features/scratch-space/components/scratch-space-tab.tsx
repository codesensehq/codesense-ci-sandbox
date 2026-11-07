// SPDX-License-Identifier: MIT
import { toast } from "sonner";
import { useNow } from "@/shared/hooks/use-now";
import { elideMiddle, agoText } from "@/features/git-sync/lib/context-repo-status";
import { type ReactNode } from "react";
import {
  Globe,
  Lock,
  Folder,
  PanelLeft,
  RefreshCw,
  StickyNote,
  LayoutDashboard,
  Repeat,
  Bot,
  type LucideIcon,
} from "lucide-react";
import { InlineAlert } from "@/components/ui/inline-alert";
import { AppButton as Button } from "@/shared/components/app-button";
import { basename } from "@/shared/lib/basename";
import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useActiveTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import {
  revealPath,
  openPanelsMenu,
  type ScratchAllOrgsFile,
  type ScratchInertKind,
} from "@/tauri/commands";
import { ScratchItemsSwitch } from "@/features/shell/components/team-menu-section";
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { openRoutines } from "@/features/routines/routines-virtual-tab";
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { useScratchSpace } from "../lib/use-scratch-space";
import { ScratchItemMenu } from "./scratch-item-actions";

const KIND_LABEL: Record<ScratchInertKind, string> = {
  skill: "Skill",
  prompt: "Prompt",
  snippet: "Snippet",
  template: "Template",
};
const NOT_APPLIED = "Not used — only skills, prompts, snippets and templates apply";

export function ScratchSpaceTab(_props: CommonViewerProps & { filePath: string }) {
  const orgSlug = useOrgStore((s) => s.activeOrgSlug);
  const orgs = useOrgStore((s) => s.orgs);
  const teamName = useOrgStore((s) => s.activeTeamName);
  const teamKey = useActiveTeamKey();
  const hasTeam = teamKeyHasTeam(teamKey);
  const org = orgs.find((o) => o.slug === orgSlug);
  const profile = useLocalProfileStore((s) => s.status?.active);
  const initialized = useLocalProfileStore((s) => s.initialized);
  const { state, reload } = useScratchSpace(orgSlug);
  const signedOut = initialized && !profile;
  const now = useNow(!signedOut, 60_000);
  const showNav = () => {
    if (!hasTeam || signedOut) return;
    const v = useViewerStore.getState();
    v.setNavLens(teamKey, { source: { kind: "private" } });
    v.setNavCollapsed(teamKey, false);
    useUiStore.setState((s) => ({ leftNavSearchFocusNonce: s.leftNavSearchFocusNonce + 1 }));
  };
  return (
    <div
      data-testid="scratch-space-tab"
      className="h-full min-h-0 overflow-y-auto bg-background text-foreground"
    >
      <div className="mx-auto flex max-w-3xl flex-col gap-6 px-6 py-6">
        <header className="flex flex-wrap items-start gap-x-4 gap-y-3">
          <div className="min-w-64 flex-1">
            <h1 className="flex items-center gap-2 text-lg font-semibold">
              <Lock size={16} aria-hidden />
              Private
            </h1>
            <p data-testid="scratch-space-about" className="mt-1 text-sm text-muted-foreground">
              Only you see it. One git repo on this Mac, with a folder for each org. CodeSense never
              copies it anywhere; you back it up.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button
              size="sm"
              onClick={showNav}
              disabled={!hasTeam || signedOut}
              title={!hasTeam ? "Open a team first." : undefined}
            >
              <PanelLeft aria-hidden />
              Show in left nav
            </Button>
            <Button size="sm" variant="outline" onClick={reload} aria-label="Refresh">
              <RefreshCw aria-hidden />
            </Button>
          </div>
        </header>
        {!hasTeam && <p className="text-sm text-muted-foreground">Open a team first.</p>}
        {signedOut ? (
          <p>Sign in to a profile to have a private space.</p>
        ) : state.kind === "loading" ? (
          <p className="text-sm text-muted-foreground">Loading Private…</p>
        ) : state.kind === "error" ? (
          <InlineAlert
            data-testid="scratch-space-error"
            severity="error"
            title="Couldn't read Private"
            action={{ label: "Try again", onClick: reload }}
          >
            {state.message}
          </InlineAlert>
        ) : (
          (() => {
            const { listing, errors } = state.contents;
            const management = listing.management;
            const repo = management?.repo;
            const active = management?.orgs.find((o) => o.org === orgSlug);
            const counts = active?.counts;
            const summaries = [
              {
                label: "Notes",
                count: counts?.notes ?? state.contents.notes.length,
                icon: StickyNote,
                action: "Open notes",
                open: () => revealSlot("right-pane", { surface: "notes" }),
              },
              {
                label: "Panes",
                count: counts?.panes ?? 0,
                icon: LayoutDashboard,
                action: "Open panes",
                open: () => void openPanelsMenu().catch((e) => toast.error(String(e))),
              },
              {
                label: "Routines",
                count: counts?.routines ?? state.contents.routines.length,
                icon: Repeat,
                action: "Open routines",
                open: () => openRoutines(),
              },
              {
                label: "Agents",
                count: counts?.agents ?? state.contents.agents.length,
                icon: Bot,
                action: "Open agents",
                open: () => openAgents(),
              },
            ];
            return (
              <>
                <Group
                  title="Your private repo"
                  icon={Folder}
                  count={1}
                  empty="Nothing private yet"
                >
                  <li className="flex min-w-0 items-center gap-2 text-sm">
                    <Folder size={14} aria-hidden />
                    <code title={repo?.path} className="min-w-0 flex-1 truncate">
                      {repo
                        ? elideMiddle(repo.path, 70)
                        : "Private repo is created when you add your first item."}
                    </code>
                    {repo && (
                      <Button size="sm" variant="ghost" onClick={() => void revealPath(repo.path)}>
                        Reveal
                      </Button>
                    )}
                  </li>
                  <li className="text-sm text-muted-foreground">
                    {repo?.error
                      ? `Couldn't read its git state: ${repo.error}`
                      : `${management?.orgs.some((o) => o.tracked) ? `${repo?.uncommitted ?? 0} uncommitted edits` : "Not tracked yet"}${repo?.lastCommitAt ? ` · last commit ${agoText(repo.lastCommitAt * 1000, now)}` : ""} · local only`}
                  </li>
                </Group>
                <Group
                  title="Orgs"
                  icon={Folder}
                  count={management?.orgs.filter((o) => o.exists).length ?? 0}
                  empty="Nothing private yet"
                >
                  {management?.orgs
                    .filter((o) => o.exists)
                    .map((o) => (
                      <li key={o.org} className="flex flex-col gap-2 py-1">
                        <div className="flex flex-wrap items-center gap-2 text-sm">
                          <span className="font-medium">
                            {orgs.find((org) => org.slug === o.org)?.display_name ?? o.org}
                          </span>
                          {o.org === orgSlug && (
                            <span className="rounded border border-border px-1.5 text-xs text-muted-foreground">
                              Open
                            </span>
                          )}
                          <span className="ml-auto text-xs tabular-nums text-muted-foreground">
                            {Object.entries({
                              files: o.counts.overlayFiles,
                              notes: o.counts.notes,
                              panes: o.counts.panes,
                              routines: o.counts.routines,
                              agents: o.counts.agents,
                            })
                              .filter(([, n]) => n > 0)
                              .map(([label, n]) => `${n} ${label}`)
                              .join(" · ")}
                            {o.uncommitted > 0 && ` · ${o.uncommitted} edits`}
                          </span>
                        </div>
                        {o.settingsDamaged && (
                          <InlineAlert
                            severity="error"
                            title="Your per-team choices couldn't be read."
                          />
                        )}
                        {o.org === orgSlug
                          ? org?.teams
                              .filter((t) => !t.archived)
                              .map((t) => (
                                <ScratchItemsSwitch
                                  key={t.name}
                                  orgSlug={o.org}
                                  team={t.name}
                                  inMenu={false}
                                />
                              ))
                          : o.offTeams.length > 0 && (
                              <p className="text-xs text-muted-foreground">
                                Private items off in: {o.offTeams.join(", ")}
                              </p>
                            )}
                      </li>
                    ))}
                </Group>
                {orgSlug ? (
                  <Group
                    title={`Also in ${org?.display_name ?? orgSlug}`}
                    icon={Lock}
                    count={4}
                    empty="Nothing private yet"
                  >
                    {summaries.map((row) => (
                      <Row
                        key={row.label}
                        icon={row.icon}
                        primary={row.count ? row.label : `No ${row.label.toLowerCase()} yet`}
                        secondary={String(row.count)}
                        onOpen={row.open}
                        actions={
                          <Button size="sm" variant="ghost" onClick={row.open}>
                            {row.action}
                          </Button>
                        }
                      />
                    ))}
                    {Object.entries(errors).map(([kind, error]) => (
                      <li key={kind} className="text-sm text-destructive">
                        Couldn't read your {kind}: {error}
                      </li>
                    ))}
                  </Group>
                ) : (
                  <p data-testid="scratch-space-no-org" className="text-sm text-muted-foreground">
                    Open an org to see what you keep for it.
                  </p>
                )}
                <AllOrgsGroup
                  items={listing.allOrgs}
                  orgSlug={orgSlug}
                  teamName={teamName}
                  onChanged={reload}
                />
              </>
            );
          })()
        )}
      </div>
    </div>
  );
}

function AllOrgsGroup({
  items,
  orgSlug,
  teamName,
  onChanged,
}: {
  items: ScratchAllOrgsFile[];
  orgSlug: string | null;
  teamName: string | null;
  onChanged: () => void;
}) {
  return (
    <Group
      title="All orgs"
      icon={Globe}
      count={items.length}
      note="Skills, prompts, snippets and templates here apply in every org."
      empty="Nothing applies in every org yet."
    >
      {items.map((f) => (
        <Row
          key={f.path}
          icon={Globe}
          primary={basename(f.logical)}
          secondary={f.kind ? f.logical : NOT_APPLIED}
          tag={f.kind ? KIND_LABEL[f.kind] : undefined}
          muted={!f.kind}
          onOpen={() => openManagementTab(f.path)}
          actions={
            <ScratchItemMenu
              item={{
                orgSlug,
                logical: f.logical,
                where: "allOrgs",
                inertKind: f.kind,
                defaultTeam: teamName,
              }}
              onDone={onChanged}
            />
          }
        />
      ))}
    </Group>
  );
}

function Group({
  title,
  icon: Icon,
  count,
  note,
  empty,
  error,
  children,
}: {
  title: string;
  icon: LucideIcon;
  count: number;
  note?: string;
  empty: string;
  /** Why this list couldn't be read (the rest of the tab still shows). */
  error?: string;
  children: ReactNode;
}) {
  return (
    <section aria-label={title} className="flex flex-col gap-1">
      <h2 className="flex items-center gap-1.5 px-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        <Icon size={12} aria-hidden="true" />
        {title}
        {count > 0 && <span className="font-normal tabular-nums">{count}</span>}
      </h2>
      {note && (
        <p data-testid="scratch-space-group-note" className="px-1 text-xs text-muted-foreground">
          {note}
        </p>
      )}
      {error ? (
        <p data-testid="scratch-space-section-error" className="px-1 text-sm text-destructive">
          Couldn&apos;t read your {title.toLowerCase()}: {error}
        </p>
      ) : count === 0 ? (
        <p className="px-1 text-sm text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex flex-col">{children}</ul>
      )}
    </section>
  );
}

function Row({
  icon: Icon,
  primary,
  secondary,
  tag,
  muted = false,
  onOpen,
  actions,
}: {
  icon: LucideIcon;
  primary: string;
  secondary?: string;
  tag?: string;
  muted?: boolean;
  onOpen: () => void;
  /** The row's own menu (Share, Copy, Remove…), at its end. */
  actions?: ReactNode;
}) {
  return (
    <li className="group/row flex items-center gap-0.5">
      <button
        type="button"
        onClick={onOpen}
        className="group flex min-w-0 flex-1 items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Icon
          size={14}
          aria-hidden="true"
          className="shrink-0 text-muted-foreground group-hover:text-foreground"
        />
        <span className={muted ? "truncate text-muted-foreground" : "truncate font-medium"}>
          {primary}
        </span>
        {tag && (
          <span className="shrink-0 rounded border border-border px-1.5 text-[0.7rem] leading-4 text-muted-foreground">
            {tag}
          </span>
        )}
        {secondary && (
          <span className="ml-auto min-w-0 truncate pl-3 text-xs text-muted-foreground">
            {secondary}
          </span>
        )}
      </button>
      {actions}
    </li>
  );
}

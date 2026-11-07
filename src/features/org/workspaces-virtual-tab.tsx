// SPDX-License-Identifier: MIT
import { BookOpen, Building2, Check, X } from "lucide-react";

import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { switchContextToOrg } from "@/features/viewer/lib/context-switch";

import { isCodesenseDocs, orderContextRepos } from "@/shared/lib/codesense-docs";

import { ContextAttachments } from "./components/context-attachments";
import { WORKSPACES_VIRTUAL_PATH } from "./lib/open-workspaces";

/** Connected context repos and an in-place connect / create form. */
export { WORKSPACES_VIRTUAL_PATH } from "./lib/open-workspaces";

/**
 * The tab that lists your context repos (the top level; SW-OC-16 names it "context
 * repo" everywhere in the UI) and connects or creates one.
 */
export const YOUR_TEAMS_TITLE = "Context repos";

export function ManageWorkspacesTab() {
  const orgs = useOrgStore((s) => s.orgs);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const disabledOrgSlugs = useOrgStore((s) => s.disabledOrgSlugs);
  const setOrgDisabled = useOrgStore((s) => s.setOrgDisabled);
  // The requested entry intent: `create` opens the create form; `home`/`join` the actions.
  const screen = useUiStore((s) => s.workspacesScreen);
  const setScreen = useUiStore((s) => s.setWorkspacesScreen);

  const disabled = new Set(disabledOrgSlugs);
  // The CodeSense docs pinned first, then alphabetical. The active one is marked in place
  // by the "open" badge, never hoisted: reordering a list under the cursor when you select
  // an item is disorienting (the row you clicked jumps away).
  const enabled = orderContextRepos(orgs.filter((o) => !disabled.has(o.slug)));
  const disabledOrgs = orderContextRepos(orgs.filter((o) => disabled.has(o.slug)));
  const canDisable = enabled.length > 1;

  return (
    <div
      data-testid="workspaces-tab"
      className="h-full min-h-0 overflow-y-auto overflow-x-hidden bg-background text-foreground"
    >
      {/* text-foreground: virtual tabs render in the team pane, which inherits the
          terminal theme's near-white color — re-establish the theme fg.
          SW-LN-B49: ONE column — the create/join form stacks below the list instead
          of squeezing into a side column next to the right pane. */}
      <div className="mx-auto flex w-full min-w-0 max-w-2xl flex-col p-6">
        <header className="flex flex-col gap-1">
          <h2 className="text-lg font-semibold">{YOUR_TEAMS_TITLE}</h2>
          <p className="text-sm text-muted-foreground">
            Connect your team's context repo, or create a new one.
          </p>
        </header>

        <div className="flex flex-col gap-1.5 py-5">
          {enabled.length === 0 ? (
            <p className="text-xs text-muted-foreground">
              No context repos yet. Connect one or create a new one.
            </p>
          ) : (
            <ul className="flex flex-col gap-1.5" data-testid="workspaces-list">
              {enabled.map((org) => {
                const isActive = org.slug === activeOrgSlug;
                return (
                  <li
                    key={org.slug}
                    data-testid={`workspaces-row-${org.slug}`}
                    className="group flex items-center gap-3 rounded-md border border-border px-3 py-2 hover:bg-accent hover:text-accent-foreground"
                  >
                    <button
                      type="button"
                      onClick={() => switchContextToOrg(org.slug, "nav")}
                      className="flex min-w-0 flex-1 items-center gap-2 text-left"
                    >
                      {isCodesenseDocs(org.slug) ? (
                        <BookOpen
                          size={16}
                          className="shrink-0 text-muted-foreground"
                          aria-hidden="true"
                        />
                      ) : (
                        <Building2
                          size={16}
                          className="shrink-0 text-muted-foreground"
                          aria-hidden="true"
                        />
                      )}
                      <span className="truncate text-sm font-medium">{org.display_name}</span>
                      {isActive && (
                        <span className="inline-flex items-center gap-1 text-[11px] text-green-600 dark:text-green-400">
                          <Check size={12} aria-hidden="true" /> open
                        </span>
                      )}
                    </button>
                    {canDisable && !isCodesenseDocs(org.slug) && (
                      <button
                        type="button"
                        aria-label={`Disable ${org.display_name}`}
                        data-testid={`workspaces-disable-${org.slug}`}
                        onClick={() => setOrgDisabled(org.slug, true)}
                        className="shrink-0 rounded p-1 text-muted-foreground opacity-0 hover:bg-background hover:text-foreground focus:opacity-100 group-hover:opacity-100"
                      >
                        <X size={12} aria-hidden="true" />
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          )}

          {disabledOrgs.length > 0 && (
            <>
              <p className="mt-2 px-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                Disabled
              </p>
              {disabledOrgs.map((org) => (
                <div
                  key={org.slug}
                  className="flex items-center gap-3 rounded-md border border-dashed border-border px-3 py-2 text-muted-foreground"
                >
                  <Building2 size={16} className="shrink-0" aria-hidden="true" />
                  <span className="min-w-0 flex-1 truncate text-sm">{org.display_name}</span>
                  <button
                    type="button"
                    data-testid={`workspaces-enable-${org.slug}`}
                    onClick={() => setOrgDisabled(org.slug, false)}
                    className="shrink-0 rounded px-2 py-1 text-xs hover:bg-accent hover:text-foreground"
                  >
                    Enable
                  </button>
                </div>
              ))}
            </>
          )}
        </div>

        <div className="mt-2 border-t border-border pt-3">
          <ContextAttachments
            key={screen}
            initialCreate={screen === "create"}
            onAttached={() => setScreen("home")}
          />
        </div>
      </div>
    </div>
  );
}

let registered = false;

/** Register the Context repos surface under its virtual path. Idempotent. */
export function registerWorkspacesTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(WORKSPACES_VIRTUAL_PATH, YOUR_TEAMS_TITLE, ManageWorkspacesTab);
}

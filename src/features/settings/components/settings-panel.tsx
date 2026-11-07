// SPDX-License-Identifier: MIT
import { AppearanceSection } from "./appearance-section";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Search } from "lucide-react";

import { useFocusWhenShown, useOwningScope } from "@/shared/lib/warm-scopes";
import { useUiStore } from "@/shared/stores/ui-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useOrgStore } from "@/shared/stores/org-store";
import { WorkspaceSection } from "@/features/settings/components/workspace-section";
import type { SectionKey } from "@/features/settings/lib/types";

import { AccountSection } from "./account-section";
import { DashboardsSection } from "./dashboards-section";
import { GeneralSection } from "./general-section";
import { IdentitySection } from "./identity-section";
import { PhasesSection } from "./phases-section/phases-section";
import { ProjectionExcludesSection } from "./projection-excludes-section";
import { RepoSection } from "./repo-section";
import { ShortcutsSection } from "./shortcuts-section";
import { SettingsSidebar, SECTION_NAV_ITEMS, sectionHasContent } from "./settings-sidebar";

/**
 * App-wide Settings panel — the body of the `virtual://settings` tab. Two-column
 * layout: persistent sidebar nav on the left, scrollable content area on the
 * right. As a tab it has no Back button, no global Escape-to-close and no focus
 * restore — the tab's own close (X / ⌘W) returns you.
 */
export function SettingsPanel() {
  const initialSection = useUiStore((s) => s.settingsInitialSection);
  // Each open is a fresh request, so it jumps even when the section asked for is unchanged.
  const jump = useUiStore((s) => s.settingsJump);
  const configRepos = useConfigStore((s) => s.repos);
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const { repoPath: activeRepoPath, teamKey } = useActiveWorkspaceContext();

  // T.6: resolve the active RepoState via the focused workspace's
  // synthetic `repoPath` key (NOT `workspace.sourceRepoPath`, which is
  // the M.2 on-disk clone path and would miss every `workspaceRepos`
  // lookup). Fallback chain:
  //   1. activeRepoPath → workspace-store match (synthetic workspace repos)
  //   2. activeRepoPath → config-store match (legacy local repos)
  //   3. first workspace-store entry
  //   4. first config-store entry
  //   5. null (no repos configured — empty state)
  const repo =
    (activeRepoPath
      ? (workspaceRepos[activeRepoPath] ?? configRepos.find((r) => r.repoPath === activeRepoPath))
      : undefined) ??
    Object.values(workspaceRepos)[0] ??
    configRepos[0] ??
    null;

  const searchInputRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const [query, setQuery] = useState("");
  const [activeSection, setActiveSection] = useState<SectionKey>(initialSection ?? "general");
  const [collapsed, setCollapsed] = useState(false);

  // Ref tracks activeSection for the auto-switch effect without
  // re-triggering it — avoids double-render and prevents auto-switch
  // from overriding explicit sidebar clicks. `useLayoutEffect` (not
  // `useEffect`) so the ref is updated SYNCHRONOUSLY after DOM commit
  // and BEFORE any subsequent `useEffect` (including the query
  // auto-switch effect below) runs. With a plain `useEffect` there
  // would be a one-render window where back-to-back changes to
  // `activeSection` + `query` in the same tick could let the query
  // effect read a stale ref. Updating during render is what the
  // refs lint correctly flags — useLayoutEffect is the right seam.
  const activeSectionRef = useRef(activeSection);
  useLayoutEffect(() => {
    activeSectionRef.current = activeSection;
  }, [activeSection]);

  // Sync activeSection when openAppSettings is re-invoked while open.
  // Also clears stale search query so auto-switch doesn't immediately
  // override the requested section. Set-state-in-effect is intentional:
  // `initialSection` is a request to jump to a section from outside the
  // component, and the local state must be reset to honor it. The user
  // can also click sections manually after, so this isn't a pure
  // derived value.
  useEffect(() => {
    if (initialSection) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setActiveSection(initialSection);
      setQuery("");
    }
  }, [initialSection, jump]);

  // Search auto-switch: if the active section's nav spec doesn't match
  // the query, jump to the first matching section. Uses ref to read
  // activeSection without including it in deps (prevents sidebar clicks
  // from re-triggering auto-switch).
  useEffect(() => {
    if (!query.trim()) return;
    const current = activeSectionRef.current;
    if (sectionHasContent(current, query)) {
      return; // current section still has matching rows
    }
    const first = SECTION_NAV_ITEMS.find((n) => sectionHasContent(n.key, query));
    // Set-state-in-effect is intentional — auto-switch needs to observe
    // query changes (a side-effectful trigger) and reconcile with the
    // ref-tracked activeSection (intentionally NOT in deps to avoid
    // sidebar clicks re-triggering the switch). Can't be a derived
    // useMemo because the user's manual sidebar click is also a valid
    // setActiveSection writer, so the value isn't a pure function of query.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (first) setActiveSection(first.key);
  }, [query]);

  // Manual section change from sidebar click.
  // A row an opener asked for is dropped once the user moves on by hand, so it never replays.
  const handleSectionChange = useCallback((key: SectionKey) => {
    setActiveSection(key);
    useUiStore.setState({ settingsInitialRow: null });
  }, []);

  // Responsive guard: collapse sidebar when panel is narrower than 500px.
  // `prevCollapsed` matches the initial `useState(false)` so the observer
  // only fires `setCollapsed` when the derived boolean actually changes —
  // avoids re-entering React's state-update path on every pixel of resize.
  useEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    let prevCollapsed = false;
    const observer = new ResizeObserver(([entry]) => {
      const next = entry.contentRect.width < 500;
      if (next !== prevCollapsed) {
        prevCollapsed = next;
        setCollapsed(next);
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <div
      ref={panelRef}
      aria-label="Settings"
      data-testid="settings-panel"
      className="flex h-full min-h-0 flex-1 flex-col bg-background text-foreground"
    >
      <TopBar query={query} onQueryChange={setQuery} searchInputRef={searchInputRef} />
      <div className="flex min-h-0 flex-1">
        <SettingsSidebar
          activeSection={activeSection}
          onSectionChange={handleSectionChange}
          query={query}
          collapsed={collapsed}
        />
        <div
          role="tabpanel"
          id="settings-tabpanel"
          aria-labelledby={`settings-tab-${activeSection}`}
          className="flex-1 overflow-y-auto"
        >
          <div className="max-w-3xl px-6 pb-12">
            {activeSection === "general" && <GeneralSection query={query} />}
            {activeSection === "appearance" && <AppearanceSection query={query} />}
            {activeSection === "dashboards" && <DashboardsSection query={query} />}
            {activeSection === "identity" && <IdentitySection query={query} />}
            {activeSection === "shortcuts" && <ShortcutsSection query={query} />}
            {activeSection === "repo" && (
              <RepoSection
                key={`${teamKey}:${repo?.repoPath ?? "no-repo"}`}
                repo={repo}
                teamKey={teamKey}
                query={query}
              />
            )}
            {activeSection === "workspace" && <WorkspaceSection query={query} />}
            {activeSection === "phases" && <PhasesSection query={query} />}
            {activeSection === "projection-excludes" && <ProjectionExcludesSection query={query} />}
            {activeSection === "account" && <AccountSection query={query} />}
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------- Top bar ----------

function TopBar({
  query,
  onQueryChange,
  searchInputRef,
}: {
  query: string;
  onQueryChange: (next: string) => void;
  searchInputRef: React.RefObject<HTMLInputElement | null>;
}) {
  // A warm team stays mounted, so `autoFocus` does not run again when the team is shown again.
  useFocusWhenShown(useOwningScope(), true, () =>
    searchInputRef.current?.focus({ preventScroll: true }),
  );
  return (
    <div className="border-b border-border bg-background">
      <div className="flex w-full items-center gap-3 px-6 pt-4">
        <h1 className="text-base font-semibold">Settings</h1>
      </div>
      <div className="flex w-full items-center gap-2 px-6 pb-4 pt-3">
        <Search size={14} className="text-muted-foreground" aria-hidden="true" />
        <input
          ref={searchInputRef}
          type="search"
          aria-label="Search settings"
          data-testid="settings-search-input"
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          onKeyDown={(e) => {
            // Escape clears the search (local to the input — a tab never
            // registers a global Escape listener).
            if (e.key === "Escape" && query.length > 0) {
              e.preventDefault();
              e.stopPropagation();
              onQueryChange("");
            }
          }}
          placeholder="Search settings..."
          autoFocus
          className="flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
        />
      </div>
    </div>
  );
}

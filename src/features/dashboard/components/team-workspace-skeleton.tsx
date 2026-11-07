// SPDX-License-Identifier: MIT
// SF-8: structural skeleton shown while `<TeamWorkspaceView>` is gathering
// the data it needs to be interactive. Replaces the previous `<p>Loading…</p>`
// fallback in the sidebar AND the `if (!repo) return null` blank state. The
// skeleton mimics TeamWorkspaceView's outer flex column so swapping in the
// real view at gate-clear doesn't shift layout. Tailwind `animate-pulse` on
// `bg-muted/40` blocks signals "loading" without a spinner.

export function TeamWorkspaceSkeleton() {
  return (
    <div
      data-testid="team-workspace-skeleton"
      className="flex flex-col h-full bg-background text-foreground animate-pulse"
      aria-busy="true"
      aria-label="Loading workspace"
    >
      <div className="flex flex-1 overflow-hidden">
        <div className="flex-1 flex flex-col overflow-hidden min-w-0">
          {/* Sidebar + viewer row */}
          <div className="flex overflow-hidden flex-1">
            {/* Sidebar nav placeholder — mirrors width of the live status nav */}
            <div className="h-full flex flex-col flex-shrink-0 gap-2 p-3" style={{ width: 256 }}>
              <div className="h-3 w-3/4 rounded bg-muted/40" />
              <div className="h-3 w-2/3 rounded bg-muted/40" />
              <div className="h-3 w-5/6 rounded bg-muted/40" />
              <div className="h-3 w-1/2 rounded bg-muted/40" />
            </div>

            {/* Pipeline / content column placeholder */}
            <div className="flex-1 flex flex-col overflow-hidden p-4 gap-3">
              <div className="h-4 w-1/3 rounded bg-muted/40" />
              <div className="h-24 w-full rounded bg-muted/40" />
              <div className="h-24 w-full rounded bg-muted/40" />
              <div className="h-24 w-2/3 rounded bg-muted/40" />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

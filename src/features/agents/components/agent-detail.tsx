// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { Users } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/shared/components/empty-state";

import { isBuiltin } from "../lib/agent-display";
import { useAgentRequest } from "../lib/use-agent-request";
import { useAgentInstances } from "../lib/use-agent-instances";
import { useAgentsStore, type AgentView } from "../stores/agents-store";
import { AgentCreateForm, AgentEditor } from "./agent-editor";
import { AgentHistory } from "./agent-history";
import { AgentProfileHeader, AgentSlimHeader } from "./agent-header";
import { AgentOverview } from "./agent-overview";
import { AgentRoutines } from "./agent-routines";
import { SpendCapBlock } from "./spend-cap";
import { AgentRunning } from "./agent-running";
import { LineTab, LineTabs } from "./line-tabs";
import { RoutinePage } from "@/features/routines/components/routine-page";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";

const VIEWS: { id: AgentView; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "running", label: "Running" },
  { id: "edit", label: "Edit" },
  { id: "history", label: "History" },
];

/** The views a starter has: it ships with the app, so nothing to edit or restore. */
const BUILTIN_VIEWS = new Set<AgentView>(["overview", "running"]);

/** The selected agent: its profile and four facts, then its views (Overview, Running, Edit,
 *  History). A starter has no editor or history: Use template makes an editable agent from it.
 *  With the New agent form open, that form instead. */
export function AgentDetailPane({ contentRoot }: { contentRoot: string }) {
  const creating = useAgentsStore((s) => s.creating);
  const selectedKey = useAgentsStore((s) => s.selectedKey);
  const row = useAgentsStore((s) => s.rows?.find((r) => r.key === s.selectedKey) ?? null);
  const detail = useAgentsStore((s) => (s.selectedKey ? s.details[s.selectedKey] : undefined));
  const detailError = useAgentsStore((s) =>
    s.selectedKey ? s.detailError[s.selectedKey] : undefined,
  );
  const storedView = useAgentsStore((s) => s.view);
  const setView = useAgentsStore((s) => s.setView);
  const loadDetail = useAgentsStore((s) => s.loadDetail);
  // A routine's page replaces the agent's views while open (design §4.5).
  const routinePage = useRoutinesStore(
    (s) =>
      s.routineOwner !== null &&
      s.routineOwner === selectedKey &&
      (s.creating || s.selectedKey !== null),
  );
  const now = useNowClock(30_000);
  const builtin = row ? isBuiltin(row) : false;
  // A starter ships with the app: one revision, nothing to edit or restore.
  const view: AgentView = builtin && !BUILTIN_VIEWS.has(storedView) ? "overview" : storedView;
  const views = builtin ? VIEWS.filter((v) => BUILTIN_VIEWS.has(v.id)) : VIEWS;
  const instances = useAgentInstances(row?.key ?? null);
  // The account picker lives on the Overview: a request for it brings the view back there.
  const accountRequested = useAgentRequest("account-picker", selectedKey) !== null;
  useEffect(() => {
    if (accountRequested && storedView !== "overview") setView("overview");
  }, [accountRequested, storedView, setView]);
  const waiting = instances.filter((i) => i.state === "needs").length;
  // The agent whose editor is mounted: opened on the first visit to Edit, then kept while
  // the user looks at another view, so an unsaved edit survives it. Nothing is resolved for
  // an agent whose editor was never opened.
  const [editorFor, setEditorFor] = useState<string | null>(null);
  if (view === "edit" && selectedKey && editorFor !== selectedKey) setEditorFor(selectedKey);
  if (creating) return <AgentCreateForm contentRoot={contentRoot} />;
  if (!selectedKey || !row) {
    return (
      <EmptyState
        testId="agent-detail-empty"
        className="pt-24"
        icon={<Users />}
        title="Select an agent"
        body="Pick one on the left to see what it does, what it runs on and what is running it."
      />
    );
  }
  if (routinePage) return <RoutinePage agent={row} now={now} />;
  return (
    <div data-testid="agent-detail" className="flex min-h-0 flex-1 flex-col">
      {view === "overview" ? <AgentProfileHeader row={row} /> : <AgentSlimHeader row={row} />}
      <LineTabs
        value={view}
        onValueChange={(v) => setView(v as AgentView)}
        label="Agent views"
        className="h-[38px] shrink-0 border-b border-border px-5"
      >
        {views.map(({ id, label }) => (
          <LineTab key={id} value={id} data-testid={`agent-view-${id}`}>
            {label}
            {id === "running" && instances.length > 0 && (
              <Badge
                tone={waiting > 0 ? "needs" : "neutral"}
                aria-label={
                  waiting > 0 ? `${waiting} waiting on you` : `${instances.length} running`
                }
              >
                {instances.length}
              </Badge>
            )}
          </LineTab>
        ))}
      </LineTabs>
      {/* The one scrollport of an agent: the header and tabs stay put, and the editor's
          navigation and save bar stick to this box. */}
      <div data-testid="agent-detail-scroll" className="min-h-0 flex-1 overflow-y-auto">
        {view === "running" && <AgentRunning agent={row} instances={instances} />}
        {detailError && (
          <InlineAlert
            severity="error"
            className="mx-6 mt-4"
            title="Couldn't load this agent"
            error={detailError}
            action={{ label: "Try again", onClick: () => void loadDetail(row.key) }}
          />
        )}
        {!detail && !detailError && view !== "running" && (
          <div aria-busy="true" aria-label="Loading" className="grid max-w-[860px] gap-2 px-6 pt-4">
            <span className="sr-only">Loading…</span>
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-[38px] w-full" />
            ))}
          </div>
        )}
        {detail && (
          <>
            {view === "overview" && (
              <div className="flex max-w-[860px] flex-col gap-4 px-6 pt-1 pb-6">
                <AgentOverview row={row} detail={detail} />
                <AgentRoutines agent={row} now={now} />
                {!builtin && <SpendCapBlock scopeKind="agent" scopeKey={detail.key} />}
                {!builtin && (
                  <p className="text-xs text-muted-foreground">
                    Changes are local until your agent pushes them.
                  </p>
                )}
              </div>
            )}
            {/* Kept mounted while another view shows, so an unsaved edit survives a look at
                the Overview or History. */}
            {editorFor === row.key && !builtin && (
              <div hidden={view !== "edit"} className="h-full">
                <AgentEditor key={row.key} row={row} detail={detail} contentRoot={contentRoot} />
              </div>
            )}
            {view === "history" && <AgentHistory detail={detail} />}
          </>
        )}
      </div>
    </div>
  );
}

// SPDX-License-Identifier: MIT
import { useRoleEditorStore } from "../stores/role-editor-store";
import { RolesView, RoleLeaveGuard } from "./roles-view";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Plus } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { InlineAlert } from "@/components/ui/inline-alert";
import { AppButton as Button } from "@/shared/components/app-button";
import { ConfirmActionDialog } from "@/features/settings/components/confirm-action-dialog";
import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";
import type { AgentRow } from "@/tauri/commands";

import { useRoutinesStore, watchRoutineList } from "@/features/routines/stores/routines-store";

import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useActiveTeamKey } from "@/shared/lib/team-key";

import { attentionCount } from "../lib/schedule-model";
import { LineTab, LineTabs } from "./line-tabs";
import { useAgentsStore, watchAgentList, type AgentsTabView } from "../stores/agents-store";
import { switchView } from "./agents-tab-nav";
import { ScheduleView } from "./schedule-view";
import { AgentDetailPane } from "./agent-detail";
import { AgentList } from "./agent-list";
import { NewAgentDialog } from "./new-agent-dialog";

/** What a delete confirm says: a teammate's personal routine using the agent can't be seen
 *  here, so a delete could strand it. Archive keeps everything working. */
export const DELETE_WARNING =
  "Routines teammates keep for themselves aren't visible to you and would stop running. Archive keeps them working.";

/**
 * The Agents tab (`virtual://agents`): your agents and your teams', each in a sentence,
 * with which of your accounts runs it, what uses it and what is running it. The list sits
 * on the left in a fixed order; the selected agent opens on the right.
 */
export function AgentsTab({
  teamContentPath,
  hostTeamKey,
}: CommonViewerProps & { filePath: string }) {
  const activeTeam = useActiveTeamKey();
  const visible = useViewerStore(
    (s) =>
      !hostTeamKey ||
      (hostTeamKey === activeTeam &&
        s.activeTab[hostTeamKey] === "virtual://agents" &&
        s.layoutMode !== "console"),
  );
  useEffect(() => (visible ? watchAgentList() : undefined), [visible]);
  // Routines belong to agents: their list, runs and counts stay live while the tab is open.
  useEffect(() => watchRoutineList(), []);
  const routineError = useRoutinesStore((s) => s.actionError);
  const dismissRoutineError = useRoutinesStore((s) => s.dismissActionError);
  const routineNotice = useRoutinesStore((s) => s.notice);
  const dismissRoutineNotice = useRoutinesStore((s) => s.dismissNotice);
  const routineLeave = useRoutinesStore((s) => s.pendingLeave);
  const confirmRoutineLeave = useRoutinesStore((s) => s.confirmLeave);
  const cancelRoutineLeave = useRoutinesStore((s) => s.cancelLeave);
  const creatingRoutine = useRoutinesStore((s) => s.creating);
  const actionError = useAgentsStore((s) => s.actionError);
  const dismiss = useAgentsStore((s) => s.dismissActionError);
  const notice = useAgentsStore((s) => s.notice);
  const dismissNotice = useAgentsStore((s) => s.dismissNotice);
  const remove = useAgentsStore((s) => s.remove);
  const deleting = useAgentsStore((s) =>
    Object.keys(s.pending).some((id) => id.startsWith("delete:")),
  );
  const [confirmDelete, setConfirmDelete] = useState<AgentRow | null>(null);
  const pendingLeave = useAgentsStore((s) => s.pendingLeave);
  const confirmLeave = useAgentsStore((s) => s.confirmLeave);
  const cancelLeave = useAgentsStore((s) => s.cancelLeave);
  const creating = useAgentsStore((s) => s.creating);
  const [newAgentOpen, setNewAgentOpen] = useState(false);
  const tabView = useAgentsStore((s) => s.tabView);
  const now = useNowClock(30_000);
  return (
    <div
      data-testid="agents-tab"
      className="relative flex h-full min-h-0 flex-col bg-background text-foreground"
    >
      {/* What went wrong or was worth saying floats over the tab like a toast, one card per
          message with its own Dismiss, so nothing pushes the tab's own layout down. */}
      <div
        data-testid="agents-notices"
        className="pointer-events-none absolute top-12 right-3 z-30 flex w-96 max-w-[calc(100%-1.5rem)] flex-col gap-2 [&>*]:pointer-events-auto [&>*]:bg-background [&>*]:shadow-md"
      >
        {actionError && (
          <InlineAlert
            severity="error"
            title="That didn't work"
            action={{ label: "Dismiss", onClick: dismiss }}
          >
            {actionError}
          </InlineAlert>
        )}
        {notice && (
          <InlineAlert
            data-testid="agents-notice"
            action={{ label: "Dismiss", onClick: dismissNotice }}
          >
            {notice}
          </InlineAlert>
        )}
        {routineError && (
          <InlineAlert
            severity="error"
            title="That didn't work"
            action={{ label: "Dismiss", onClick: dismissRoutineError }}
          >
            {routineError}
          </InlineAlert>
        )}
        {routineNotice && (
          <InlineAlert
            data-testid="routines-notice"
            action={{ label: "Dismiss", onClick: dismissRoutineNotice }}
          >
            {routineNotice}
          </InlineAlert>
        )}
      </div>
      <ConfirmActionDialog
        open={routineLeave !== null}
        onOpenChange={(open) => {
          if (!open) cancelRoutineLeave();
        }}
        title="Discard unsaved changes?"
        description={
          creatingRoutine
            ? "This new routine isn't saved yet. Leaving drops it."
            : "This routine has edits that aren't saved. Leaving drops them."
        }
        confirmLabel="Discard"
        loadingLabel="Discarding…"
        loading={false}
        onConfirm={confirmRoutineLeave}
        testId="routine-discard-confirm"
      />
      <ConfirmActionDialog
        open={confirmDelete !== null}
        onOpenChange={(open) => {
          if (!open) setConfirmDelete(null);
        }}
        title={`Delete ${confirmDelete?.name ?? "agent"}?`}
        description={DELETE_WARNING}
        confirmLabel="Delete"
        loadingLabel="Deleting…"
        loading={deleting}
        onConfirm={() => {
          const row = confirmDelete;
          if (!row) return;
          void remove(row.key, row.revision).then(() => setConfirmDelete(null));
        }}
        testId="agent-delete-confirm"
      />
      <ConfirmActionDialog
        open={pendingLeave !== null}
        onOpenChange={(open) => {
          if (!open) cancelLeave();
        }}
        title="Discard unsaved changes?"
        description={
          creating
            ? "This new agent isn't saved yet. Leaving drops it."
            : "This agent has edits that aren't saved. Leaving drops them."
        }
        confirmLabel="Discard"
        loadingLabel="Discarding…"
        loading={false}
        onConfirm={confirmLeave}
        testId="agent-discard-confirm"
      />
      <RoleLeaveGuard />
      <AgentsTabHeader>
        {tabView === "agents" && (
          <Button
            size="sm"
            variant="outline"
            disabled={creating}
            onClick={() => setNewAgentOpen(true)}
          >
            <Plus />
            New agent
          </Button>
        )}
      </AgentsTabHeader>
      <NewAgentDialog open={newAgentOpen} onClose={() => setNewAgentOpen(false)} />
      {tabView === "roles" ? (
        <div id="agents-panel-roles" role="tabpanel" className="flex min-h-0 flex-1">
          <RolesView />
        </div>
      ) : tabView === "schedule" ? (
        <div id="agents-panel-schedule" role="tabpanel" className="flex min-h-0 flex-1">
          <ScheduleView now={now} />
        </div>
      ) : (
        <div id="agents-panel-agents" role="tabpanel" className="flex min-h-0 flex-1">
          <div className="flex w-[min(25rem,45%)] shrink-0 flex-col border-r border-border">
            <AgentList onDelete={setConfirmDelete} />
          </div>
          <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto">
            <AgentDetailPane contentRoot={teamContentPath} />
          </div>
        </div>
      )}
    </div>
  );
}

const VIEWS: { id: AgentsTabView; label: string }[] = [
  { id: "agents", label: "Agents" },
  { id: "schedule", label: "Schedule" },
  { id: "roles", label: "Roles" },
];

/** The tab's header: the Agents | Schedule switch, the Schedule filter, and the view's one
 *  primary action (design §4.1). */
function AgentsTabHeader({ children }: { children: ReactNode }) {
  const tabView = useAgentsStore((s) => s.tabView);
  const seen = useAgentsStore((s) => s.seenRuns);
  const rows = useRoutinesStore((s) => s.rows);
  const attention = useMemo(() => (rows ? attentionCount(rows, seen) : 0), [rows, seen]);
  const go = (view: AgentsTabView) => {
    if (view === "roles")
      useRoleEditorStore.getState().requestLeave(() => {
        useRoleEditorStore.setState({ teamKey: null, role: null });
        switchView(view);
      });
    else switchView(view);
  };
  return (
    <div className="flex h-11 shrink-0 items-stretch gap-2 border-b border-border bg-background pr-3 pl-2.5">
      <LineTabs
        value={tabView}
        onValueChange={(v) => go(v as AgentsTabView)}
        onReselect={(v) => go(v as AgentsTabView)}
        label="Agents tab views"
        testId="agents-view-switch"
        listClassName="gap-1"
      >
        {VIEWS.map(({ id, label }) => {
          const count = id === "schedule" && attention > 0 ? attention : 0;
          return (
            <LineTab
              key={id}
              value={id}
              data-testid={`agents-view-${id}`}
              aria-label={
                count ? `${label}, ${count} ${count === 1 ? "needs" : "need"} attention` : label
              }
              className="px-3 text-sm"
            >
              {label}
              {count > 0 && (
                <Badge tone="warn" aria-hidden="true">
                  {count}
                </Badge>
              )}
            </LineTab>
          );
        })}
      </LineTabs>
      <div className="ml-auto flex items-center">{children}</div>
    </div>
  );
}

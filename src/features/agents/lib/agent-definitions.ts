// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { liveHub, type HubFrame, type HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { agentDefinitionsPrepare, type AgentDetailView, type AgentRow } from "@/tauri/commands";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { agentDetailIsVisible, agentListIsVisible, useAgentsStore } from "../stores/agents-store";

const TOPIC = "agentDefinitions";
let frame: HubFrame | undefined;
let key: string | undefined;
let visible = false;
let generation = 0;
let readSequence = 0;
let factsRevision: unknown;

/** Only materialize the hub's rows; selection, drafts and pending actions belong to the UI. */
export function applyAgentDefinitions(view: HubSnapshot): void {
  const nextFacts = view.snapshot.find((row) => row.rowType === "facts")?.routinesRevision;
  const resetRefusals = nextFacts !== factsRevision;
  factsRevision = nextFacts;
  const rows: AgentRow[] = [];
  const details: Record<string, AgentDetailView> = {};
  const accounts = new Map<string, AgentRow["account"]>();
  for (const row of view.snapshot) {
    if (row.rowType === "definition") rows.push(row.row as unknown as AgentRow);
    if (row.rowType === "detail")
      details[String(row.key)] = row.detail as unknown as AgentDetailView;
    if (row.rowType === "account")
      accounts.set(String(row.key), row.account as AgentRow["account"]);
  }
  useAgentsStore.setState((state) => {
    const oldRows = new Map(state.rows?.map((row) => [row.key, row]));
    const nextRows = rows
      .filter((row) => state.includeArchived || !row.archived)
      .map((row) => {
        const next = { ...row, account: accounts.get(row.key) ?? row.account };
        const before = oldRows.get(row.key);
        return before && JSON.stringify(before) === JSON.stringify(next) ? before : next;
      });
    const nextDetails = { ...state.details };
    for (const [id, detail] of Object.entries(details)) {
      const next = {
        ...detail,
        resolved: { ...detail.resolved, account: accounts.get(id) ?? detail.resolved.account },
      };
      if (JSON.stringify(nextDetails[id]) !== JSON.stringify(next)) nextDetails[id] = next;
    }
    const present = new Set(rows.map((row) => row.key));
    for (const id of Object.keys(nextDetails)) if (!present.has(id)) delete nextDetails[id];
    const inUse = Object.fromEntries(
      Object.entries(state.inUse).filter(
        ([id]) =>
          JSON.stringify(oldRows.get(id)?.used_by) ===
          JSON.stringify(rows.find((row) => row.key === id)?.used_by),
      ),
    );
    return {
      rows: nextRows,
      details: nextDetails,
      inUse: resetRefusals ? {} : inUse,
      listError: null,
      detailError: {},
    };
  });
}

/** One native bridge, shared by the app root and every mounted Agents tab. */
export async function readAgentDefinitions(detailKey?: string): Promise<void> {
  const started = generation;
  const sequence = ++readSequence;
  const view = await agentDefinitionsPrepare(
    agentDetailIsVisible() ? detailKey : undefined,
    undefined,
    visible,
    sequence,
  );
  if (started !== generation || sequence !== readSequence) return;
  liveHub.seed(view);
  // Explicit reads also prime navigation before its tab is revealed.
  const snapshot = await liveHub.snapshot(TOPIC, view.viewKey);
  if (started !== generation || sequence !== readSequence) return;
  applyAgentDefinitions(snapshot);
  if (!visible) return;
  const connecting = key !== view.viewKey;
  if (connecting) {
    frame?.dispose();
    key = view.viewKey;
    frame = liveHub.attach(
      "native:agent-definitions",
      key,
      (message) => {
        if (message.topic !== TOPIC || !visible || !frame) return;
        const current = frame;
        void current.subscribe(TOPIC).then((snapshot) => {
          if (current === frame && visible) applyAgentDefinitions(snapshot);
        });
      },
      true,
      true,
    );
  }
  frame!.visible(true);
  const currentFrame = frame;
  const subscribed = await currentFrame!.subscribe(TOPIC, view.rev);
  if (currentFrame === frame && visible && started === generation && sequence === readSequence)
    applyAgentDefinitions(subscribed);
  if (connecting) {
    // Close the prepare/attach gap using cached hub rows, without another source read.
    const current = frame;
    const latest = await liveHub.resync(TOPIC, view.viewKey);
    if (current === frame && visible && started === generation && sequence === readSequence)
      applyAgentDefinitions(latest);
  }
}

export function showAgentDefinitions(shown: boolean): void {
  visible = shown;
  if (!shown) {
    frame?.visible(false);
    // Stop resolving the selected detail while its retained tab is hidden.
    if (key) void agentDefinitionsPrepare(undefined, undefined, false, ++readSequence);
    return;
  }
  const store = useAgentsStore.getState();
  void store.loadList();
  if (agentDetailIsVisible()) {
    if (store.selectedKey) void store.loadDetail(store.selectedKey);
    void store.loadAccounts();
  }
}

export function resetAgentDefinitions(disposeHub = false): void {
  generation++;
  visible = false;
  frame?.dispose();
  frame = undefined;
  key = undefined;
  factsRevision = undefined;
  if (disposeHub) liveHub.dispose();
}

/** A storage-profile change releases the private view rather than changing provider defaults. */
export function useAgentDefinitions(): void {
  const principal = useCurrentUser()?.github_user_id;
  useEffect(() => {
    useAgentsStore.setState({ rows: null, details: {}, accounts: null, selectedKey: null });
    if (agentListIsVisible()) showAgentDefinitions(true);
    return () => resetAgentDefinitions();
  }, [principal]);
}

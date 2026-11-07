// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { useAgentsStore, watchAgentList } from "@/features/agents/stores/agents-store";
import { useTeamSourceRepoSlugs } from "@/shared/lib/pipeline-graph/use-source-repo-selection";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  AGENTS_CHANGED_EVENT,
  CONTEXT_FILE_CHANGE_EVENT,
  type AgentsChangedPayload,
  type ContextFileChangePayload,
} from "@/tauri/events";
import {
  agentContextPreview,
  agentContextUpdate,
  type AgentContextView,
  type AgentRow,
  type SourceRepoSlug,
} from "@/tauri/commands";

import {
  buildContextMembership,
  isUnder,
  planAdd,
  planRemove,
  repoRequestLabel,
  repoSlugMatches,
  type AgentContextSelection,
  type ContextEditPlan,
  type ContextMembership,
} from "../lib/agent-context-model";

/** One agent the pivot offers: a team agent of this view's team. */
export interface PivotAgent {
  key: string;
  name: string;
  /** The face its definition picked, if any (else one is dealt from its key). */
  avatar: string | null;
}

export type ContextEditAction = "add" | "remove";

/** The resolved pivot, its want-set overlay and the edits it offers. */
export interface AgentContextPivot {
  /** The team's agents (not archived), by name. */
  agents: PivotAgent[];
  /** The team's source repos, for "· in ‹Repo›". */
  repos: SourceRepoSlug[];
  /** The effective selection; `null` = Team (everything). A persisted agent or repo that is
   *  gone reads as Team (everything) / no repo, with no write. */
  agent: PivotAgent | null;
  repo: SourceRepoSlug | null;
  select: (next: AgentContextSelection | null) => void;
  /** The agent's want set for the repo, once resolved; the last one stays while a newer
   *  resolve runs, so the overlay never flickers. */
  view: AgentContextView | null;
  loading: boolean;
  error: string | null;
  reload: () => void;
  /** Membership of a team-content-relative path, optimistic edits applied. `null` when no
   *  agent is selected (or nothing has resolved yet). */
  membership: ((rel: string) => ContextMembership) | null;
  /** What an edit would do (`null` without a resolved view). */
  plan: (rel: string, action: ContextEditAction, repoOnly: boolean) => ContextEditPlan | null;
  /** Apply an edit: optimistic, applied on the server against the latest revision this view
   *  knows (edits run one at a time, so a second edit never conflicts with the first); a
   *  teammate's concurrent edit is a conflict toast offering Reload. */
  edit: (rel: string, action: ContextEditAction, repoOnly: boolean) => void;
}

/** The id part of an agent key (`…/barb` → `barb`): a name until the list lands, and the
 *  name of an agent that has none. */
function idOfKey(key: string): string {
  return key.split("/").pop() || key;
}

function teamAgentsOf(rows: AgentRow[] | null, org: string, team: string | null): PivotAgent[] {
  if (!rows || !team) return [];
  return rows
    .filter((r) => r.scope === "team" && r.org === org && r.team === team && !r.archived)
    .map((r) => ({ key: r.key, name: r.name.trim() || idOfKey(r.key), avatar: r.avatar }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** A file change the agent's want set may depend on: under one of its tier roots, at or
 *  around one of its entries (a repo-tier change always counts). Unknown paths count. */
function touchesWantSet(
  view: AgentContextView,
  filePath: string | undefined,
  inheritedBy?: string,
): boolean {
  if (!filePath) return true;
  if (
    view.files.some((file) => file.source === filePath || file.source?.startsWith(`${filePath}/`))
  )
    return true;
  if (inheritedBy && view.roots.some((root) => root.path === inheritedBy)) return true;
  for (const root of view.roots) {
    const base = root.path.replace(/\/+$/, "");
    if (!filePath.startsWith(`${base}/`)) continue;
    if (root.tier.kind === "repo") return true;
    const rel = filePath.slice(base.length + 1);
    if (view.entries.some((e) => isUnder(rel, e.path) || isUnder(e.path, rel))) return true;
  }
  return false;
}

const FILE_CHANGE_SETTLE_MS = 300;

/**
 * SW-XS-15 — the left nav's "Context for ‹Agent› · in ‹Repo›" pivot for one team view: the
 * persisted selection (`viewer-store.selectedAgentContext[teamKey]`), the agent's resolved
 * want set (`agent_context_preview`, re-resolved when the agent changes elsewhere or a file
 * under its want set changes), and the right-click edits (`agent_context_update`, one
 * operation applied on the server, whose reply carries the re-resolved want set).
 *
 * Mounted once per team view; the header, the nav overlay and the nav's context menu all read
 * this one instance, so there is one resolve per selection and one per edit.
 */
export function useAgentContext(teamKey: string, enabled = true): AgentContextPivot {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const rows = useAgentsStore((s) => s.rows);
  const repos = useTeamSourceRepoSlugs(teamKey);
  const raw = useViewerStore((s) => s.selectedAgentContext?.[teamKey] ?? null);
  const setSlot = useViewerStore((s) => s.setSelectedAgentContext);

  useEffect(() => {
    if (!enabled) return;
    return watchAgentList();
  }, [enabled]);

  const agents = useMemo(() => teamAgentsOf(rows, orgSlug, teamName), [rows, orgSlug, teamName]);

  // Read-time coercion: until the list lands, trust the persisted key (named by its id);
  // after, an agent that is gone reads as Team (everything).
  const agent = useMemo<PivotAgent | null>(() => {
    if (!raw) return null;
    if (rows === null) return { key: raw.agentKey, name: idOfKey(raw.agentKey), avatar: null };
    return agents.find((a) => a.key === raw.agentKey) ?? null;
  }, [raw, rows, agents]);
  const repo = useMemo<SourceRepoSlug | null>(() => {
    if (!agent || !raw?.repoSlug) return null;
    return repos.find((r) => repoSlugMatches(raw.repoSlug!, r)) ?? null;
  }, [agent, raw, repos]);

  const select = useCallback(
    (next: AgentContextSelection | null) => setSlot(teamKey, next),
    [setSlot, teamKey],
  );

  // ---- the want set -------------------------------------------------------------------
  const [view, setView] = useState<AgentContextView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reloadNonce, setReloadNonce] = useState(0);
  const [optimistic, setOptimistic] = useState<ReadonlyMap<string, "in" | "out">>(new Map());
  const generation = useRef(0);
  const viewRef = useRef<AgentContextView | null>(null);
  viewRef.current = view;

  const reload = useCallback(() => setReloadNonce((n) => n + 1), []);

  const agentKey = agent?.key ?? null;
  const repoLabel = repo ? repoRequestLabel(repo) : null;

  // ---- edits: one at a time, each against the latest revision this view knows ---------
  const queue = useRef<Promise<void>>(Promise.resolve());
  const latestRevision = useRef<{ key: string; revision: number } | null>(null);
  /** While one of our edits is in flight, an `agents:changed` for this agent may be its own
   *  echo arriving before the reply: hold it (highest revision seen; Infinity when unknown)
   *  and decide once the reply lands. */
  const inFlight = useRef(false);
  const heldEcho = useRef<number | null>(null);

  useEffect(() => {
    if (!enabled || !agentKey) return;
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const keep = (p: Promise<() => void>) =>
      void p.then((fn) => (disposed ? fn() : unlisteners.push(fn))).catch(() => {});
    keep(
      listen<AgentsChangedPayload>(AGENTS_CHANGED_EVENT, (event) => {
        const changed = event.payload?.key;
        if (changed && changed !== agentKey) return;
        // A revision this view already shows (our own save's echo) needs no re-read.
        const revision = event.payload?.revision;
        if (inFlight.current) {
          heldEcho.current = Math.max(heldEcho.current ?? 0, revision ?? Infinity);
          return;
        }
        const known = latestRevision.current;
        if (revision !== undefined && known?.key === agentKey && revision <= known.revision) {
          return;
        }
        setReloadNonce((n) => n + 1);
      }),
    );
    let settle: ReturnType<typeof setTimeout> | null = null;
    keep(
      listen<ContextFileChangePayload>(CONTEXT_FILE_CHANGE_EVENT, (event) => {
        const current = viewRef.current;
        if (
          current &&
          !touchesWantSet(current, event.payload?.file_path, event.payload?.inherited_by)
        )
          return;
        if (settle) clearTimeout(settle);
        settle = setTimeout(() => setReloadNonce((n) => n + 1), FILE_CHANGE_SETTLE_MS);
      }),
    );
    return () => {
      disposed = true;
      if (settle) clearTimeout(settle);
      unlisteners.forEach((fn) => fn());
    };
  }, [enabled, agentKey]);

  useEffect(() => {
    if (!enabled || !agentKey) {
      // Drop any resolve in flight. The last view stays in state but is never shown:
      // `current` below only shows a view of the picked agent and repo.
      generation.current += 1;
      return;
    }
    const mine = ++generation.current;
    setLoading(true);
    setError(null);
    agentContextPreview(agentKey, repoLabel ?? undefined).then(
      (next) => {
        if (generation.current !== mine) return;
        setView(next);
        latestRevision.current = { key: next.key, revision: next.revision };
        setOptimistic(new Map());
        setLoading(false);
      },
      (e: unknown) => {
        if (generation.current !== mine) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
  }, [enabled, agentKey, repoLabel, reloadNonce]);

  // A view for another agent or repo is never shown (a switch shows "resolving", not the old
  // agent's set).
  const current =
    view &&
    agentKey &&
    view.key === agentKey &&
    (view.repo ?? "").toLowerCase() === (repoLabel ?? "").toLowerCase()
      ? view
      : null;

  const baseMembership = useMemo(
    () => (current ? buildContextMembership(current) : null),
    [current],
  );
  const membership = useMemo(() => {
    if (!baseMembership) return null;
    if (optimistic.size === 0) return baseMembership;
    return (rel: string): ContextMembership => {
      for (const [path, state] of optimistic) {
        if (isUnder(rel, path)) return { state };
      }
      return baseMembership(rel);
    };
  }, [baseMembership, optimistic]);

  const plan = useCallback(
    (rel: string, action: ContextEditAction, repoOnly: boolean): ContextEditPlan | null => {
      if (!current) return null;
      const target = repoOnly ? (repoLabel ?? undefined) : undefined;
      if (repoOnly && !target) return null;
      const path = rel.normalize("NFC");
      return action === "add" ? planAdd(current, path, target) : planRemove(current, path, target);
    },
    [current, repoLabel],
  );

  const edit = useCallback(
    (rel: string, action: ContextEditAction, repoOnly: boolean) => {
      if (!current) return;
      rel = rel.normalize("NFC");
      const planned = plan(rel, action, repoOnly);
      if (!planned) return;
      if (planned.kind === "blocked") {
        toast(planned.reason);
        return;
      }
      const key = current.key;
      const name = current.name;
      const target = repoOnly ? (repoLabel ?? undefined) : undefined;
      const viewRepo = repoLabel;
      const revert = () =>
        setOptimistic((prev) => {
          if (!prev.has(rel)) return prev;
          const next = new Map(prev);
          next.delete(rel);
          return next;
        });
      setOptimistic((prev) => new Map(prev).set(rel, planned.optimistic));
      const run = async () => {
        const base =
          latestRevision.current?.key === key ? latestRevision.current.revision : current.revision;
        inFlight.current = true;
        try {
          const reply = await agentContextUpdate(key, base, {
            op: action,
            path: rel,
            repo: target,
            viewRepo: viewRepo ?? undefined,
          });
          switch (reply.outcome) {
            case "saved": {
              latestRevision.current = { key, revision: reply.revision };
              // The reply is the want set re-resolved for the view being shown: no second
              // read. Should it be another repo's (the pivot moved meanwhile), re-read.
              const sameRepo =
                (reply.view.repo ?? "").toLowerCase() === (viewRepo ?? "").toLowerCase();
              if (sameRepo) setView(reply.view);
              else reload();
              setOptimistic((prev) => {
                const next = new Map(prev);
                next.delete(rel);
                return next;
              });
              return;
            }
            case "conflict":
              revert();
              toast.error(`${name}'s context changed since it was loaded`, {
                description: "Nothing was saved. Reload to see the latest, then try again.",
                action: { label: "Reload", onClick: reload },
              });
              return;
            case "blocked":
              revert();
              toast(reply.reason);
              return;
            case "invalid":
              revert();
              toast.error(`Couldn't change ${name}'s context`, { description: reply.message });
              return;
          }
        } catch (e: unknown) {
          revert();
          toast.error(`Couldn't change ${name}'s context`, {
            description: e instanceof Error ? e.message : String(e),
          });
        } finally {
          inFlight.current = false;
          const held = heldEcho.current;
          heldEcho.current = null;
          const known = latestRevision.current;
          // A change we have not shown arrived meanwhile (someone else's): re-read.
          if (held !== null && !(known?.key === key && held <= known.revision)) reload();
        }
      };
      queue.current = queue.current.then(run, run);
    },
    [current, plan, reload, repoLabel],
  );

  return {
    agents,
    repos,
    agent,
    repo,
    select,
    view: current,
    loading: !!agentKey && (loading || (!current && !error)),
    error: agentKey ? error : null,
    reload,
    membership,
    plan,
    edit,
  };
}

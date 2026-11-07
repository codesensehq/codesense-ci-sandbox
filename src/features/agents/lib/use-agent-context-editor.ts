// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

import {
  agentContextPreview,
  agentContextUpdate,
  agentDismissContextSuggestion,
  type AgentContextEdit,
  type AgentContextSuggestion,
  type AgentContextView,
} from "@/tauri/commands";

/** What the last edit came to, when it did not save. */
export type ContextEditProblem =
  | { kind: "conflict"; currentRevision: number }
  | { kind: "blocked"; reason: string }
  | { kind: "invalid"; message: string }
  | { kind: "error"; message: string };

export interface AgentContextEditor {
  /** The want set for `repo`, once resolved; the last one stays while a newer resolve runs. */
  view: AgentContextView | null;
  loading: boolean;
  error: string | null;
  reload: () => void;
  /** An edit is being saved; the section takes no second one meanwhile. */
  saving: boolean;
  problem: ContextEditProblem | null;
  clearProblem: () => void;
  /** Apply one edit against the revision shown. Resolves true when it saved. */
  apply: (edit: AgentContextEdit) => Promise<boolean>;
  /** The one-time `projections.yml` suggestion, while there is one. */
  suggestion: AgentContextSuggestion | null;
  dismissSuggestion: () => Promise<void>;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * SW-XS-16 — the agent editor's Context section: the agent's want set for one repo (or its
 * team workspace) from `agent_context_preview`, and every edit as ONE `agent_context_update`
 * operation the server applies to the stored definition under the revision CAS (the same
 * command the left nav's pivot edits through, SW-XS-15), whose reply carries the want set
 * re-resolved for the repo shown. `revision` is the agent's revision as the editor last read
 * it: a newer one (a teammate's edit, an agent's) re-resolves. `onSaved(from, to)` lets the
 * editor's own form move its base past a context edit it did not make.
 */
export function useAgentContextEditor(
  key: string,
  repo: string | null,
  revision: number,
  onSaved?: (from: number, to: number) => void,
): AgentContextEditor {
  const [view, setView] = useState<AgentContextView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<ContextEditProblem | null>(null);
  const generation = useRef(0);
  const savingRef = useRef(false);
  const viewRef = useRef<AgentContextView | null>(null);
  viewRef.current = view;
  const repoRef = useRef(repo);
  repoRef.current = repo;
  const onSavedRef = useRef(onSaved);
  onSavedRef.current = onSaved;

  const reload = useCallback(() => setNonce((n) => n + 1), []);

  useEffect(() => {
    const mine = ++generation.current;
    setLoading(true);
    setError(null);
    agentContextPreview(key, repo ?? undefined).then(
      (next) => {
        if (generation.current !== mine) return;
        setView(next);
        setLoading(false);
      },
      (e: unknown) => {
        if (generation.current !== mine) return;
        setError(message(e));
        setLoading(false);
      },
    );
  }, [key, repo, nonce]);

  // The agent moved on elsewhere: re-resolve (our own save already shows its revision). A
  // revision that arrives while a save is in flight is checked once the save lands.
  const revisionRef = useRef(revision);
  revisionRef.current = revision;
  useEffect(() => {
    const shown = viewRef.current?.revision;
    if (shown !== undefined && revision > shown && !savingRef.current) reload();
  }, [revision, reload]);

  // The suggestion rides on the view (`agent_context_preview` and every saved reply), so
  // accepting or a teammate's edit retires it with no second read. Dismissing hides it
  // here until the next view says otherwise.
  const [dismissedFingerprint, setDismissedFingerprint] = useState<string | null>(null);
  const offered = view?.context_suggestion ?? null;
  const suggestion = offered && offered.fingerprint !== dismissedFingerprint ? offered : null;

  const apply = useCallback(
    async (edit: AgentContextEdit): Promise<boolean> => {
      const current = viewRef.current;
      if (!current || savingRef.current) return false;
      savingRef.current = true;
      setSaving(true);
      setProblem(null);
      const base = current.revision;
      let landed = base;
      try {
        const reply = await agentContextUpdate(key, base, { ...edit, viewRepo: repo ?? undefined });
        switch (reply.outcome) {
          case "saved": {
            landed = reply.revision;
            // The reply is resolved for the repo shown when the edit was made; should the
            // preview have moved on meanwhile, resolve that one instead.
            const shown = (repoRef.current ?? "").toLowerCase();
            if ((reply.view.repo ?? "").toLowerCase() === shown) {
              generation.current += 1;
              setView(reply.view);
              setLoading(false);
            } else {
              reload();
            }
            onSavedRef.current?.(base, reply.revision);
            return true;
          }
          case "conflict":
            setProblem({ kind: "conflict", currentRevision: reply.current_revision });
            return false;
          case "blocked":
            setProblem({ kind: "blocked", reason: reply.reason });
            return false;
          case "invalid":
            setProblem({ kind: "invalid", message: reply.message });
            return false;
        }
      } catch (e: unknown) {
        setProblem({ kind: "error", message: message(e) });
        return false;
      } finally {
        savingRef.current = false;
        setSaving(false);
        // A newer revision than the one this save wrote arrived meanwhile (a teammate's
        // edit, an agent's): show it.
        if (revisionRef.current > landed) reload();
      }
    },
    [key, repo, reload],
  );

  const dismissSuggestion = useCallback(async () => {
    if (!suggestion) return;
    const { fingerprint } = suggestion;
    setDismissedFingerprint(fingerprint);
    try {
      await agentDismissContextSuggestion(key, fingerprint);
    } catch (e: unknown) {
      setDismissedFingerprint(null);
      setProblem({ kind: "error", message: message(e) });
    }
  }, [key, suggestion]);

  return {
    view: view && view.key === key ? view : null,
    loading,
    error,
    reload,
    saving,
    problem,
    clearProblem: () => setProblem(null),
    apply,
    suggestion,
    dismissSuggestion,
  };
}

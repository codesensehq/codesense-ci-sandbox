// SPDX-License-Identifier: MIT
import { Loader2, RotateCcw } from "lucide-react";

import { AppButton as Button } from "@/shared/components/app-button";
import type { AgentDetailView } from "@/tauri/commands";

import { revisionDiff } from "../lib/agent-form";
import { useAgentsStore } from "../stores/agents-store";

/** When a revision was saved, as a short local date and time. */
function savedAt(at: string): string {
  const time = Date.parse(at);
  return Number.isNaN(time)
    ? at
    : new Date(time).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/**
 * Every revision of an agent, newest first: who saved it (a person or an agent), when, and
 * what changed against the revision before, startup paths as added and removed. Anyone can
 * restore any older revision, which writes its definition as a new revision credited to them.
 */
export function AgentHistory({ detail }: { detail: AgentDetailView }) {
  const restoring = useAgentsStore((s) => s.pending[`restore:${detail.key}`] === true);
  const restore = useAgentsStore((s) => s.restoreRevision);
  const revisions = detail.revisions;
  const current = detail.definition.revision;
  return (
    <section aria-label="History" data-testid="agent-history">
      <ol className="flex flex-col divide-y divide-border rounded-md border border-border">
        {[...revisions].reverse().map((revision) => {
          const index = revisions.indexOf(revision);
          const prev = index > 0 ? revisions[index - 1].agent : null;
          const changes = prev ? revisionDiff(prev, revision.agent) : [];
          // Revisions with no snapshot (an agent written by hand or pulled) leave a gap: the
          // diff then spans every revision since the one listed before it.
          const since = index > 0 ? revisions[index - 1].revision : null;
          const gap = since !== null && since !== revision.revision - 1;
          return (
            <li
              key={revision.revision}
              data-testid="agent-revision"
              data-revision={revision.revision}
              className="flex flex-col gap-1 px-3 py-2 text-sm"
            >
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">Revision {revision.revision}</span>
                <span data-testid="agent-revision-author" className="text-xs text-muted-foreground">
                  by {revision.author.name}
                  {revision.author.kind === "agent" ? " (agent)" : ""} · {savedAt(revision.at)}
                </span>
                {revision.revision === current ? (
                  <span className="ml-auto text-xs text-muted-foreground">Current</span>
                ) : (
                  <Button
                    className="ml-auto"
                    size="sm"
                    variant="outline"
                    disabled={restoring}
                    aria-label={`Restore revision ${revision.revision}`}
                    onClick={() => void restore(detail.key, revision.revision, current)}
                  >
                    {restoring ? <Loader2 className="animate-spin" /> : <RotateCcw />}
                    Restore
                  </Button>
                )}
              </div>
              {prev === null ? (
                <p className="text-xs text-muted-foreground">
                  {revision.revision === 1 ? "Created." : "The earliest revision on record."}
                </p>
              ) : changes.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  No change to the agent&apos;s fields.
                </p>
              ) : (
                <>
                  {gap && (
                    <p className="text-xs text-muted-foreground">Changes since revision {since}:</p>
                  )}
                  <dl
                    data-testid="agent-revision-diff"
                    data-since={gap ? since : undefined}
                    className="grid grid-cols-[max-content_1fr] gap-x-3 text-xs"
                  >
                    {changes.map((change) => (
                      <div key={change.field} className="contents">
                        <dt className="text-muted-foreground">{change.field}</dt>
                        <dd className="min-w-0 break-words">
                          <del className="text-destructive">{change.before || "—"}</del>
                          {" → "}
                          <ins className="font-medium text-foreground no-underline">
                            {change.after || "—"}
                          </ins>
                        </dd>
                      </div>
                    ))}
                  </dl>
                </>
              )}
            </li>
          );
        })}
      </ol>
    </section>
  );
}

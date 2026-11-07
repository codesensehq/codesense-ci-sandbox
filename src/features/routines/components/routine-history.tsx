// SPDX-License-Identifier: MIT
import { Loader2, RotateCcw } from "lucide-react";

import { AppButton as Button } from "@/shared/components/app-button";
import type { RoutineDetail, RoutineRow } from "@/tauri/commands";

import { timeAgo } from "../lib/routine-display";
import { revisionDiff } from "../lib/routine-sentence";
import { useRoutinesStore } from "../stores/routines-store";

/**
 * Every revision of a routine, newest first: who changed it, when, and what
 * changed against the revision before. Anyone can restore any older revision,
 * which writes its body as a new revision credited to them.
 */
export function RoutineHistory({
  row,
  detail,
  now,
}: {
  row: RoutineRow;
  detail: RoutineDetail;
  now: number;
}) {
  const restoring = useRoutinesStore((s) => s.pending[`restore:${row.key}`] === true);
  const restore = useRoutinesStore((s) => s.restoreRevision);
  const rows = useRoutinesStore((s) => s.rows);
  const space = row.key.slice(0, row.key.lastIndexOf("/") + 1);
  const names = new Map(
    (rows ?? []).filter((r) => r.key.startsWith(space)).map((r) => [r.def.id, r.def.name]),
  );
  const revisions = detail.revisions;
  const current = detail.def.revision;
  return (
    <section aria-label="History" data-testid="routine-history">
      <ol className="flex flex-col divide-y divide-border rounded-md border border-border">
        {[...revisions].reverse().map((revision) => {
          const index = revisions.indexOf(revision);
          const prev = index > 0 ? revisions[index - 1].routine : null;
          const changes = prev ? revisionDiff(prev, revision.routine, names) : [];
          return (
            <li
              key={revision.revision}
              data-testid="routine-revision"
              data-revision={revision.revision}
              className="flex flex-col gap-1 px-3 py-2 text-sm"
            >
              <div className="flex items-center gap-2">
                <span className="font-medium">Revision {revision.revision}</span>
                <span
                  data-testid="routine-revision-author"
                  className="text-xs text-muted-foreground"
                >
                  by {revision.author.name} · {timeAgo(Date.parse(revision.at), now)}
                </span>
                {revision.revision === current ? (
                  <span className="ml-auto text-xs text-muted-foreground">Current</span>
                ) : (
                  <Button
                    className="ml-auto"
                    size="sm"
                    variant="outline"
                    disabled={restoring}
                    onClick={() => void restore(row.key, revision.revision, current)}
                  >
                    {restoring ? <Loader2 className="animate-spin" /> : <RotateCcw />}
                    Restore
                  </Button>
                )}
              </div>
              {prev === null ? (
                <p className="text-xs text-muted-foreground">Created.</p>
              ) : changes.length === 0 ? (
                <p className="text-xs text-muted-foreground">No change to the routine's fields.</p>
              ) : (
                <dl
                  data-testid="routine-revision-diff"
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
              )}
            </li>
          );
        })}
      </ol>
    </section>
  );
}

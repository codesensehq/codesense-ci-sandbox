// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";
import { Box, ChevronDown, Copy, LoaderCircle, RotateCw, User, Users } from "lucide-react";
import { Button } from "@/components/ui/button";
import { InlineAlert } from "@/components/ui/inline-alert";
import type { DashboardRef, PaneEntry } from "@/tauri/commands";
import { shortTime } from "@/features/viewer/lib/session-timers";

export interface SlotFailure {
  kind: "timeout" | "script" | "crash" | "connection" | "missing" | "rejected";
  technical: string;
}
export function dashboardIdentity(ref?: DashboardRef, entry?: Pick<PaneEntry, "scope" | "team">) {
  const scope = ref?.scope ?? entry?.scope;
  if (scope === "private")
    return {
      icon: User,
      label: ref?.installedFrom?.team
        ? `Private · copied from ${ref.installedFrom.team} team`
        : "Private",
    };
  if (scope === "team") return { icon: Users, label: `${ref?.team ?? entry?.team ?? ""} team` };
  return { icon: Box, label: "Built-in" };
}
export function slotFailureCopy(failure: SlotFailure) {
  switch (failure.kind) {
    case "timeout":
      return {
        happened: "It didn't finish starting within 30 seconds.",
        fix: "Add `codesense.ready()` after your page renders (or call `codesense.subscribe(...)`).",
      };
    case "script":
      return { happened: "The page hit an error while loading." };
    case "crash":
      return { happened: "The page stopped working after it opened." };
    case "connection":
      return { happened: "CodeSense couldn't open a secure connection to it." };
    case "missing":
      return {
        happened: "The file isn't there any more.",
        fix: "Choose another dashboard, or restore the file.",
      };
    case "rejected": {
      const cap = /unknown capability\s+"([^"]+)"/.exec(failure.technical)?.[1];
      if (cap)
        return {
          happened: "It asks for a permission CodeSense doesn't know, so it was never opened.",
          fix: `Remove or correct ${cap} in the page's codesense:capabilities.`,
        };
      if (failure.technical.includes("unknown slot"))
        return {
          happened: "It names a place in the app that doesn't exist, so it was never opened.",
        };
      if (failure.technical.includes("manifest/meta mismatch"))
        return { happened: "Its settings file and the page disagree, so it was never opened." };
      return { happened: "Its settings file couldn't be read, so it was never opened." };
    }
  }
}
export function SlotFallbackStrip({
  entry,
  surface,
  keepingPrevious = false,
  failure,
  reloading,
  attempts,
  triedAt,
  onReload,
  returnFocus,
}: {
  entry: PaneEntry;
  surface?: string;
  keepingPrevious?: boolean;
  failure: SlotFailure;
  reloading: boolean;
  attempts: number;
  triedAt?: Date;
  onReload: () => void;
  returnFocus: boolean;
}) {
  const [details, setDetails] = useState(false);
  const id = useId();
  const reload = useRef<HTMLButtonElement>(null);
  // A failed retry retains the same button node and returns focus only after an explicit retry.
  useEffect(() => {
    if (returnFocus && !reloading) reload.current?.focus({ preventScroll: true });
  }, [returnFocus, reloading]);
  const copy = slotFailureCopy(failure);
  const from = dashboardIdentity(entry.dashboardRef, entry).label;
  const changed = entry.modified ? shortTime(Date.parse(entry.modified)) : null;
  const fields = [
    ["What happened", copy.happened],
    ...(copy.fix ? [["To fix", copy.fix]] : []),
    ["File", entry.path],
    ["From", from],
    ...(changed
      ? [
          [
            "Changed",
            `${changed}${entry.installSourceVersion ? ` · copy of ${entry.installSourceVersion.slice(0, 7)}` : ""}`,
          ],
        ]
      : []),
    ["Technical", failure.technical],
    ...(attempts > 1
      ? [["Tried", `${attempts} times, last at ${triedAt ? shortTime(triedAt.getTime()) : ""}`]]
      : []),
  ];
  return (
    <InlineAlert
      severity="warning"
      role="status"
      aria-live="polite"
      className="@container/slot shrink-0 rounded-none border-b border-amber-500/25 [&>div]:min-w-0 [&>div]:flex-1"
      data-testid="slot-fallback-strip"
    >
      <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1.5 leading-[1.45]">
        <p className="min-w-0 flex-[1_1_200px] break-words">
          <span
            className="inline-block max-w-[22ch] truncate align-bottom font-medium"
            title={entry.title}
          >
            {entry.title}
          </span>{" "}
          pane {attempts > 1 ? "still didn't load" : "didn't load"}
          {keepingPrevious
            ? " — keeping the previous version."
            : surface
              ? ` — showing the built-in ${surface}.`
              : "."}
        </p>
        <div className="flex shrink-0 gap-1.5">
          <Button
            ref={reload}
            type="button"
            size="xs"
            variant="outline"
            className="text-foreground"
            aria-disabled={reloading}
            onClick={() => {
              if (!reloading) onReload();
            }}
          >
            {reloading ? (
              <LoaderCircle className="motion-safe:animate-spin" aria-hidden="true" />
            ) : (
              <RotateCw aria-hidden="true" />
            )}
            {reloading ? "Reloading…" : "Reload"}
          </Button>
          <Button
            type="button"
            size="xs"
            variant="outline"
            className="text-foreground"
            aria-expanded={details}
            aria-controls={id}
            onClick={() => setDetails((v) => !v)}
          >
            Details
            <ChevronDown className={details ? "rotate-180" : ""} aria-hidden="true" />
          </Button>
        </div>
        {details && (
          <div
            id={id}
            className="my-1 basis-full rounded-md border bg-background px-2.5 py-2 text-foreground"
          >
            <dl className="grid grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-0.5 @max-[300px]/slot:grid-cols-1 @max-[300px]/slot:[&>dt:not(:first-child)]:mt-1.5">
              {fields.map(([label, value]) => (
                <div key={label} className="contents">
                  <dt className="text-muted-foreground">{label}</dt>
                  <dd
                    className={`min-w-0 break-words [overflow-wrap:anywhere] ${label === "Technical" ? "font-mono" : ""}`}
                  >
                    {value}
                  </dd>
                </div>
              ))}
            </dl>
            <Button
              type="button"
              size="xs"
              variant="outline"
              className="mt-2"
              onClick={() =>
                void navigator.clipboard.writeText(
                  [entry.title, ...fields.map(([label, value]) => `${label}: ${value}`)].join("\n"),
                )
              }
            >
              <Copy aria-hidden="true" />
              Copy details
            </Button>
          </div>
        )}
      </div>
    </InlineAlert>
  );
}

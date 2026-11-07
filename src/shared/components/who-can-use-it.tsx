// SPDX-License-Identifier: MIT
import { useId } from "react";

import { cn } from "@/lib/utils";

/** The value that files a new item in your scratch space. */
export const ONLY_ME = "only-me";

/** One team a new item can go to. `value` is opaque to the field. */
export interface AudienceTeam {
  value: string;
  label: string;
}

/** A new item goes to the (first) team unless there is none. */
export function defaultAudience(teams: readonly AudienceTeam[]): string {
  return teams[0]?.value ?? ONLY_ME;
}

/**
 * The audience a form saves with: what the user picked (`null` = nothing yet), as long
 * as it is still offered; otherwise `fallback` (by default the first team, or Only me
 * when there is none). A `null` fallback means "no default": `""`, the form asks for a
 * pick. So a form opened before the team loaded still defaults to the team, and a team
 * that went away never silently stays chosen.
 */
export function resolveAudience(
  choice: string | null,
  teams: readonly AudienceTeam[],
  fallback: string | null = defaultAudience(teams),
): string {
  if (choice === ONLY_ME || teams.some((t) => t.value === choice)) return choice as string;
  return fallback ?? "";
}

/**
 * "Who can use it: ‹Team› / Only me" — the one field every create form uses where a
 * new item can go to a team or to your scratch space. Defaults to the team; "Only me"
 * keeps it in your scratch space for the open org. `value` `""` = not picked yet.
 */
export function WhoCanUseItField({
  value,
  onChange,
  teams,
  scratchOffFor,
  onlyMeUnavailable,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  teams: readonly AudienceTeam[];
  /** The team whose "Use my private items" switch is off: an Only me item won't apply there. */
  scratchOffFor?: string | null;
  /** Why Only me can't be picked here (the option is then disabled). */
  onlyMeUnavailable?: string | null;
  className?: string;
}) {
  const hintId = useId();
  const team = teams.find((t) => t.value === value);
  const hint = team
    ? `Everyone on ${team.label} can use it.`
    : value === ONLY_ME
      ? scratchOffFor
        ? `Kept in Private — private items are off for ${scratchOffFor}, so it won't apply there.`
        : "Kept in Private — only you see it, and only in this org."
      : "Pick who can use it.";
  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <label className="flex flex-wrap items-center gap-2 text-sm">
        <span className="text-xs text-muted-foreground">Who can use it</span>
        <select
          aria-label="Who can use it"
          aria-describedby={hintId}
          className="h-8 rounded-lg border border-input bg-background px-2 text-sm text-foreground outline-none focus-visible:border-ring disabled:opacity-60"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        >
          {value === "" && (
            <option value="" disabled>
              Pick…
            </option>
          )}
          {teams.map((t) => (
            <option key={t.value} value={t.value}>
              {t.label}
            </option>
          ))}
          <option value={ONLY_ME} disabled={!!onlyMeUnavailable}>
            Only me
          </option>
        </select>
      </label>
      <p id={hintId} data-testid="who-can-use-it-hint" className="text-xs text-muted-foreground">
        {hint}
      </p>
      {onlyMeUnavailable && (
        <p data-testid="who-can-use-it-only-me-reason" className="text-xs text-muted-foreground">
          Only me isn&apos;t available here: {onlyMeUnavailable}
        </p>
      )}
    </div>
  );
}

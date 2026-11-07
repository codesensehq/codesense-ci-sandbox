// SPDX-License-Identifier: MIT
import { useId } from "react";
import { Check } from "lucide-react";

import { cn } from "@/lib/utils";

/** One row of a sectioned popover menu (the title bar's identity menu). Focus and
 *  hover share one highlight; `useMenuRoving` moves focus with the pointer. */
export const MENU_ITEM =
  "flex w-full min-w-0 items-center gap-2.5 rounded-md px-2 py-[5px] text-left text-[13px] leading-5 outline-none transition-colors duration-100 hover:bg-foreground/[0.07] hover:text-foreground focus-visible:bg-foreground/[0.09] focus-visible:text-foreground disabled:pointer-events-none disabled:opacity-50";

/** The row you're on: a resting wash so it stays findable while another row is
 *  highlighted. Pair it with `MenuActiveMark`. */
export const MENU_ITEM_CURRENT = "bg-foreground/[0.05] font-medium text-foreground";

/** A secondary action pinned to a row's trailing edge (hide, disable): its own menu
 *  item, revealed on the row's hover or on its own keyboard focus. */
export const MENU_ROW_ACTION =
  "absolute right-1.5 shrink-0 rounded p-1 text-muted-foreground opacity-0 outline-none hover:bg-background/60 hover:text-foreground focus-visible:bg-background/60 focus-visible:text-foreground focus-visible:opacity-100 group-hover:opacity-100";

/** A labelled group of menu rows; `id` lets a shortcut open the menu focused here.
 *  `heading` replaces the plain label with richer content (it still names the group). */
export function MenuSection({
  id,
  label,
  heading,
  shortcut,
  children,
}: {
  id: string;
  label: string;
  heading?: React.ReactNode;
  /** Display form of the shortcut that opens the menu at this section. */
  shortcut?: string;
  children: React.ReactNode;
}) {
  const headingId = useId();
  return (
    <div role="group" aria-labelledby={headingId} data-menu-section={id} className="flex flex-col">
      <div className="flex min-h-6 items-center justify-between gap-2 px-2 pb-1 pt-0.5">
        <span
          id={headingId}
          aria-label={heading ? label : undefined}
          className="flex min-w-0 items-center gap-1.5 text-[11px] font-medium text-muted-foreground"
        >
          {heading ?? label}
        </span>
        {shortcut && <MenuShortcut>{shortcut}</MenuShortcut>}
      </div>
      {children}
    </div>
  );
}

/** A keystroke shown at a row's (or section label's) trailing edge, as a keycap. */
export function MenuShortcut({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <kbd
      className={cn(
        "ml-auto inline-flex h-[18px] shrink-0 items-center rounded border border-border/80 bg-foreground/[0.03] px-1 font-sans text-[10.5px] leading-none tracking-wide text-muted-foreground",
        className,
      )}
    >
      {children}
    </kbd>
  );
}

export function MenuSeparator() {
  return <div role="separator" className="-mx-1.5 my-1.5 h-px bg-border/80" />;
}

/** The leading column every row shares (a repo mark, an icon, or this spacer), so
 *  labels line up down the whole menu. */
export function MenuLeadSpacer() {
  return <span aria-hidden="true" className="w-5 shrink-0" />;
}

/** A row's leading icon, centred in the shared 20px lead column. */
export function MenuLeadIcon({ children }: { children: React.ReactNode }) {
  return (
    <span
      aria-hidden="true"
      className="flex w-5 shrink-0 justify-center text-muted-foreground transition-colors [button:focus-visible_&]:text-current [button:hover_&]:text-current"
    >
      {children}
    </span>
  );
}

/** The lead column of a row nested under the row above (a repo's teams): a guide
 *  line under the parent's mark, so the children read as its own. */
export function MenuTreeGuide() {
  return (
    <span aria-hidden="true" className="flex w-5 shrink-0 justify-center self-stretch">
      <span className="-my-[5px] w-px bg-border" />
    </span>
  );
}

/** The check marking the row you're on, at the row's trailing edge; marked in
 *  place, never hoisted. Pair it with `MENU_ITEM_CURRENT` on the active row. A row
 *  with a hover action hides the check while that action shows. */
export function MenuActiveMark({ active, className }: { active: boolean; className?: string }) {
  if (!active) return null;
  return (
    <Check
      size={14}
      strokeWidth={2.25}
      aria-hidden="true"
      className={cn("ml-auto shrink-0 text-current opacity-80", className)}
    />
  );
}

/** "1 needs you" / "3 need you". */
export function needsYouText(n: number): string {
  return `${n > 99 ? "99+" : n} ${n === 1 ? "needs" : "need"} you`;
}

/** The needs-you hue (the agents list's "Needs you"), pulled toward the text
 *  colour so an 11px count keeps its contrast in light and dark. */
export const NEEDS_TEXT = "text-[color-mix(in_oklab,var(--cs-status-needs)_80%,var(--foreground))]";

/** A count of things waiting on you, at a row's trailing edge. */
export function MenuNeedsCount({ count, testId }: { count: number; testId?: string }) {
  if (count <= 0) return null;
  return (
    <span
      data-testid={testId}
      aria-hidden="true"
      className={cn("ml-auto shrink-0 text-[11px] font-medium", NEEDS_TEXT)}
    >
      {needsYouText(count)}
    </span>
  );
}

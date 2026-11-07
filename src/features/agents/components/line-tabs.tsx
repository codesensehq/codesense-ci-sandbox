// SPDX-License-Identifier: MIT
import type { ComponentProps } from "react";

import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";

/**
 * The one tab strip of the Agents tab: kit Tabs in the line variant, the same for the
 * Agents | Schedule | Roles views, the agent's own tabs and Upcoming | Recent. Arrow keys,
 * Home and End move the selection, as the kit does.
 */
export function LineTabs({
  value,
  onValueChange,
  label,
  children,
  className,
  listClassName,
  testId,
  onReselect,
}: {
  value: string;
  onValueChange: (value: string) => void;
  label: string;
  children: React.ReactNode;
  className?: string;
  listClassName?: string;
  testId?: string;
  /** Called when the tab already selected is clicked again. */
  onReselect?: (value: string) => void;
}) {
  return (
    <Tabs
      value={value}
      onValueChange={onValueChange}
      onClick={(e) => {
        const tab = (e.target as HTMLElement).closest<HTMLElement>('[role="tab"]');
        if (tab?.dataset.state === "active" && tab.dataset.tabValue)
          onReselect?.(tab.dataset.tabValue);
      }}
      className={cn("gap-0", className)}
    >
      <TabsList
        variant="line"
        aria-label={label}
        data-testid={testId}
        className={cn("h-full w-fit gap-0.5 p-0", listClassName)}
      >
        {children}
      </TabsList>
    </Tabs>
  );
}

/** One tab of a {@link LineTabs} strip. */
export function LineTab({ className, ...props }: ComponentProps<typeof TabsTrigger>) {
  return (
    <TabsTrigger
      data-tab-value={props.value}
      className={cn(
        "h-full flex-none gap-1.5 rounded-sm px-3 text-sm after:!bottom-0 after:!inset-x-3 after:!transition-none data-active:text-foreground",
        className,
      )}
      {...props}
    />
  );
}

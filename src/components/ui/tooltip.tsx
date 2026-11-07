// SPDX-License-Identifier: MIT
import * as React from "react";
import { Tooltip as TooltipPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";
import { ScopeGate } from "@/shared/lib/warm-scopes";

function TooltipProvider({
  delayDuration = 0,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Provider>) {
  return (
    <TooltipPrimitive.Provider
      data-slot="tooltip-provider"
      delayDuration={delayDuration}
      {...props}
    />
  );
}

function Tooltip({ ...props }: React.ComponentProps<typeof TooltipPrimitive.Root>) {
  return <TooltipPrimitive.Root data-slot="tooltip" {...props} />;
}

function TooltipTrigger({ ...props }: React.ComponentProps<typeof TooltipPrimitive.Trigger>) {
  return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />;
}

function InfoTip({
  label,
  side,
  align,
  collisionPadding,
  contentClassName,
  children,
  ...props
}: {
  label: React.ReactNode;
  side?: React.ComponentProps<typeof TooltipPrimitive.Content>["side"];
  align?: React.ComponentProps<typeof TooltipPrimitive.Content>["align"];
  collisionPadding?: React.ComponentProps<typeof TooltipPrimitive.Content>["collisionPadding"];
  contentClassName?: string;
  children: React.ReactNode;
} & React.ComponentProps<typeof TooltipPrimitive.Root>) {
  // A falsy label (e.g. a conditional `cond ? reason : undefined`) is a passthrough:
  // render the child alone rather than a Tooltip with an empty bubble.
  if (!label) return <>{children}</>;
  return (
    <Tooltip {...props}>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent
        side={side}
        align={align}
        collisionPadding={collisionPadding}
        className={contentClassName}
      >
        {label}
      </TooltipContent>
    </Tooltip>
  );
}

function TooltipContent({
  className,
  sideOffset = 0,
  children,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Content>) {
  return (
    <ScopeGate>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          data-slot="tooltip-content"
          data-surface="overlay"
          sideOffset={sideOffset}
          className={cn(
            "z-50 inline-flex w-fit max-w-xs origin-(--radix-tooltip-content-transform-origin) items-center gap-1.5 rounded-md bg-foreground px-3 py-1.5 text-xs text-background has-data-[slot=kbd]:pr-1.5 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 **:data-[slot=kbd]:relative **:data-[slot=kbd]:isolate **:data-[slot=kbd]:z-50 **:data-[slot=kbd]:rounded-sm data-[state=delayed-open]:animate-in data-[state=delayed-open]:fade-in-0 data-[state=delayed-open]:zoom-in-95 data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            className,
          )}
          {...props}
        >
          {children}
          <TooltipPrimitive.Arrow className="z-50 size-2.5 translate-y-[calc(-50%_-_2px)] rotate-45 rounded-[2px] bg-foreground fill-foreground" />
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </ScopeGate>
  );
}

export { InfoTip, Tooltip, TooltipContent, TooltipProvider, TooltipTrigger };

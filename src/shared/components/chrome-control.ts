// SPDX-License-Identifier: MIT
/**
 * One spec for the app chrome's icon controls (title bar, the CLI's tray controls):
 * a 28px square hit area, a 16px glyph at a 1.75 stroke, one hover wash, one
 * pressed/open state and one focus ring — so neighbouring controls from different
 * features line up as a single set.
 */
export const CHROME_ICON_BUTTON =
  "relative inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none transition-colors duration-150 hover:bg-foreground/[0.07] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring aria-pressed:bg-foreground/10 aria-pressed:text-foreground data-[state=open]:bg-foreground/10 data-[state=open]:text-foreground";

/** Glyph size + stroke for a chrome icon control. */
export const CHROME_ICON = { size: 16, strokeWidth: 1.75 } as const;

/** The hairline between groups of chrome controls. */
export const CHROME_DIVIDER = "mx-1 h-4 w-px shrink-0 bg-border";

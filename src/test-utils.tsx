// SPDX-License-Identifier: MIT
import type { ReactElement, ReactNode } from "react";
import { render as rtlRender } from "@testing-library/react";
import type { RenderOptions } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";

/**
 * Shared test `render` — wraps the tree in the app's global providers so a
 * component that renders a tooltip (Radix `Tooltip`/`InfoTip` or the
 * overlay-shell `ChromeIconButton`) doesn't crash with "Tooltip must be used
 * within TooltipProvider" when rendered in isolation (SW-FX-80 AC-1 removed the
 * per-surface local providers in favor of the single app-root one).
 *
 * `delayDuration` inherits the `TooltipProvider` default (0ms — instant), the
 * same value `AppShell` provides in production.
 *
 * Re-exports everything else from `@testing-library/react` (`screen`,
 * `fireEvent`, `within`, `waitFor`, `act`, `cleanup`, …) so tests swap only the
 * import source, never individual named imports.
 */
function Providers({ children }: { children: ReactNode }) {
  return <TooltipProvider>{children}</TooltipProvider>;
}

export function render(ui: ReactElement, options?: RenderOptions) {
  return rtlRender(ui, { wrapper: Providers, ...options });
}

export * from "@testing-library/react";

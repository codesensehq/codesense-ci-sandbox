// SPDX-License-Identifier: MIT
import { render, type RenderOptions, type RenderResult } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";

import { TooltipProvider } from "@/components/ui/tooltip";

/**
 * Wraps the rendered tree in the same providers the real `<AppShell>`
 * supplies at runtime (currently: Radix `TooltipProvider`). Test files
 * that render chrome components — `TitleBar`, `StatusBar`, `FileTree` —
 * in isolation should use this instead of bare `render`, since those
 * components moved their local `TooltipProvider` wrappers into
 * `AppShell` as part of the C.15 review consolidation.
 */
function Providers({ children }: { children: ReactNode }) {
  return <TooltipProvider>{children}</TooltipProvider>;
}

export function renderWithProviders(
  ui: ReactElement,
  options?: Omit<RenderOptions, "wrapper">,
): RenderResult {
  return render(ui, { wrapper: Providers, ...options });
}

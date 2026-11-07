// SPDX-License-Identifier: MIT
import type { SectionKey } from "@/features/settings/lib/types";

/**
 * Section heading rendered inside the settings content pane.
 * Keeps `border-b` for visual structure; `uppercase` and `tracking-wide`
 * are dropped — the sidebar already provides "where am I" context, so
 * the header reads as a quiet content anchor rather than a separator.
 */
export function SectionHeader({ id, children }: { id: SectionKey; children: React.ReactNode }) {
  return (
    <h2
      id={`settings-section-${id}`}
      className="border-b border-border py-2 text-sm font-semibold text-muted-foreground"
    >
      {children}
    </h2>
  );
}

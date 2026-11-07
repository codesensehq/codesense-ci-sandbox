// SPDX-License-Identifier: MIT
import { BookOpen } from "lucide-react";

import { cn } from "@/lib/utils";
import { isCodesenseDocs } from "@/shared/lib/codesense-docs";

/** Up to two initials: the first letters of the first two words, else the first two letters. */
export function repoInitials(name: string): string {
  const words = name
    .trim()
    .split(/[\s\-_./]+/)
    .filter(Boolean);
  if (words.length === 0) return "?";
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

/** The hues a repo mark may take: evenly spread, skipping the yellow-olive band
 *  that turns muddy at a mark's lightness. */
export const REPO_HUES = [25, 55, 150, 180, 215, 255, 290, 330] as const;

/** A stable hue for a slug, so a repo keeps its colour across sessions. */
export function repoHue(slug: string): number {
  let h = 0;
  for (let i = 0; i < slug.length; i++) h = (h * 31 + slug.charCodeAt(i)) >>> 0;
  return REPO_HUES[h % REPO_HUES.length];
}

/**
 * A context repo's mark: a small rounded tile with its initials on a hue derived
 * from its slug, so each repo is recognisable at a glance in the switcher and the
 * title bar. The bundled CodeSense docs get their book instead of initials.
 *
 * The initials render as generated content (not a text node), so a row's text —
 * which the menu's type-ahead matches on — stays the repo's name.
 */
export function ContextRepoMark({
  slug,
  name,
  size = 18,
  className,
}: {
  slug: string;
  name: string;
  size?: number;
  className?: string;
}) {
  const docs = isCodesenseDocs(slug);
  return (
    <span
      aria-hidden="true"
      data-testid="context-repo-mark"
      data-initials={docs ? undefined : repoInitials(name)}
      style={
        {
          width: size,
          height: size,
          fontSize: Math.round(size * 0.46),
          "--repo-hue": repoHue(slug),
        } as React.CSSProperties
      }
      className={cn(
        "inline-flex shrink-0 select-none items-center justify-center rounded-[5px] font-semibold leading-none tracking-tight",
        "before:content-[attr(data-initials)]",
        docs
          ? "bg-foreground/10 text-foreground"
          : "bg-[oklch(0.56_0.12_var(--repo-hue))] text-white shadow-[inset_0_0_0_1px_oklch(1_0_0/0.12)]",
        className,
      )}
    >
      {docs && <BookOpen size={Math.round(size * 0.62)} strokeWidth={2} />}
    </span>
  );
}

// SPDX-License-Identifier: MIT
// SW-GT-3 · The ONE Lucide name → component resolver. GT-1 ships a state's `icon`
// as a Lucide NAME string (so the Rust taxonomy loader and the DX-4 HTML iframe
// dashboards can carry it without a React dependency — mirrors the icons-as-names
// convention in `tag-display.ts`). The interactive FE needs the component, so this
// maps a curated, STATIC set of names to their named-imported components.
//
// Why a hand-curated static Record (not `import * as Icons from "lucide-react"`,
// not `lucide-react/dynamic`):
//   - `import *` pulls ~1500 icon modules into the renderer bundle — a real
//     memory regression (see the 7GB-renderer saga). Named imports tree-shake to
//     exactly the icons listed here.
//   - `lucide-react/dynamic` resolves each icon via an async import → a per-icon
//     flicker in hot lists (board columns, the status switcher). A synchronous
//     lookup keeps every render path O(1) and paint-stable.
//
// Written generically (name in, component out, total via a fallback) so other
// name-keyed icon maps (SW-GT-20, future taxonomies) can adopt the same seam.

import { createElement } from "react";
import {
  // The 7 default-taxonomy icons (must match `status-config.ts` / Rust `default_meta`).
  CircleDashed,
  CircleDot,
  Circle,
  Ban,
  Eye,
  ShieldCheck,
  CheckCircle2,
  // A nameable dashboard icon.
  LayoutDashboard,
  // Curated status palette — sensible icons a team is likely to name for a custom
  // state. Extend this list (never switch to a dynamic/star import) as real teams
  // ask for more.
  CircleCheck,
  CircleCheckBig,
  CircleX,
  CirclePlay,
  CirclePause,
  CircleAlert,
  CircleHelp,
  CircleDotDashed,
  Clock,
  Hourglass,
  Loader,
  Play,
  Pause,
  Check,
  X,
  Flag,
  Rocket,
  Target,
  ListChecks,
  ListTodo,
  GitPullRequest,
  GitMerge,
  Bug,
  FlaskConical,
  Beaker,
  Sparkles,
  TriangleAlert,
  OctagonAlert,
  Archive,
  Inbox,
  PenLine,
  Send,
  ThumbsUp,
  Lock,
  // SW-AU-77I: the per-type default glyphs for org tag rows — must match the
  // `TAG_TYPE_DEFAULTS` icon names in `tag-schema/tag-display.ts` (ListChecks +
  // CircleDot are already imported above). A tag that declares an icon outside this
  // curated set still resolves to the neutral fallback, never blank.
  Tag,
  List,
  ArrowUpNarrowWide,
  Calendar,
  Link,
  type LucideIcon,
  type LucideProps,
} from "lucide-react";

/** Curated static name → component map. Named imports only (tree-shakeable). */
const LUCIDE_ICON_MAP: Record<string, LucideIcon> = {
  CircleDashed,
  CircleDot,
  Circle,
  Ban,
  Eye,
  ShieldCheck,
  CheckCircle2,
  LayoutDashboard,
  CircleCheck,
  CircleCheckBig,
  CircleX,
  CirclePlay,
  CirclePause,
  CircleAlert,
  CircleHelp,
  CircleDotDashed,
  Clock,
  Hourglass,
  Loader,
  Play,
  Pause,
  Check,
  X,
  Flag,
  Rocket,
  Target,
  ListChecks,
  ListTodo,
  GitPullRequest,
  GitMerge,
  Bug,
  FlaskConical,
  Beaker,
  Sparkles,
  TriangleAlert,
  OctagonAlert,
  Archive,
  Inbox,
  PenLine,
  Send,
  ThumbsUp,
  Lock,
  // SW-AU-77I: org-tag per-type default glyphs (ListChecks + CircleDot above).
  Tag,
  List,
  ArrowUpNarrowWide,
  Calendar,
  Link,
};

/** The fallback for an unknown / typo'd / iframe-only name — a neutral circle, so
 *  a custom state never renders blank or throws. Kept distinct from any default
 *  state icon so an accidental fallback is visually recognizable in review. */
export const FALLBACK_LUCIDE_ICON: LucideIcon = Circle;

/**
 * Resolve a Lucide icon NAME to its component. Total: an unknown name (custom
 * taxonomy icon not in the curated set, or a typo) yields {@link FALLBACK_LUCIDE_ICON},
 * never a crash or blank. Synchronous + O(1) — safe on any render path.
 */
export function resolveLucideIcon(name: string | null | undefined): LucideIcon {
  if (!name) return FALLBACK_LUCIDE_ICON;
  return LUCIDE_ICON_MAP[name] ?? FALLBACK_LUCIDE_ICON;
}

/** Test/introspection helper — the set of curated names this resolver knows. */
export function knownLucideIconNames(): string[] {
  return Object.keys(LUCIDE_ICON_MAP);
}

/**
 * Render a Lucide icon BY NAME. A stable, module-level component — so callers pass
 * the taxonomy's icon-name string and forward Lucide props, instead of resolving a
 * component reference in their own render body (which the
 * `react-hooks/static-components` rule forbids, since it can't prove the resolved
 * reference is stable). Uses `createElement` on the resolved (static) component.
 */
export function LucideByName({ name, ...props }: LucideProps & { name?: string | null }) {
  return createElement(resolveLucideIcon(name), props);
}

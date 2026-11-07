// SPDX-License-Identifier: MIT
import { AlertTriangle, AlertCircle, CheckCircle2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { HealthVerdict } from "@/shared/lib/connectivity/types";

/**
 * SW-DX-8 — the single source for a connectivity-health badge's icon + label +
 * color (AC-8), mirroring the pipeline's one-config-feeds-badge pattern. There
 * is no semantic `--cs-*` health token, so — like the sibling live/stale badge
 * in `cs-query-view.tsx` — we use theme-consistent Tailwind color utilities
 * (red / amber / emerald) rather than hex, so the badge tracks light/dark.
 */
export const HEALTH_CONFIG: Record<
  HealthVerdict,
  { icon: LucideIcon; label: string; badgeClass: string; rowTintClass: string }
> = {
  "context-desert": {
    icon: AlertTriangle,
    label: "Will be built blind",
    badgeClass: "text-red-600 dark:text-red-400 bg-red-500/10 border-red-500/30",
    rowTintClass: "bg-red-500/5",
  },
  thin: {
    icon: AlertCircle,
    label: "Thin coverage",
    badgeClass: "text-amber-600 dark:text-amber-400 bg-amber-500/10 border-amber-500/30",
    rowTintClass: "",
  },
  healthy: {
    icon: CheckCircle2,
    label: "Healthy",
    badgeClass: "text-emerald-600 dark:text-emerald-400 bg-emerald-500/10 border-emerald-500/30",
    rowTintClass: "",
  },
};

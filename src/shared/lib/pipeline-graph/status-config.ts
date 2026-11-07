// SPDX-License-Identifier: MIT
import { CircleDashed, Circle, CircleDot, Eye, ShieldCheck, CheckCircle2, Ban } from "lucide-react";
import type { PipelineStatus } from "./types";
import { PIPELINE_STATUSES } from "./types";
import type { StatusRole, StatusState } from "@/tauri/commands";

/** SW-GT-3: the neutral slate used to render a status with no taxonomy color — an
 *  off-taxonomy (soft-drift) status: the drift column, an off-taxonomy status icon,
 *  and an off-taxonomy status-mix segment. One source so they can't drift apart. */
export const NEUTRAL_STATUS_COLOR = "#94a3b8";

/**
 * Per-status visual config. `color` is used for the inline status icon
 * next to the story title. The left-edge spine is now colored by
 * document type (see `DOC_TYPE_ICON` in `doc-type-icon.ts`).
 *
 * Hex values are locked so statuses read the same across viewer themes
 * (status semantics shouldn't drift with theme choice). If we ever want
 * theme-aware status colors we can promote these to CSS vars.
 */
export const STATUS_CONFIG: Record<
  PipelineStatus,
  { icon: React.ElementType; color: string; label: string }
> = {
  planning: { icon: CircleDashed, color: "#64748b", label: "Planning" },
  todo: { icon: CircleDot, color: "#94a3b8", label: "Todo" },
  "in-progress": { icon: Circle, color: "#0ea5e9", label: "In progress" },
  blocked: { icon: Ban, color: "#ef4444", label: "Blocked" },
  review: { icon: Eye, color: "#8b5cf6", label: "Review" },
  qa: { icon: ShieldCheck, color: "#f59e0b", label: "QA" },
  done: { icon: CheckCircle2, color: "#10b981", label: "Done" },
};

// ---------------------------------------------------------------------------
// SW-GT-3 · The default status taxonomy seed
//
// The status-taxonomy snapshot store (`status-taxonomy-store.ts`) is seeded
// synchronously at import with these 7 states so the switchers and board paint
// instantly — identical to the pre-taxonomy behavior — before the async
// per-team hydration lands. This is the ONLY remaining role of
// `PIPELINE_STATUSES` / `STATUS_CONFIG`: the seed. Live consumers read the
// snapshot, never these constants.
//
// The `icon` here is a Lucide NAME string (GT-1's wire contract), resolved to a
// component via `resolveLucideIcon`. `role` + `order` + the icon names MUST match
// the Rust `default_meta` (`status_taxonomy.rs`) — AC-8b asserts this parity so
// the board defaults can't silently drift from `validate_story`'s defaults.
// ---------------------------------------------------------------------------

/** Default role per default id — mirrors Rust `default_meta`. `review`+`qa` are
 *  both the `review` role; `planning`+`todo` both `not_started`. */
const DEFAULT_ROLE: Record<PipelineStatus, StatusRole> = {
  planning: "not_started",
  todo: "not_started",
  "in-progress": "active",
  blocked: "blocked",
  review: "review",
  qa: "review",
  done: "done",
};

/** Default Lucide icon NAME per default id — mirrors Rust `default_meta`. The
 *  component form lives in `STATUS_CONFIG.icon`; the name is what the taxonomy
 *  wire model (and this seed) carries. */
const DEFAULT_ICON_NAME: Record<PipelineStatus, string> = {
  planning: "CircleDashed",
  todo: "CircleDot",
  "in-progress": "Circle",
  blocked: "Ban",
  review: "Eye",
  qa: "ShieldCheck",
  done: "CheckCircle2",
};

/**
 * The shipped default taxonomy — one `StatusState` per default id, in
 * `PIPELINE_STATUSES` order (`order` = index, matching the Rust id-index). Label
 * and color are single-sourced from `STATUS_CONFIG`; role + icon-name from the
 * tables above. The snapshot store seeds from this.
 */
export const DEFAULT_STATUS_STATES: StatusState[] = PIPELINE_STATUSES.map((id, order) => ({
  id,
  label: STATUS_CONFIG[id].label,
  color: STATUS_CONFIG[id].color,
  icon: DEFAULT_ICON_NAME[id],
  order,
  role: DEFAULT_ROLE[id],
}));

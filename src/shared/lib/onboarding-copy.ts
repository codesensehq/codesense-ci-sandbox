// SPDX-License-Identifier: MIT
/**
 * SW-OB-5: canonical onboarding microcopy used across 2+ surfaces. Single
 * source of truth so the wording can't drift between the surfaces that show
 * it. One-off onboarding strings stay inline at their call site — a constant
 * for a single-use string is indirection, not DRY.
 */

/**
 * "What a team is." Shown in the team-management tab create form
 * and the choose-your-team empty state (`team-workspace-empty-state.tsx`).
 * Source of the finalized wording: onboarding-ux-design.md Screen 4a (L118).
 */
export const TEAM_EXPLAINER =
  "A team groups the repos and context for one slice of work — start with one; you can add more later.";

/**
 * SW-OB-3 (AC-3): "what attaching a local clone does." Shown on every
 * local-clone attach surface — the `NoAttachedCloneState` onboarding gate
 * (`team-workspace-empty-state.tsx`), `AttachLocalCloneDialog`, and
 * `NoLocalCloneModal` — so the consequence framing can't drift between them.
 */
export const LOCAL_CLONE_EXPLAINER =
  "Your local checkout is what codesense shows alongside the team's context.";

/**
 * SW-LN-29 (AC-1): where the team context lands for a repo attached from the
 * user's own folder. Shown in the attach success state and on the attached
 * repo's row, so it reads the same everywhere. CodeSense never writes into that
 * main checkout. SW-XS-11: nothing is delivered on its own; an agent gets what
 * its definition declares when it starts, and more only by an explicit call.
 */
export const CONTEXT_LANDS_ATTACHED =
  "Agents you start in worktrees CodeSense opens for this repo get the context their definition declares. Your main checkout isn't touched.";

/** SW-LN-29 (AC-1): the same, for a clone CodeSense made and manages ("Clone for me"). */
export const CONTEXT_LANDS_MANAGED =
  "CodeSense manages this clone. Agents you start in worktrees CodeSense opens for this repo get the context their definition declares.";

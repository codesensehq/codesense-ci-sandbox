// SPDX-License-Identifier: MIT
/**
 * The one button. Primary is the default variant (one per surface), secondary is
 * `outline` (Cancel is always outline), destructive is `destructive`. Before this
 * there were two wrappers — the base `Button` and an `AppButton` that only added a
 * pointer cursor; the cursor now lives in the base, so both names are the same
 * component. See the consistency spec, Rule 2.
 */
export { Button as AppButton } from "@/components/ui/button";

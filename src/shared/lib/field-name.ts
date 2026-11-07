// SPDX-License-Identifier: MIT
/**
 * SW-AU-77E · The ONE client-side field-name rule, shared by the metadata-drawer
 * add-form and the Settings inline-add so the same input reconciles onto the same
 * declared field. Mirrors the backend `canonical_field_key` for an ASCII identifier
 * input — the backend remains the authority (it canonicalizes + reconciles whatever it
 * receives); this is the early, in-form validation that keeps the two UI paths in sync.
 */

/** The identifier form a declared field name must reduce to (lowercase word chars). */
export const FIELD_NAME_PATTERN = /^[a-z0-9_]+$/;

/** Canonicalize a raw name to the backend's identifier form (trim + lowercase). */
export function canonicalizeFieldName(name: string): string {
  return name.trim().toLowerCase();
}

/** Is `name` a valid field identifier once canonicalized? */
export function isValidFieldName(name: string): boolean {
  return FIELD_NAME_PATTERN.test(canonicalizeFieldName(name));
}

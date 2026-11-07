// SPDX-License-Identifier: MIT
/**
 * AC-20 — the agent-agnostic labeling contract. Vendor/product names that must
 * never appear in result/structural copy or the ownership chip ("Finalized",
 * "Agent", never a vendor). The live chat may show a real agent name; these
 * view-model labels never do. Single source for the guard and its tests so the
 * pattern can't drift across the surfaces that assert it.
 */
export const VENDOR_NAME_RE = /\b(claude|anthropic|openai|gpt|gemini|copilot|cursor|codex)\b/i;

/** True if the text contains a vendor/agent product name (AC-20 violation). */
export function containsVendorName(text: string): boolean {
  return VENDOR_NAME_RE.test(text);
}

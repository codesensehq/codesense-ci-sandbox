// SPDX-License-Identifier: MIT
/**
 * SV-AGP-6: with no saved dock choice the dock shows the built-in HTML `agent-list`, not the
 * classic native list. One switch for the whole app (the dock, the slot reader and Settings all
 * resolve a missing choice through it). Off until the founder's walk approves the HTML list as the
 * default; a saved choice always wins.
 */
export const DOCK_DEFAULT_HTML_AGENT_LIST = false;

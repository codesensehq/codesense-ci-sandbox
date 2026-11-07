// SPDX-License-Identifier: MIT
// ---------------------------------------------------------------------------
// tile-chrome-fit — the width budget for one terminal tile's chrome bar.
//
// The chrome is `[identity] [roll-up] [tab pills][+] [⌄+N] [⋮] [grid][hint][zoom][–][×]`.
// Narrow tiles (6-up, a squeezed split) used to let the flex row decide what to
// drop, and it dropped the wrong things: the tab strip collapsed to zero width
// and clipped the "+" and every pill (status dots included). This module decides
// up-front what renders inline at a given width so that:
//   - the "+" and every tile control are always on screen, or folded into ⋮;
//   - every tab's status is always visible: on its pill, on the "+N" trigger, or
//     (when even "+N" folds into ⋮) on the tile roll-up dot;
//   - a pill is never narrower than its dot plus a few characters of name.
//
// The widths below are the SAME numbers the chrome applies as inline sizes, so
// this is the real layout math, not an estimate of it.
// ---------------------------------------------------------------------------

/** Width of one icon control in the right cluster (⋮, grid, zoom, –, ×). */
export const CHROME_CONTROL_W = 20;
/** The "+" new-terminal button. */
export const CHROME_NEW_TAB_W = 24;
/** The overflow "⌄ +N" trigger. */
export const CHROME_OVERFLOW_W = 40;
/** The tile roll-up status dot slot. */
export const CHROME_ROLLUP_W = 14;
/** Horizontal padding of the right cluster (px-1 on each side). */
export const CHROME_CLUSTER_PAD_W = 8;
/** The zoomed tile's "+N hidden" hint, full and compact ("+N" + dot). */
export const CHROME_HIDDEN_HINT_W = 64;
export const CHROME_HIDDEN_HINT_COMPACT_W = 32;
/** Room the identity block keeps (status icon + a middle-truncated id) before
 *  the tab strip may take it. Below this it yields: status beats the label. */
export const CHROME_IDENTITY_MIN_W = 64;
/** Average rendered width of one text-xs character, rounded up so estimates of
 *  how many characters fit err short (used by middle truncation). */
export const TEXT_XS_CHAR_W = 7;
/** Room for a 22px agent face, a 6px gap, 44px of padding/close room and a
 *  readable 26px truncated name. Plain terminal icons use the same conservative slot. */
export const TAB_PILL_MIN_W = 98;
export const TAB_NAME_MIN_W = TAB_PILL_MIN_W - 72;

/** Count caps: past these a strip overflows even when there is room, so pills
 *  stay a readable width instead of splitting the space ever thinner. */
export const MAX_VISIBLE_TABS = 4;
export const MAX_VISIBLE_TABS_ZOOMED = 8;

export interface TileChromeFitInput {
  /** Measured chrome width in CSS px; `null` before the first measurement. */
  width: number | null;
  tabCount: number;
  /** Count cap for the strip (MAX_VISIBLE_TABS or MAX_VISIBLE_TABS_ZOOMED). */
  maxTabs: number;
  hasIdentity: boolean;
  /** The ⋮ menu has entries of its own (view toggles), so it renders anyway. */
  hasViewOptions: boolean;
  /** How many window controls (grid, zoom, minimize, close) the tile carries. */
  windowControlCount: number;
  /** The zoomed tile's "+N hidden" hint is present. */
  hasHiddenHint: boolean;
}

export interface TileChromeFit {
  /** Pills shown inline (the active tab is pinned into this window). */
  visibleTabs: number;
  /** Window controls (grid/zoom/–/×) are folded into the ⋮ menu. */
  collapseControls: boolean;
  /** Even the "⌄ +N" trigger doesn't fit: the tabs are listed in ⋮ instead. */
  tabsInMenu: boolean;
  /** The ⋮ menu renders (view toggles, or anything folded into it). */
  showKebab: boolean;
  /** Render the "+N hidden" hint in its compact form. */
  compactHiddenHint: boolean;
  /** The tabs are folded into ⋮ → the tile roll-up dot carries their status
   *  (otherwise pills and the "+N" already show every tab's status). */
  showRollup: boolean;
}

/** Fit the chrome's contents to `width`. Inactive tabs always go to "+N"
 *  first; then, to keep the ACTIVE tab's pill, the window controls fold into ⋮,
 *  and then the identity label gives up its reserve (the active tab's name
 *  matters more than the id). If the active pill can't be kept either way, the
 *  controls come back inline when they fit (with every tab in "+N"); failing
 *  that they fold, the label yields, and last the "+N" trigger itself folds
 *  into ⋮ with the roll-up dot carrying status. The "+" and ⋮ never fold. */
export function fitTileChrome(input: TileChromeFitInput): TileChromeFit {
  const { width, tabCount, maxTabs, hasIdentity, hasViewOptions, windowControlCount } = input;

  // Unmeasured: the classic count-only split, controls inline.
  if (width === null) {
    const visibleTabs = Math.min(tabCount, maxTabs);
    return {
      visibleTabs,
      collapseControls: false,
      tabsInMenu: false,
      showKebab: hasViewOptions,
      compactHiddenHint: false,
      showRollup: false,
    };
  }

  // Space left for [roll-up][pills][⌄+N] once the given fixed pieces are placed.
  const stripSpace = (collapse: boolean, identityReserve: number) => {
    const kebab = hasViewOptions || collapse ? CHROME_CONTROL_W : 0;
    const controls = collapse ? 0 : windowControlCount * CHROME_CONTROL_W;
    const hint = input.hasHiddenHint
      ? collapse
        ? CHROME_HIDDEN_HINT_COMPACT_W
        : CHROME_HIDDEN_HINT_W
      : 0;
    return (
      width - CHROME_CLUSTER_PAD_W - CHROME_NEW_TAB_W - kebab - controls - hint - identityReserve
    );
  };

  // How many pills fit in `space`, or null when the layout doesn't fit at all.
  const pillsIn = (space: number): number | null => {
    if (tabCount === 0) return space >= 0 ? 0 : null;
    if (tabCount <= maxTabs && tabCount * TAB_PILL_MIN_W <= space) return tabCount;
    const rest = space - CHROME_OVERFLOW_W;
    if (rest < 0) return null;
    return Math.min(Math.floor(rest / TAB_PILL_MIN_W), maxTabs, tabCount - 1);
  };

  const identity = hasIdentity ? CHROME_IDENTITY_MIN_W : 0;
  const build = (visibleTabs: number, collapse: boolean, tabsInMenu = false): TileChromeFit => ({
    visibleTabs,
    collapseControls: collapse,
    tabsInMenu,
    showKebab: hasViewOptions || collapse || tabsInMenu,
    compactHiddenHint: collapse,
    showRollup: tabsInMenu,
  });

  // 1. Everything inline, as long as the active tab keeps its pill.
  const inline = pillsIn(stripSpace(false, identity));
  if (inline !== null && (inline >= 1 || tabCount === 0)) return build(inline, false);

  // 2. To keep the active pill: fold the controls, then drop the label reserve.
  for (const reserve of [identity, 0]) {
    const n = pillsIn(stripSpace(true, reserve));
    if (n !== null && n >= 1) return build(n, true);
  }

  // 3. The active pill can't stay: keep the controls inline if they fit at all…
  if (inline !== null) return build(inline, false);
  // …else fold them, then drop the label reserve.
  for (const reserve of [identity, 0]) {
    const n = pillsIn(stripSpace(true, reserve));
    if (n !== null) return build(n, true);
  }

  // 4. Not even "⌄ +N" fits: the tabs move into ⋮, the roll-up dot keeps status.
  return build(0, true, tabCount > 0);
}

/** Split a label for CSS middle truncation: `head` ellipsizes, `tail` never
 *  shrinks — so a narrow `SW-FX-101-lag` renders `SW-FX…01-lag`, and siblings
 *  that share a prefix stay distinct. Short labels (≤ 8 chars) don't split.
 *
 *  `availableChars` (how many characters the box holds, when measured) caps the
 *  tail so it can never spill: it takes at most half of what's left after the
 *  ellipsis, and a label that fits isn't split at all. */
export function splitForMiddleTruncation(
  text: string,
  availableChars?: number,
): { head: string; tail: string } {
  if (text.length <= 8) return { head: text, tail: "" };
  if (availableChars !== undefined && text.length <= availableChars) {
    return { head: text, tail: "" };
  }
  let tailLen = Math.min(6, Math.ceil(text.length * 0.4));
  if (availableChars !== undefined) {
    tailLen = Math.min(tailLen, Math.max(1, Math.floor((availableChars - 1) / 2)));
  }
  return { head: text.slice(0, text.length - tailLen), tail: text.slice(text.length - tailLen) };
}

/** The label of an overflow "+N" trigger, shared by its aria-label and title:
 *  "3 more tabs", or what they hold: "3 more tabs: 2 need you · 1 working". */
export function overflowLabel(
  count: number,
  [singular, plural]: readonly [string, string],
  status?: string,
): string {
  const base = `${count} more ${count === 1 ? singular : plural}`;
  return status ? `${base}: ${status}` : base;
}

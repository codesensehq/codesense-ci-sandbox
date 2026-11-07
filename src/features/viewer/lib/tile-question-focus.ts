// SPDX-License-Identifier: MIT
/**
 * DOM helpers for a tile's question banner (`TileNeedsYouSlot`). The banner marks
 * its root with `data-tile-question` and carries its question's correlation id;
 * its host tile frame carries `data-pane-id`. Kept framework-free so the keyboard
 * shortcut registry and the banner share one definition of "the banner's first
 * control".
 */

import { isInHiddenTeam } from "@/shared/lib/warm-scopes";

export const TILE_QUESTION_ATTR = "data-tile-question";
export const TILE_QUESTION_ASK_ATTR = "data-correlation-id";

const FOCUSABLE =
  'button:not([disabled]), textarea:not([disabled]), input:not([disabled]), [href], [tabindex]:not([tabindex="-1"])';

/** The control the banner focuses first: the first choice, else the answer
 *  field, else any focusable control. */
export function firstQuestionControl(root: HTMLElement | null): HTMLElement | null {
  if (!root) return null;
  return (
    root.querySelector<HTMLElement>('[data-testid="ask-option"]') ??
    root.querySelector<HTMLElement>("textarea") ??
    root.querySelector<HTMLElement>(FOCUSABLE)
  );
}

/** Inside a tile collapsed by another tile's zoom. */
function isZoomCollapsed(el: HTMLElement): boolean {
  return el.closest('[data-zoom="collapsed"]') !== null;
}

function isRendered(el: HTMLElement): boolean {
  if (isZoomCollapsed(el)) return false;
  return typeof el.checkVisibility === "function" ? el.checkVisibility() : true;
}

const paneOf = (el: HTMLElement): string | null =>
  el.closest("[data-pane-id]")?.getAttribute("data-pane-id") ?? null;

const askNumber = (el: HTMLElement): number => {
  const m = /(\d+)$/.exec(el.getAttribute(TILE_QUESTION_ASK_ATTR) ?? "");
  return m ? Number(m[1]) : Number.MAX_SAFE_INTEGER;
};

/** Focus `banner`'s first control; true only if focus actually landed there. */
function focusBanner(banner: HTMLElement): boolean {
  const control = firstQuestionControl(banner);
  if (!control) return false;
  control.focus();
  return document.activeElement === control;
}

/** How many frames the chord waits for a revealed tile's banner to render. */
const REVEAL_FRAMES = 10;

const nextFrame = (): Promise<void> =>
  new Promise((resolve) => requestAnimationFrame(() => resolve()));

/**
 * Move keyboard focus into a question banner: the one in `preferredWorkspaceId`'s
 * tile first, then the oldest question on screen. Banners in a tile collapsed by
 * zoom are skipped; when the only questions are in such tiles, `reveal` brings the
 * oldest one's tile on screen and focus follows once its banner renders. Resolves
 * true only once focus has actually landed in a banner.
 */
export async function focusTileQuestion(
  preferredWorkspaceId: string | null,
  reveal?: (workspaceId: string) => void,
): Promise<boolean> {
  if (typeof document === "undefined") return false;
  const banners = [...document.querySelectorAll<HTMLElement>(`[${TILE_QUESTION_ATTR}]`)]
    .filter((el) => !isInHiddenTeam(el))
    .sort((a, b) => askNumber(a) - askNumber(b));
  const ordered = [
    ...banners.filter((el) => preferredWorkspaceId && paneOf(el) === preferredWorkspaceId),
    ...banners.filter((el) => !preferredWorkspaceId || paneOf(el) !== preferredWorkspaceId),
  ];
  for (const banner of ordered) {
    if (isRendered(banner) && focusBanner(banner)) return true;
  }
  const hidden = ordered.find((el) => !isRendered(el));
  const workspaceId = hidden ? paneOf(hidden) : null;
  if (!hidden || !workspaceId || !reveal) return false;
  reveal(workspaceId);
  for (let i = 0; i < REVEAL_FRAMES; i++) {
    await nextFrame();
    const again = [...document.querySelectorAll<HTMLElement>(`[${TILE_QUESTION_ATTR}]`)].find(
      (el) => paneOf(el) === workspaceId && !isInHiddenTeam(el) && isRendered(el),
    );
    if (again && focusBanner(again)) return true;
  }
  return false;
}

/** True when keyboard focus is inside `workspaceId`'s question banner. */
export function isFocusInTileQuestion(workspaceId: string): boolean {
  if (typeof document === "undefined") return false;
  const banner = document.activeElement?.closest<HTMLElement>(`[${TILE_QUESTION_ATTR}]`);
  return banner != null && paneOf(banner) === workspaceId;
}

/**
 * Focus the question banner of exactly `workspaceId`'s tile, waiting a few frames
 * for it to render (the tile may have just been brought on screen). Resolves true
 * only once focus has landed there.
 */
export async function focusQuestionIn(workspaceId: string): Promise<boolean> {
  if (typeof document === "undefined") return false;
  for (let i = 0; i <= REVEAL_FRAMES; i++) {
    const banner = [...document.querySelectorAll<HTMLElement>(`[${TILE_QUESTION_ATTR}]`)].find(
      (el) => paneOf(el) === workspaceId && !isInHiddenTeam(el) && isRendered(el),
    );
    if (banner && focusBanner(banner)) return true;
    await nextFrame();
  }
  return false;
}

/**
 * Park keyboard focus on a tile's question banner itself (its container, not a
 * choice or the answer field), so the question is announced and one Tab reaches
 * the answers, but a stray Enter answers nothing. False when the tile shows no
 * banner on screen.
 */
export function focusTileQuestionBanner(workspaceId: string, root?: ParentNode): boolean {
  if (typeof document === "undefined") return false;
  for (const banner of (root ?? document).querySelectorAll<HTMLElement>(
    `[${TILE_QUESTION_ATTR}]`,
  )) {
    if (paneOf(banner) !== workspaceId || isInHiddenTeam(banner) || !isRendered(banner)) continue;
    if (!banner.hasAttribute("tabindex")) banner.setAttribute("tabindex", "-1");
    banner.focus({ preventScroll: true });
    if (document.activeElement === banner) return true;
  }
  return false;
}

/**
 * Park keyboard focus on a tile's frame (not its terminal input), so keystrokes
 * the user is still typing go nowhere rather than into the agent's prompt.
 */
export function focusTileFrame(workspaceId: string, root?: ParentNode): boolean {
  if (typeof document === "undefined") return false;
  const frames =
    root instanceof HTMLElement && root.hasAttribute("data-pane-id")
      ? [root]
      : (root ?? document).querySelectorAll<HTMLElement>("[data-pane-id]");
  for (const frame of frames) {
    if (frame.getAttribute("data-pane-id") !== workspaceId || isInHiddenTeam(frame)) continue;
    if (!frame.hasAttribute("tabindex")) {
      frame.setAttribute("tabindex", "-1");
      frame.style.outline = "none";
    }
    frame.focus({ preventScroll: true });
    return document.activeElement === frame;
  }
  return false;
}

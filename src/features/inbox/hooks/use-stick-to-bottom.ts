// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

/** Within this many pixels of the end counts as "at the bottom". */
export const NEAR_BOTTOM_PX = 48;

/** True when `el` is scrolled to within {@link NEAR_BOTTOM_PX} of its end. */
export const nearBottom = (el: HTMLElement): boolean =>
  el.scrollHeight - el.scrollTop - el.clientHeight <= NEAR_BOTTOM_PX;

/**
 * Whether the reader has scrolled `ref` away from its end. Updated on scroll, and only on a
 * change, so scrolling re-renders nothing until the reader crosses the line.
 */
export function useAwayFromBottom(ref: RefObject<HTMLElement | null>, resetKey: unknown): boolean {
  const [state, setState] = useState({ key: resetKey, away: false });
  // Another conversation (or filter) starts at its bottom: reset while rendering, not after.
  if (state.key !== resetKey) setState({ key: resetKey, away: false });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = () => {
      const away = !nearBottom(el);
      setState((s) => (s.away === away ? s : { ...s, away }));
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [ref, resetKey]);
  return state.key === resetKey && state.away;
}

/** The first child of `el` still showing at its top, and how far that child sits from it. */
function topAnchor(el: HTMLElement): { node: Element; offset: number } | null {
  const kids = el.children;
  if (kids.length === 0) return null;
  const top = el.getBoundingClientRect().top;
  // Children run top to bottom: find the first whose bottom is below the viewport's top.
  let lo = 0;
  let hi = kids.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (kids[mid]!.getBoundingClientRect().bottom > top) hi = mid;
    else lo = mid + 1;
  }
  const node = kids[lo]!;
  return { node, offset: node.getBoundingClientRect().top - top };
}

/**
 * Keep a message list pinned to its newest entry the way a chat does: it opens at the bottom
 * (again whenever `resetKey` changes, e.g. another conversation opens) and follows new
 * messages while the reader is at the bottom, but never yanks a reader who scrolled up to
 * read history. Returns whether the reader is `away` from the bottom and a `jump` back to it.
 *
 * A reader who scrolled up keeps the message at the top of their view where it was when the
 * list changes above them (the oldest trimmed at the 500 cap, a message drawn late). Done by
 * hand rather than left to CSS scroll anchoring, which WebKit (the desktop app) lacks.
 */
export function useStickToBottom(
  ref: RefObject<HTMLElement | null>,
  resetKey: unknown,
  /** Changes when content is added or trimmed (a count and the newest key, say). */
  contentKey: unknown,
): { away: boolean; jump: () => void } {
  const pinned = useRef(true);
  const anchor = useRef<{ node: Element; offset: number } | null>(null);
  const away = useAwayFromBottom(ref, resetKey);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = () => {
      pinned.current = nearBottom(el);
      anchor.current = pinned.current ? null : topAnchor(el);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [ref, resetKey]);

  useLayoutEffect(() => {
    pinned.current = true;
    anchor.current = null;
  }, [resetKey]);

  // Pinned: to the bottom. Scrolled up: put the anchor message back where the reader saw it.
  const settle = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    if (pinned.current) {
      el.scrollTop = el.scrollHeight;
      return;
    }
    const a = anchor.current;
    if (!a || !a.node.isConnected || a.node.parentElement !== el) return;
    const delta = a.node.getBoundingClientRect().top - el.getBoundingClientRect().top - a.offset;
    if (delta !== 0) el.scrollTop += delta;
  }, [ref]);

  useLayoutEffect(() => {
    settle();
  }, [settle, resetKey, contentKey]);

  // Content that grows after layout (an HTML message drawn into its sandbox in an effect, an
  // image loading) keeps a pinned list at the bottom, and a scrolled-up reader in place,
  // and so does the list's own box resizing (a popup dragged smaller).
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(settle);
    ro.observe(el);
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => ro.disconnect();
  }, [ref, settle, resetKey, contentKey]);

  const jump = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    pinned.current = true;
    anchor.current = null;
    el.scrollTop = el.scrollHeight;
  }, [ref]);

  return { away, jump };
}

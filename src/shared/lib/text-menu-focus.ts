// SPDX-License-Identifier: MIT
// Architecture: an editor and its portaled text menu form one focus boundary.
const sessions = new Map<Element, { onLeave?: () => void }>();

export function holdTextMenuFocus(element: Element) {
  const session: { onLeave?: () => void } = {};
  sessions.set(element, session);
  return (restore: boolean) => {
    requestAnimationFrame(() => {
      if (sessions.get(element) !== session) return;
      if (restore && element.isConnected && element instanceof HTMLElement)
        element.focus({ preventScroll: true });
      sessions.delete(element);
      if (element.isConnected && !element.contains(document.activeElement)) session.onLeave?.();
    });
  };
}

export function textMenuWithin(element: HTMLElement | null) {
  return !!element && [...sessions.keys()].some((target) => element.contains(target));
}

export function commitTextEditOnBlur(element: HTMLElement, commit: () => void) {
  const session = sessions.get(element);
  if (session) session.onLeave = commit;
  else commit();
}

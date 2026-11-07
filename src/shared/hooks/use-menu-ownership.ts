// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef, type RefObject } from "react";

const owners = new Set<(owner: object) => void>();
const surfaces = new WeakMap<Element, object>();
// Architecture: a field's text menu shares its containing menu/dialog's owner.
export function menuOwnerFor(element: Element | null): object | undefined {
  for (let node = element; node; node = node.parentElement) {
    const owner = surfaces.get(node);
    if (owner) return owner;
  }
}
export function useMenuOwnership(
  owner: object,
  open: boolean,
  close: () => void,
  surface?: RefObject<HTMLElement | null>,
) {
  const closeRef = useRef(close);
  useLayoutEffect(() => {
    closeRef.current = close;
  });
  useLayoutEffect(() => {
    if (!open) return;
    const element = surface?.current;
    if (element) surfaces.set(element, owner);
    for (const listener of owners) listener(owner);
    const listener = (next: object) => {
      if (next !== owner) closeRef.current();
    };
    owners.add(listener);
    return () => {
      owners.delete(listener);
      if (element) surfaces.delete(element);
    };
  }, [owner, open, surface]);
}

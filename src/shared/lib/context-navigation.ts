// SPDX-License-Identifier: MIT
// Deliberate keyboard/menu navigation, pointer or keyboard focus entry, document close,
// grid arrangement and right-pane changes revoke pending Show/restore work.
// Automatic focus and layout reconciliation preserve the same revision.
let navigationRevision = 0;
export function currentContextNavigationRevision(): number {
  return navigationRevision;
}
export function beginContextNavigation(): number {
  return ++navigationRevision;
}

const MODIFIER_KEYS = new Set([
  "Alt",
  "AltGraph",
  "CapsLock",
  "Control",
  "Fn",
  "FnLock",
  "Meta",
  "NumLock",
  "ScrollLock",
  "Shift",
  "Symbol",
  "SymbolLock",
  "Hyper",
  "Super",
]);

/** Observe physical navigation entry, including portaled editors and overlays.
 * Focus/blur alone stay neutral because restores and popup return focus use them. */
export function installUserNavigationCapture(target: Document): () => void {
  const pointer = () => {
    beginContextNavigation();
  };
  const keyboard = (event: KeyboardEvent) => {
    if (!MODIFIER_KEYS.has(event.key)) beginContextNavigation();
  };
  target.addEventListener("pointerdown", pointer, true);
  target.addEventListener("keydown", keyboard, true);
  return () => {
    target.removeEventListener("pointerdown", pointer, true);
    target.removeEventListener("keydown", keyboard, true);
  };
}

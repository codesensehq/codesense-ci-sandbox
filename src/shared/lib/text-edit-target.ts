// SPDX-License-Identifier: MIT
import type { TextEditAction, TextEditState } from "@/components/ui/text-edit-menu";

export function captureTextEditTarget(event: Pick<MouseEvent, "target" | "clientX" | "clientY">) {
  const target = event.target instanceof Element ? event.target : null;
  if (target?.closest(".xterm")) return null;
  const field =
    target?.closest<HTMLElement>(
      'input,textarea,[contenteditable]:not([contenteditable="false"])',
    ) ?? null;
  const input =
    field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement ? field : null;
  if (
    input instanceof HTMLInputElement &&
    !/^(text|search|url|tel|email|number|password)$/i.test(input.type)
  )
    return null;
  const selection = window.getSelection();
  let range: Range | null = null;
  if (selection?.rangeCount && !selection.isCollapsed) {
    const selected = selection.getRangeAt(0);
    if (
      (field?.contains(selection.anchorNode) ?? false) ||
      (event.clientX === 0 &&
        event.clientY === 0 &&
        (!target || target === document.body || selected.intersectsNode(target))) ||
      Array.from(selected.getClientRects()).some(
        (r) =>
          event.clientX >= r.left &&
          event.clientX <= r.right &&
          event.clientY >= r.top &&
          event.clientY <= r.bottom,
      )
    )
      range = selected.cloneRange();
  }
  if (input) range = null;
  if (!field && !range) return null;
  const start = input?.selectionStart,
    end = input?.selectionEnd,
    direction = input?.selectionDirection;
  const state: TextEditState = {
    editable:
      !!field &&
      !input?.disabled &&
      !input?.readOnly &&
      field.getAttribute("aria-readonly") !== "true",
    hasSelection:
      !!range ||
      (typeof start === "number" && start !== end) ||
      (!!input &&
        typeof start !== "number" &&
        document.activeElement === input &&
        !!selection?.toString()),
    hasText: !!(input ? input.value : (field ?? document.body).textContent),
    password: input?.type === "password",
  };
  const focus = () => {
    if (field?.isConnected) field.focus({ preventScroll: true });
  };
  return {
    element: field ?? target!,
    field: !!field,
    active: document.activeElement === field,
    state,
    focus,
    async apply(action: TextEditAction) {
      const connected = () =>
        (field ?? target)?.isConnected && (!range || range.commonAncestorContainer.isConnected);
      if (!connected()) return;
      // Read the clipboard only in the menu item's activation path.
      const text = action === "paste" ? await navigator.clipboard.readText() : undefined;
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      // Architecture: async clipboard reads never edit a removed/replaced field.
      if (!connected()) return;
      focus();
      if (input && typeof start === "number")
        input.setSelectionRange(start, end ?? start, direction ?? undefined);
      if (range) {
        const selected = window.getSelection();
        selected?.removeAllRanges();
        selected?.addRange(range);
      }
      document.execCommand(action === "paste" ? "insertText" : action, false, text);
    },
  };
}

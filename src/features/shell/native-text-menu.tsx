// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
} from "@/components/ui/dropdown-menu";
import { TextEditMenu } from "@/components/ui/text-edit-menu";
import { captureTextEditTarget } from "@/shared/lib/text-edit-target";
import { menuOwnerFor, useMenuOwnership } from "@/shared/hooks/use-menu-ownership";
import { holdTextMenuFocus } from "@/shared/lib/text-menu-focus";
import { toast } from "sonner";

type Captured = NonNullable<ReturnType<typeof captureTextEditTarget>>;
export function NativeTextMenu() {
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    target: Captured;
    find?: HTMLElement;
    owner?: object;
    finish: (restore: boolean) => void;
  } | null>(null);
  const [owner] = useState(() => ({}));
  const pending = useRef<Promise<unknown> | null>(null);
  const current = useRef(menu);
  const restore = useRef(true);
  const close = (returnFocus = restore.current) => {
    const closing = menu;
    current.current = null;
    setMenu(null);
    if (closing)
      void (pending.current ?? Promise.resolve()).then(() => closing.finish(returnFocus));
    pending.current = null;
  };
  useMenuOwnership(menu?.owner ?? owner, !!menu, () => close(false));
  useEffect(() => {
    let pointerTarget: ReturnType<typeof captureTextEditTarget> | undefined;
    const onPointer = (event: PointerEvent) => {
      if (event.button === 2) pointerTarget = captureTextEditTarget(event);
    };
    const onContext = (
      event: Pick<
        MouseEvent,
        | "target"
        | "clientX"
        | "clientY"
        | "defaultPrevented"
        | "preventDefault"
        | "stopImmediatePropagation"
      >,
    ) => {
      if (event.defaultPrevented) return;
      const captured = captureTextEditTarget(event);
      const target =
        pointerTarget?.field && pointerTarget.active && pointerTarget.state.hasSelection
          ? pointerTarget
          : !captured?.field &&
              (event.clientX !== 0 || event.clientY !== 0) &&
              pointerTarget !== undefined
            ? pointerTarget
            : captured;
      pointerTarget = undefined;
      if (!target) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      const element = event.target instanceof HTMLElement ? event.target : null;
      const rect = element?.getBoundingClientRect();
      const keyboard = event.clientX === 0 && event.clientY === 0;
      restore.current = true;
      current.current?.finish(false);
      const next = {
        x: keyboard ? (rect?.left ?? 0) : event.clientX,
        y: keyboard ? (rect?.bottom ?? 0) : event.clientY,
        target,
        // Dispatch inside the content, where the viewer scopes its search listener.
        find: element?.closest("[data-viewer-text-menu]") ? element : undefined,
        owner: menuOwnerFor(target.element),
        finish: holdTextMenuFocus(target.element),
      };
      current.current = next;
      setMenu(next);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "ContextMenu" && !(event.key === "F10" && event.shiftKey)) return;
      onContext({
        target: event.target,
        clientX: 0,
        clientY: 0,
        defaultPrevented: event.defaultPrevented,
        preventDefault: () => event.preventDefault(),
        stopImmediatePropagation: () => event.stopImmediatePropagation(),
      });
    };
    document.addEventListener("pointerdown", onPointer, true);
    document.addEventListener("contextmenu", onContext, true);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onPointer, true);
      document.removeEventListener("contextmenu", onContext, true);
      document.removeEventListener("keydown", onKey, true);
      current.current?.finish(false);
    };
  }, []);
  if (!menu) return null;
  return (
    <DropdownMenu
      open
      modal={false}
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <DropdownMenuTrigger
        style={{
          position: "fixed",
          left: menu.x,
          top: menu.y,
          width: 0,
          height: 0,
          pointerEvents: "none",
        }}
        tabIndex={-1}
        aria-hidden
      />
      <DropdownMenuContent
        className="min-w-44"
        onCloseAutoFocus={(event) => event.preventDefault()}
        onInteractOutside={() => {
          restore.current = false;
        }}
      >
        <TextEditMenu
          state={menu.target.state}
          onChoose={(action) => {
            pending.current = menu.target
              .apply(action)
              .catch(() => toast.error("Couldn't edit text"));
          }}
        />
        {menu.find && (
          <NativeFindItem
            onFind={() => {
              restore.current = false;
              menu.find?.dispatchEvent(new CustomEvent("codesense:find", { bubbles: true }));
            }}
          />
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
function NativeFindItem({ onFind }: { onFind: () => void }) {
  return (
    <>
      <DropdownMenuSeparator />
      <DropdownMenuItem onSelect={onFind}>
        Find in document
        <DropdownMenuShortcut>{formatKeystrokeDisplay("CmdOrCtrl+F")}</DropdownMenuShortcut>
      </DropdownMenuItem>
    </>
  );
}

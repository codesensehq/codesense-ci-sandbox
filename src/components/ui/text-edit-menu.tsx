// SPDX-License-Identifier: MIT
import { DropdownMenuItem, DropdownMenuSeparator, DropdownMenuShortcut } from "./dropdown-menu";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";

export type TextEditState = {
  editable: boolean;
  hasSelection: boolean;
  hasText: boolean;
  password?: boolean;
};
export type TextEditAction = "cut" | "copy" | "paste" | "selectAll";
export function TextEditMenu({
  state,
  onChoose,
}: {
  state: TextEditState;
  onChoose: (action: TextEditAction) => void;
}) {
  return (
    <>
      {(
        [
          ["cut", "Cut", "X", !state.editable || !state.hasSelection || state.password],
          ["copy", "Copy", "C", !state.hasSelection || state.password],
          ["paste", "Paste", "V", !state.editable],
        ] as const
      ).map(([action, label, key, disabled]) => (
        <DropdownMenuItem key={action} disabled={!!disabled} onSelect={() => onChoose(action)}>
          {label}
          <DropdownMenuShortcut>{formatKeystrokeDisplay(`CmdOrCtrl+${key}`)}</DropdownMenuShortcut>
        </DropdownMenuItem>
      ))}
      <DropdownMenuSeparator />
      <DropdownMenuItem disabled={!state.hasText} onSelect={() => onChoose("selectAll")}>
        Select All
        <DropdownMenuShortcut>{formatKeystrokeDisplay("CmdOrCtrl+A")}</DropdownMenuShortcut>
      </DropdownMenuItem>
    </>
  );
}

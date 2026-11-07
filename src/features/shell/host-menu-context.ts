// SPDX-License-Identifier: MIT
import type { TextEditAction, TextEditState } from "@/components/ui/text-edit-menu";
import type { InboxTeam } from "@/tauri/commands";
import { createContext } from "react";

export type FrameAnchorRect = { x: number; y: number; width: number; height: number };
export type HostMenuGlyph =
  | string
  | { kind: "stageRail"; role?: import("@/tauri/commands").StatusRole; ready?: boolean }
  | {
      kind: "statusMark";
      mark: import("@/shared/lib/status-mark").StatusMarkKind;
      size?: 6 | 8;
      still?: boolean;
    }
  | { kind: "fileKindIcon"; fileKind: import("@/shared/lib/icons/file-kind").FileKind };
export type HostMenuItem = {
  id: string;
  label?: string;
  icon?: HostMenuGlyph;
  count?: number;
  radioGroup?: string;
  segmented?: HostMenuItem[];
  separator?: boolean;
  submenu?: Array<HostMenuItem | string>;
  checked?: boolean;
  enabled?: boolean;
};
export type HostMenuResult = { closed: true } | { itemId: string; row: unknown; builtin?: true };
export type HostMenuRun = (
  verb: string,
  args: unknown,
  options?: { confirmInHost?: boolean },
) => Promise<unknown>;
export type HostMenuRequest = {
  builtinAction?: "builtin:rename" | "builtin:delete" | "builtin:new-file";
  type: "menu" | "default" | "popover" | "native" | "text" | "frame" | "inbox";
  teamKey?: string;
  team?: InboxTeam;
  page?: {
    enabled: () => boolean;
    notify: (
      itemId: string,
      row: Record<string, unknown>,
      waitForPage?: boolean,
    ) => void | Promise<void>;
    run: HostMenuRun;
  };
  contentRoot?: string;
  signal?: AbortSignal;
  text?: TextEditState;
  editText?: (action: TextEditAction) => Promise<void>;
  dashboard?: {
    reload: () => void | Promise<unknown>;
    openAsTab?: () => void;
    editSource?: () => void;
    centre?: boolean;
  };
  frame: HTMLIFrameElement;
  origin: string;
  anchorRect: FrameAnchorRect;
  anchorId?: string;
  row?: Record<string, unknown>;
  items?: Array<HostMenuItem | string>;
  kind?: string;
  nativeSurface?: string;
  sessionId?: string;
  returnFocus?: () => void;
  run: HostMenuRun;
};
export const HostMenuContext = createContext<(request: HostMenuRequest) => Promise<HostMenuResult>>(
  async () => {
    throw new Error("unavailable: host menu layer");
  },
);
export function hostAnchorRect(request: HostMenuRequest): DOMRect {
  const box = request.frame.getBoundingClientRect();
  const { x, y, width, height } = request.anchorRect;
  return new DOMRect(box.left + x, box.top + y, width, height);
}

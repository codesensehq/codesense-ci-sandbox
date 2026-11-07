// SPDX-License-Identifier: MIT
import { useRef, useState } from "react";
import type { InboxLens } from "@/features/viewer/stores/agent-inbox-rail-store";

/** Chip, pinned row and keyboard share one owner for the return target. */
export function useGroupChatSelection({
  selected,
  lens,
  onSelect,
  onLensChange,
}: {
  selected: string | null;
  lens: InboxLens;
  onSelect: (key: string | null) => void;
  onLensChange: (lens: InboxLens) => void;
}) {
  const returnTarget = useRef<{ selected: string | null; lens: InboxLens } | null>(null);
  const [focusRequest, setFocusRequest] = useState(0);
  const open = (key: string, keyboard: boolean) => {
    const inGroup = lens === "threads" && selected?.startsWith("group:");
    if (!inGroup) returnTarget.current = { selected: lens === "threads" ? selected : null, lens };
    onSelect(key);
    onLensChange("threads");
    setFocusRequest((n) => (keyboard ? n + 1 : 0));
  };
  const toggle = (key: string, keyboard: boolean) => {
    if (lens === "threads" && selected === key) {
      onSelect(returnTarget.current?.selected ?? null);
      onLensChange(returnTarget.current?.lens ?? "threads");
    } else open(key, keyboard);
  };
  return { open, toggle, focusRequest };
}

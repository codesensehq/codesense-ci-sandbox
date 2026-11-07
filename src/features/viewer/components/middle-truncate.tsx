// SPDX-License-Identifier: MIT
import { useLayoutEffect, useState, type HTMLAttributes } from "react";
import { cn } from "@/lib/utils";
import { TEXT_XS_CHAR_W, splitForMiddleTruncation } from "../lib/tile-chrome-fit";

/**
 * A label that truncates in the MIDDLE (`SW-FX…01-lag`), in CSS: the head
 * ellipsizes while the tail never shrinks, so labels that share a prefix stay
 * distinguishable. The box's measured width caps the tail, so in a tight slot
 * (a pill at its minimum width) the tail shortens instead of spilling. Short
 * labels render as one plain span. Extra span props land on the root.
 */
export function MiddleTruncate({
  text,
  className,
  ...rest
}: { text: string } & Omit<HTMLAttributes<HTMLSpanElement>, "children">) {
  const [el, setEl] = useState<HTMLSpanElement | null>(null);
  const [availableChars, setAvailableChars] = useState<number | undefined>(undefined);
  useLayoutEffect(() => {
    if (!el || typeof ResizeObserver === "undefined") return;
    const update = (width: number) => {
      if (width > 0) setAvailableChars(Math.floor(width / TEXT_XS_CHAR_W));
    };
    update(el.getBoundingClientRect().width);
    const ro = new ResizeObserver((entries) => update(entries[0]?.contentRect.width ?? 0));
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);

  const { head, tail } = splitForMiddleTruncation(text, availableChars);
  if (!tail) {
    return (
      <span {...rest} ref={setEl} className={cn("truncate whitespace-pre", className)}>
        {text}
      </span>
    );
  }
  return (
    <span {...rest} ref={setEl} className={cn("flex min-w-0 whitespace-pre", className)}>
      {/* `whitespace-pre`, not `truncate`'s `nowrap`: nowrap collapses the head's
          trailing space, gluing "Notifications " + "triage" into one word. */}
      <span className="overflow-hidden text-ellipsis whitespace-pre">{head}</span>
      <span className="flex-shrink-0">{tail}</span>
    </span>
  );
}

/** True when `el` (a MiddleTruncate root) is currently cutting its text. */
export function isMiddleTruncated(el: HTMLElement): boolean {
  if (el.scrollWidth > el.clientWidth) return true;
  return Array.from(el.children).some((c) => c.scrollWidth > c.clientWidth);
}

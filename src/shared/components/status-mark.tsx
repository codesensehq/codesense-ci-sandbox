// SPDX-License-Identifier: MIT
import { useId, useMemo, type HTMLAttributes } from "react";

import { InfoTip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { statusMarkDrawing } from "@/shared/lib/status-mark-svg";
import type { StatusMarkKind } from "@/shared/lib/status-mark";

/** Named sizes: `sm` for tabs and tray badges, `md` for tile chrome and pill headlines. */
const NAMED_SIZE = { sm: 6, md: 8 } as const;

export interface StatusMarkProps extends Omit<HTMLAttributes<HTMLSpanElement>, "children"> {
  mark: StatusMarkKind;
  /** The mark's box in CSS px, or a named size. Default 7 (agent list rows). */
  size?: number | keyof typeof NAMED_SIZE;
  /** The state in words. Given, the mark is an image with that accessible name and
   *  shows it as a tooltip; without it, the mark is decoration beside its own words. */
  label?: string;
  "data-testid"?: string;
}

/**
 * The one status mark: an SVG drawn per state in the shared vocabulary
 * (`shared/lib/status-mark.ts`), coloured by the theme-aware `--cs-status-*`
 * tokens (`.cs-mark-*` in `styles/status-mark.css`). Drawn in its own pixel
 * space, so strokes stay 1.5px and crisp at every size and density.
 */
export function StatusMark({
  mark,
  size = 7,
  label,
  className,
  ...rest
}: StatusMarkProps): React.JSX.Element {
  const px = typeof size === "number" ? size : NAMED_SIZE[size];
  const a11y = label ? { role: "img", "aria-label": label } : { "aria-hidden": true as const };
  const el = (
    <span
      {...rest}
      {...a11y}
      data-mark={mark}
      className={cn("cs-mark", `cs-mark-${mark}`, className)}
      style={{ width: px, height: px, ...rest.style }}
    >
      <MarkSvg mark={mark} s={px} />
    </span>
  );
  return label ? (
    <InfoTip label={label} side="top">
      {el}
    </InfoTip>
  ) : (
    el
  );
}

function MarkSvg({ mark, s }: { mark: StatusMarkKind; s: number }): React.JSX.Element {
  const maskId = useId();
  const drawing = statusMarkDrawing(mark, s, maskId);
  // React 19 re-assigns innerHTML whenever the `dangerouslySetInnerHTML` object is a new
  // reference, even with an identical string. That tears down the glyph's nodes on every
  // re-render (a press on the tray chip is one), detaching the pointerdown target so the
  // browser drops the click. Keep the object stable while the markup is unchanged.
  const inner = useMemo(() => ({ __html: drawing.html }), [drawing.html]);
  return (
    <svg
      width={s}
      height={s}
      viewBox={`0 0 ${s} ${s}`}
      overflow="visible"
      className={drawing.className}
      dangerouslySetInnerHTML={inner}
    />
  );
}

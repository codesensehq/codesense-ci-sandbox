// SPDX-License-Identifier: MIT
import type { StatusRole } from "@/tauri/commands";
import { cn } from "@/lib/utils";
import { useStatusState } from "@/shared/stores/status-taxonomy-store";

/** How far along the track each role fills, in thirds. */
const THIRDS: Readonly<Record<StatusRole, number>> = {
  not_started: 0,
  active: 1,
  review: 2,
  done: 3,
  blocked: 1,
};

export interface StageRailProps {
  /** A status id in the live taxonomy; an id outside it draws the dotted off-taxonomy track. */
  status: string;
  /** The rail's box height in CSS px (default 14, the floor for list rows). */
  size?: number;
  /** Name the stage for assistive tech and as a native tooltip. Off when the stage is
   *  named beside the rail already (a menu option, a group header). */
  labelled?: boolean;
  /** Not started with every dependency done: the empty track points onward. */
  ready?: boolean;
  /** Overrides the stage's name as the accessible label and tooltip. */
  label?: string;
  /** Show the label as the native tooltip (off when a richer tooltip wraps the rail). */
  titled?: boolean;
  className?: string;
}

/**
 * Where a story is: a track filled to its stage in thirds, keyed by the stage's taxonomy
 * ROLE (never its id or colour), so a team's own states draw the same way. Blocked puts a
 * wall after the first third; Done fills the track solid; a status outside the taxonomy is
 * a dotted outline; a not-started story whose dependencies are all done ends its empty track
 * in an open arrowhead. Neutral ink only: colour in the nav belongs to agent marks.
 */
export function StageRail({
  status,
  size = 14,
  labelled = true,
  ready,
  label,
  titled = true,
  className,
}: StageRailProps) {
  const state = useStatusState(status);
  return (
    <StageRailShape
      role={state?.role}
      size={size}
      ready={ready}
      label={labelled ? (label ?? state?.label ?? status) : undefined}
      titled={titled}
      className={className}
    />
  );
}

/** The rail for a role, without the taxonomy read (group headers and lens options). */
export function StageRailShape({
  role,
  size = 14,
  ready = false,
  label,
  titled = true,
  className,
}: {
  role: StatusRole | undefined;
  size?: number;
  /** Not started and clear to start: the track ends in an open arrowhead. Other roles ignore it. */
  ready?: boolean;
  label?: string;
  titled?: boolean;
  className?: string;
}) {
  const h = size;
  const w = Math.round(h * 1.3);
  const th = Math.max(3, Math.round(h * 0.29));
  const y = (h - th) / 2;
  const r = th / 2;
  const a11y = label
    ? { role: "img", "aria-label": label, title: titled ? label : undefined }
    : { "aria-hidden": true };
  let body: React.ReactNode;
  if (role === undefined) {
    body = (
      <rect
        x={0.5}
        y={y + 0.5}
        width={w - 1}
        height={th - 1}
        rx={r - 0.5}
        fill="none"
        stroke="var(--cs-rail-fill)"
        strokeWidth={1}
        strokeDasharray="1.5 1.5"
      />
    );
  } else if (role === "not_started" && ready) {
    // The empty track, shortened, ends in an open head: nothing done yet, nothing in the way.
    // Drawn at the far end so it never reads as the disclosure chevron at a row's start.
    const head = Math.max(3, h * 0.27);
    const tip = w - 0.6;
    const back = tip - head;
    body = (
      <>
        <rect x={0} y={y} width={back - 0.6} height={th} rx={r} fill="var(--cs-rail-empty)" />
        <path
          d={`M ${back} ${h / 2 - head * 0.9} L ${tip} ${h / 2} L ${back} ${h / 2 + head * 0.9}`}
          fill="none"
          stroke="var(--cs-rail-fill)"
          strokeWidth={1.5}
          strokeLinecap="round"
          strokeLinejoin="round"
          data-rail="ready"
        />
      </>
    );
  } else if (role === "blocked") {
    const x0 = w / 3 + 1.1;
    const bw = 1.75;
    body = (
      <>
        <rect x={0} y={y} width={w / 3 - 0.6} height={th} rx={1} fill="var(--cs-rail-fill)" />
        <rect
          x={x0 + bw + 1}
          y={y}
          width={w - x0 - bw - 1}
          height={th}
          rx={r}
          fill="var(--cs-rail-empty)"
        />
        <rect
          x={x0}
          y={y - 2.5}
          width={bw}
          height={th + 5}
          rx={bw / 2}
          fill="var(--cs-rail-wall)"
          data-rail="wall"
        />
      </>
    );
  } else {
    const third = THIRDS[role];
    const done = role === "done";
    const fillW = done ? w : (w * third) / 3 - (third ? 0.6 : 0);
    body = (
      <>
        <rect x={0} y={y} width={w} height={th} rx={r} fill="var(--cs-rail-empty)" />
        {third > 0 && (
          <rect
            x={0}
            y={y}
            width={fillW}
            height={th}
            rx={done ? r : 1}
            fill={done ? "var(--cs-rail-done)" : "var(--cs-rail-fill)"}
            data-rail="fill"
          />
        )}
      </>
    );
  }
  return (
    <span
      {...a11y}
      data-rail-role={role ?? "off"}
      data-rail-ready={(role === "not_started" && ready) || undefined}
      className={cn("cs-rail", className)}
      style={{ width: w, height: h }}
    >
      <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`}>
        {body}
      </svg>
    </span>
  );
}

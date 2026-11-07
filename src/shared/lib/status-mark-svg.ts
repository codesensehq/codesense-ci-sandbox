// SPDX-License-Identifier: MIT
import type { StatusMarkKind } from "./status-mark";

/** Framework-free pixel geometry shared by native and framed status marks. */
export function statusMarkDrawing(
  mark: StatusMarkKind,
  s: number,
  maskId: string,
): { html: string; className?: string } {
  const c = s / 2;
  const w = s <= 8 ? 1.5 : Math.round(s * 0.17 * 4) / 4;
  const r = c - w / 2;
  const ring = `fill="none" stroke="currentColor" stroke-width="${w}"`;
  const circle = `<circle cx="${c}" cy="${c}" r="${r}" ${ring}/>`;
  const halo = (still = false) =>
    `<circle class="cs-mark-halo${still ? "-still" : ""}" cx="${c}" cy="${c}" r="${c + 1.25}" fill="none" stroke="currentColor" stroke-width="1.5"${still ? ' opacity="0.35"' : ""}/>`;
  const cut = (shape: string, body = `<circle cx="${c}" cy="${c}" r="${c}" fill="currentColor"`) =>
    `<mask id="${maskId}"><rect width="${s}" height="${s}" fill="white"/>${shape}</mask>${body} mask="url(#${maskId})"/>`;
  switch (mark) {
    case "ready":
      return { html: circle };
    case "tool":
      return {
        className: "cs-mark-spin",
        html: `<path d="M ${c} ${c - r} A ${r} ${r} 0 1 1 ${c - r} ${c}" ${ring} stroke-linecap="round"/>`,
      };
    case "thinking": {
      const dr = Math.max(0.65, s * 0.11),
        gap = Math.max(1.65, s * 0.3);
      return {
        html: [c - gap, c, c + gap]
          .map(
            (x, i) =>
              `<circle class="cs-mark-think" data-i="${i}" cx="${x}" cy="${c}" r="${dr}" fill="currentColor"/>`,
          )
          .join(""),
      };
    }
    case "question":
      return {
        html:
          halo() +
          cut(
            `<text x="${c}" y="${c + s * 0.14}" font-size="${s * 0.75}" font-family="ui-sans-serif, system-ui" font-weight="700" text-anchor="middle" fill="black">?</text>`,
          ),
      };
    case "permission": {
      const bw = Math.max(1, Math.round(s * 0.5)),
        bh = Math.max(1, Math.round(s * 0.18));
      return {
        html:
          halo() +
          cut(
            `<rect x="${c - bw / 2}" y="${c - bh / 2}" width="${bw}" height="${bh}" rx="${bh * 0.3}" fill="black"/>`,
          ),
      };
    }
    case "blocked": {
      const bw = Math.max(0.9, Math.round(s * 0.14 * 2) / 2),
        bh = s * 0.5,
        gap = Math.max(1, s * 0.1);
      return {
        html:
          halo(true) +
          cut(
            `<rect x="${c - gap / 2 - bw}" y="${c - bh / 2}" width="${bw}" height="${bh}" fill="black"/><rect x="${c + gap / 2}" y="${c - bh / 2}" width="${bw}" height="${bh}" fill="black"/>`,
          ),
      };
    }
    case "error": {
      const bw = Math.max(1, Math.round(s * 0.15 * 2) / 2);
      return {
        html: cut(
          `<rect x="${c - bw / 2}" y="${s * 0.36}" width="${bw}" height="${s * 0.28}" fill="black"/><rect x="${c - bw / 2}" y="${s * 0.7}" width="${bw}" height="${bw}" fill="black"/>`,
          `<path d="M ${c} ${s * 0.04} L ${s} ${s * 0.92} L 0 ${s * 0.92} Z" fill="currentColor" stroke="currentColor" stroke-width="0.6" stroke-linejoin="round"`,
        ),
      };
    }
    case "ended": {
      const sw = s <= 8 ? 1.25 : w * 0.85,
        inset = s * 0.1 + sw / 2;
      return {
        html: `<rect x="${inset}" y="${inset}" width="${s - 2 * inset}" height="${s - 2 * inset}" rx="${Math.max(0.5, s * 0.08)}" fill="none" stroke="currentColor" stroke-width="${sw}"/>`,
      };
    }
    case "crashed": {
      const inset = s * 0.1;
      return {
        html: `<rect x="${inset}" y="${inset}" width="${s - 2 * inset}" height="${s - 2 * inset}" rx="${Math.max(0.75, s * 0.1)}" fill="currentColor"/>`,
      };
    }
    case "unknown": {
      const seg = (2 * Math.PI * r) / 6;
      return {
        html: `<circle cx="${c}" cy="${c}" r="${r}" ${ring} stroke-dasharray="${seg * 0.55} ${seg * 0.45}"/>`,
      };
    }
    case "stalled":
      return {
        html:
          circle +
          `<circle cx="${c}" cy="${c}" r="${Math.max(0.9, s * 0.15)}" fill="currentColor"/>`,
      };
    case "offline":
      return {
        html:
          circle +
          `<line x1="${s * 0.15}" y1="${s * 0.85}" x2="${s * 0.85}" y2="${s * 0.15}" stroke="currentColor" stroke-width="${w}" stroke-linecap="round"/>`,
      };
  }
}

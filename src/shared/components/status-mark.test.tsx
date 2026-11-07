// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import {
  countMarks,
  markOfAgentStatus,
  markOfNeedsYouKind,
  markOfTerminalStatus,
  rollupWords,
  STATUS_MARKS,
  topMark,
  type StatusMarkKind,
} from "@/shared/lib/status-mark";

import { StatusMark } from "./status-mark";
import { themeTokens } from "@/themes";
import { contrastRatio, mixHex, CONTRAST_THRESHOLDS } from "@/shared/lib/color-contrast";
import { statusPillStyle } from "./status-pill";

const CSS = readFileSync(resolve(__dirname, "../../styles/status-mark.css"), "utf-8");
const AGENTS_CSS = readFileSync(resolve(__dirname, "../../styles/agents.css"), "utf-8");

function draw(mark: StatusMarkKind, size?: number | "sm" | "md") {
  const { container, unmount } = render(<StatusMark mark={mark} size={size} />);
  const el = container.querySelector<HTMLElement>("[data-mark]")!;
  return { el, svg: el.querySelector("svg")!, unmount };
}

describe("StatusMark — one shape per state", () => {
  it("draws each state by its own shape, coloured by its own class", () => {
    const shapes: Record<StatusMarkKind, (svg: SVGSVGElement) => boolean> = {
      // A tool call turns.
      tool: (s) =>
        s.classList.contains("cs-mark-spin") &&
        s.querySelector("path")?.getAttribute("stroke-linecap") === "round",
      // Thinking breathes — three dots.
      thinking: (s) => s.querySelectorAll("circle.cs-mark-think").length === 3,
      // A question has a pulsing halo, a `?` cut-out and the dot's fill.
      question: (s) =>
        !!s.querySelector("circle.cs-mark-halo") &&
        !!s.querySelector("mask text") &&
        !!s.querySelector("circle[fill='currentColor'][mask]"),
      // A permission has a pulsing halo, a lock-bar cut-out and the dot's fill.
      permission: (s) =>
        !!s.querySelector("circle.cs-mark-halo") &&
        s.querySelectorAll("mask rect").length === 2 &&
        !!s.querySelector("circle[fill='currentColor'][mask]"),
      // Blocked has a STILL halo (no pulse), two pause-bar cut-outs and the dot's fill.
      blocked: (s) =>
        !!s.querySelector("circle.cs-mark-halo-still") &&
        !s.querySelector("circle.cs-mark-halo") &&
        s.querySelectorAll("mask rect").length === 3 &&
        !!s.querySelector("circle[fill='currentColor'][mask]"),
      // Error: filled triangle with a "!" cut-out.
      error: (s) => !!s.querySelector("mask") && !!s.querySelector("path[fill='currentColor']"),
      // Crashed: filled rounded square.
      crashed: (s) => s.querySelector("rect")?.getAttribute("fill") === "currentColor",
      // Ended: hollow rounded square.
      ended: (s) =>
        s.querySelector("rect")?.getAttribute("fill") === "none" &&
        s.querySelector("rect")?.getAttribute("stroke") === "currentColor",
      // Ready: hollow ring (no dash, no central dot, no strike).
      ready: (s) => {
        const circles = s.querySelectorAll("circle");
        if (circles.length !== 1) return false;
        const c = circles[0];
        return (
          c.getAttribute("fill") === "none" &&
          !c.getAttribute("stroke-dasharray") &&
          !s.querySelector("line")
        );
      },
      // Stalled: hollow ring with a central dot (two circles).
      stalled: (s) => {
        const circles = s.querySelectorAll("circle");
        return (
          circles.length === 2 &&
          circles[0].getAttribute("fill") === "none" &&
          circles[1].getAttribute("fill") === "currentColor"
        );
      },
      // Unknown: dashed ring.
      unknown: (s) => !!s.querySelector("circle")?.getAttribute("stroke-dasharray"),
      // Offline: hollow ring with a diagonal strike.
      offline: (s) => !!s.querySelector("circle") && !!s.querySelector("line"),
    };
    for (const mark of STATUS_MARKS) {
      const { el, svg, unmount } = draw(mark);
      expect(el).toHaveClass("cs-mark", `cs-mark-${mark}`);
      expect(shapes[mark](svg), mark).toBe(true);
      unmount();
    }
  });

  it("only a tool call turns and only question/permission pulses: no other mark carries motion", () => {
    for (const mark of STATUS_MARKS) {
      const { svg, unmount } = draw(mark);
      // The turning arc is only on tool.
      expect(!!svg.closest(".cs-mark-spin") || svg.classList.contains("cs-mark-spin")).toBe(
        mark === "tool",
      );
      // The pulsing halo is only on the live asks — question + permission. Blocked
      // carries a steady halo (`.cs-mark-halo-still`), not the pulsing one.
      expect(!!svg.querySelector(".cs-mark-halo")).toBe(
        mark === "question" || mark === "permission",
      );
      unmount();
    }
  });

  it("is drawn in its own pixel space at the small sizes, 1.5px strokes", () => {
    for (const [size, px] of [
      ["sm", 6],
      [7, 7],
      ["md", 8],
    ] as const) {
      const { el, svg, unmount } = draw("ready", size);
      expect(el.style.width).toBe(`${px}px`);
      expect(svg.getAttribute("viewBox")).toBe(`0 0 ${px} ${px}`);
      expect(svg.querySelector("circle")?.getAttribute("stroke-width")).toBe("1.5");
      unmount();
    }
  });

  it("with words, is a labelled image; without, decoration", () => {
    render(<StatusMark mark="permission" label="Needs approval · 2m" />);
    expect(screen.getByRole("img", { name: "Needs approval · 2m" })).toHaveAttribute(
      "data-mark",
      "permission",
    );
    const { el } = draw("ready");
    expect(el).toHaveAttribute("aria-hidden", "true");
  });
});

describe("StatusMark — updates in place (SV-PBF-171)", () => {
  // A press re-renders the tray chip. If the mark's SVG children are torn down and rebuilt on a
  // re-render with the same drawing, the pointerdown target is detached before pointerup and the
  // browser never dispatches `click`, so the chip's select is swallowed.
  it.each(["question", "permission", "blocked", "error"] as const)(
    "a re-render of the same %s mark replaces none of its SVG nodes",
    (mark) => {
      const { container, rerender } = render(<StatusMark mark={mark} size="sm" />);
      const svg = container.querySelector("svg")!;
      const before = [...svg.querySelectorAll("*")];
      const seen: MutationRecord[] = [];
      const mo = new MutationObserver((ms) => seen.push(...ms));
      mo.observe(svg, { childList: true, subtree: true });
      rerender(<StatusMark mark={mark} size="sm" data-pressed="true" />);
      seen.push(...mo.takeRecords());
      mo.disconnect();
      expect(seen).toEqual([]);
      expect(container.querySelector("svg")).toBe(svg);
      expect([...svg.querySelectorAll("*")]).toEqual(before);
      before.forEach((n) => expect(n.isConnected).toBe(true));
    },
  );
});

describe("status-mark.css — motion and colour", () => {
  it.each(
    [6, 7].flatMap((size) =>
      ["normal", "pinned"].flatMap((variant) =>
        ["ready", "ended", "crashed", "offline"].map((mark) => ({ size, variant, mark })),
      ),
    ),
  )(
    "$mark $variant row keeps meaningful content at full opacity at $size px",
    ({ size, variant, mark }) => {
      const rules = [...AGENTS_CSS.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
        .filter((rule) => /\[data-(?:dead|offline|pinned)=/.test(rule[1]))
        .map((rule) => rule[0])
        .join("\n");
      const dead = mark === "ended" || mark === "crashed";
      const view = render(
        <>
          <style>{rules}</style>
          <div
            className="ag-row"
            data-dead={dead || undefined}
            data-offline={mark === "offline" || undefined}
            data-pinned={variant.includes("pinned") ? "dim" : undefined}
          >
            <button className="ag-main">
              <span className="ag-avatar-wrap">
                <span className="ag-avatar">AB</span>
                <span className="ag-row-dot ag-avatar-dot">
                  <StatusMark mark={mark as StatusMarkKind} size={size} />
                </span>
              </span>
              <span className="ag-name">{mark}</span>
            </button>
          </div>
        </>,
      );
      const avatar = view.container.querySelector(".ag-avatar")!;
      const expectedOpacity =
        mark === "offline" ? 0.55 : dead ? 0.72 : variant.includes("pinned") ? 0.55 : 1;
      expect(Number(getComputedStyle(avatar).opacity || 1)).toBe(expectedOpacity);
      if (dead) expect(getComputedStyle(avatar).filter).toBe("grayscale(0.6)");
      for (const selector of [
        ".ag-row",
        ".ag-main",
        ".ag-avatar-wrap",
        ".ag-row-dot",
        ".cs-mark",
        ".ag-name",
      ]) {
        expect(
          Number(getComputedStyle(view.container.querySelector(selector)!).opacity || 1),
          selector,
        ).toBe(1);
      }
      view.unmount();
    },
  );

  it("the spinner, the halo and the think dots stop under reduced motion; nothing breathes", () => {
    expect(CSS).toMatch(
      /@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.cs-mark-spin,\s*\.cs-mark-halo,\s*\.cs-mark-think\s*\{\s*animation: none;/,
    );
    expect(CSS).toMatch(/\.cs-mark-halo\s*\{[^}]*animation: cs-mark-call [^;]* 3;/);
    expect(CSS).not.toMatch(/@keyframes [a-z-]*breathe/);
  });

  it("every mark takes its colour from a --cs-status-* token, light and dark", () => {
    for (const mark of STATUS_MARKS) expect(CSS).toContain(`.cs-mark-${mark}`);
    expect(CSS).not.toMatch(/\.cs-mark-(done|check|working|needs|idle)\b/);
    expect(STATUS_MARKS).not.toContain("done" as StatusMarkKind);
    expect(STATUS_MARKS).not.toContain("working" as StatusMarkKind);
    expect(STATUS_MARKS).not.toContain("needs" as StatusMarkKind);
    expect(STATUS_MARKS).not.toContain("idle" as StatusMarkKind);
    for (const mode of ["light", "dark"] as const) {
      const tokens = themeTokens("codesense", mode);
      for (const status of ["busy", "needs", "failed", "idle"])
        expect(tokens[`--theme-status-canvas-${status}`]).toMatch(/^#[a-f\d]{6}$/i);
    }
  });

  it("keeps small pill text and thinking glyphs legible on tinted light and dark surfaces", () => {
    const palettes = (["light", "dark"] as const).map((mode) => themeTokens("codesense", mode));
    const thinkingFrames = CSS.match(/@keyframes cs-mark-think\s*\{([\s\S]*?)\n\}/)![1];
    const minimumOpacity = Math.min(
      ...[...thinkingFrames.matchAll(/opacity: ([\d.]+)/g)].map((m) => Number(m[1])),
    );
    // Compose the actual offline-row CSS with the mark. Testing the token alone
    // misses opacity inherited visually from a surface's wrapper.
    const offlineRules = [...AGENTS_CSS.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .filter((rule) => rule[1].includes('[data-offline="true"]'))
      .map((rule) => rule[0])
      .join("\n");
    const offline = render(
      <>
        <style>{offlineRules}</style>
        <div className="ag-row" data-offline="true">
          <span className="ag-row-dot ag-avatar-dot">
            <StatusMark mark="offline" />
          </span>
        </div>
      </>,
    );
    const offlineOpacity = [
      offline.container.querySelector(".ag-row")!,
      offline.container.querySelector(".ag-row-dot")!,
    ].reduce((opacity, element) => opacity * Number(getComputedStyle(element).opacity || 1), 1);
    for (const [index, palette] of palettes.entries()) {
      const ink = palette["--theme-status-canvas-text"];
      const tones = ["busy", "needs", "failed", "idle"].map(
        (s) => palette[`--theme-status-canvas-${s}`],
      );
      const thinkingTone = palette["--theme-status-canvas-busy"];
      const backgrounds = index === 0 ? ["#ffffff", "#f6f8fa"] : ["#161b22", "#2e3440", "#44475a"];
      for (const background of backgrounds) {
        const offlineTone = palette["--theme-status-canvas-idle"];
        expect(
          contrastRatio(mixHex(background, offlineTone, offlineOpacity), background),
          `offline row mark on ${background}`,
        ).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.nonText);
        for (const tone of tones) {
          const tint = mixHex(background, tone, 0.14);
          expect(contrastRatio(ink, tint), `text on ${background}`).toBeGreaterThanOrEqual(
            CONTRAST_THRESHOLDS.text,
          );
          expect(contrastRatio(tone, background), `mark on ${background}`).toBeGreaterThanOrEqual(
            3,
          );
          expect(contrastRatio(tone, tint), `pill mark on ${background}`).toBeGreaterThanOrEqual(
            CONTRAST_THRESHOLDS.nonText,
          );
        }
        const thinkingTint = mixHex(background, thinkingTone, 0.14);
        expect(
          contrastRatio(mixHex(thinkingTint, thinkingTone, minimumOpacity), thinkingTint),
          `thinking on ${background}`,
        ).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.nonText);
      }
    }
    expect(statusPillStyle("permission").color).toBe("var(--cs-status-text)");
    offline.unmount();
  });
});

describe("the one roll-up rule", () => {
  it("needs you > error and crash > working > stalled > the quiet marks", () => {
    expect(topMark(["tool", "question", "crashed"])).toBe("question");
    expect(topMark(["tool", "crashed"])).toBe("crashed");
    expect(topMark(["tool", "error"])).toBe("error");
    expect(topMark(["ready", "tool"])).toBe("tool");
    expect(topMark(["ready", "stalled"])).toBe("stalled");
    expect(topMark(["ready", "unknown", "ended"])).toBe("ready");
    expect(topMark(["permission", "blocked", "crashed"])).toBe("permission");
    expect(topMark(["blocked", "crashed"])).toBe("blocked");
    expect(topMark(["thinking", "tool"])).toBe("tool");
    expect(topMark([])).toBeUndefined();
  });

  it("names a roll-up by its members, most urgent first, leaving quiet ones out", () => {
    const counts = countMarks(["question", "tool", "question", "ready"]);
    expect(rollupWords(counts)).toBe("2 need you · 1 running a tool");
    expect(rollupWords(counts, ", ")).toBe("2 need you, 1 running a tool");
    expect(rollupWords(countMarks(["ready", "ended"]))).toBe("1 ready · 1 ended");
  });

  it("maps the tabs' statuses and the needs-you kinds onto the finer marks", () => {
    expect(markOfTerminalStatus("thinking")).toBe("thinking");
    expect(markOfTerminalStatus("running")).toBe("thinking");
    expect(markOfTerminalStatus("needs-permission")).toBe("permission");
    expect(markOfTerminalStatus("done")).toBe("ready");
    expect(markOfTerminalStatus(undefined)).toBe("ready");
    expect(markOfNeedsYouKind("question")).toBe("question");
    expect(markOfNeedsYouKind("permission")).toBe("permission");
    expect(markOfNeedsYouKind("blocked")).toBe("blocked");
    expect(markOfNeedsYouKind("failed")).toBe("error");
    expect(markOfNeedsYouKind("crashed")).toBe("crashed");
    expect(markOfNeedsYouKind("startup")).toBe("question");
  });

  it("maps every agent status onto its single mark (one table, every surface)", () => {
    expect(markOfAgentStatus("thinking")).toBe("thinking");
    expect(markOfAgentStatus("tool-running")).toBe("tool");
    expect(markOfAgentStatus("running")).toBe("thinking");
    expect(markOfAgentStatus("paused")).toBe("permission");
    expect(markOfAgentStatus("blocked")).toBe("blocked");
    expect(markOfAgentStatus("awaiting-answer")).toBe("question");
    expect(markOfAgentStatus("idle")).toBe("ready");
    expect(markOfAgentStatus("stalled")).toBe("stalled");
    expect(markOfAgentStatus("unknown")).toBe("unknown");
    expect(markOfAgentStatus("completed")).toBe("ended");
    expect(markOfAgentStatus("done")).toBe("ended");
    expect(markOfAgentStatus("dropped")).toBe("question");
    expect(markOfAgentStatus("failed")).toBe("error");
    expect(markOfAgentStatus("errored")).toBe("error");
    expect(markOfAgentStatus("crashed")).toBe("crashed");
  });
});

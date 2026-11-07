// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";
import { TERMINAL_THEMES } from "@/features/viewer/components/terminal-constants";
import {
  IMAGE_DIM,
  MachineCanvas,
  WATERMARK_OPACITY,
  machineWatermarkText,
  tintPaneBackground,
} from "./machine-canvas";
import {
  MACHINE_PALETTE,
  contrastRatio,
  isDarkColor,
  lightenOver,
  mixHex,
  multiplyOver,
} from "../lib/machine-color";
import {
  IMAGE_OPACITY,
  __resetMachineAppearanceForTest,
  useMachineAppearanceStore,
} from "../lib/machine-appearance";

afterEach(() => __resetMachineAppearanceForTest());

const THEMES = Object.entries(TERMINAL_THEMES).map(([name, c]) => [name, c.theme] as const);
/** The dark terminal themes (the mark lightens over them). */
const DARK_THEMES = THEMES.filter(([, t]) => isDarkColor(t.background!));
/** Light grounds (a light terminal theme, or the app's light chrome): the mark
 *  multiplies over them. */
const LIGHT_GROUNDS = [
  ...THEMES.filter(([, t]) => !isDarkColor(t.background!)).map(
    ([n, t]) => [n, t.background!] as const,
  ),
  ["white", "#ffffff"] as const,
  ["paper", "#f6f8fa"] as const,
];

const DEFAULT = TERMINAL_THEMES["github-dark"].theme;
/** The image's whitest pixel after the dark-theme dim filter (saturate leaves white white). */
const DIMMED_WHITE = mixHex("#000000", "#ffffff", IMAGE_DIM.brightness);

describe("the watermark stays a whisper", () => {
  it("over the default terminal, every palette color is within 1.15:1 of the ground", () => {
    for (const { hex } of MACHINE_PALETTE) {
      const ground = tintPaneBackground(DEFAULT.background!, hex);
      const mark = lightenOver(ground, hex, WATERMARK_OPACITY.alone);
      expect(contrastRatio(mark, ground)).toBeLessThanOrEqual(1.15);
    }
  });
  it.each(DARK_THEMES)("over %s it stays near the ground too", (_, theme) => {
    for (const { hex } of MACHINE_PALETTE) {
      const ground = tintPaneBackground(theme.background!, hex);
      const mark = lightenOver(ground, hex, WATERMARK_OPACITY.alone);
      expect(contrastRatio(mark, ground)).toBeLessThanOrEqual(1.16);
    }
  });
});

describe("over a light ground the mark multiplies and stays a whisper", () => {
  it.each(LIGHT_GROUNDS)("over %s, every palette color is within 1.15:1", (_, bg) => {
    for (const { hex } of MACHINE_PALETTE) {
      const ground = tintPaneBackground(bg, hex);
      const mark = multiplyOver(ground, hex, WATERMARK_OPACITY.alone);
      expect(contrastRatio(mark, ground)).toBeLessThanOrEqual(1.15);
    }
  });
});

describe("a background image never costs text its readability", () => {
  it("on the default terminal at full strength over a white pixel, dim text keeps ≥ 3:1", () => {
    for (const { hex } of MACHINE_PALETTE) {
      const ground = tintPaneBackground(DEFAULT.background!, hex);
      const under = lightenOver(ground, DIMMED_WHITE, IMAGE_OPACITY.max);
      expect(contrastRatio("#8b949e", under)).toBeGreaterThanOrEqual(3);
      expect(contrastRatio(DEFAULT.foreground!, under)).toBeGreaterThanOrEqual(4.5);
    }
  });
  it.each(DARK_THEMES)(
    "on %s at full strength over a white pixel, text keeps AA where the theme has room",
    (_, theme) => {
      // A theme whose own text/ground contrast is high (≥ 7:1) keeps 4.5:1; one
      // that starts near the floor (Solarized's 4.75:1) keeps 3:1.
      const floor = contrastRatio(theme.foreground!, theme.background!) >= 7 ? 4.5 : 3;
      for (const { hex } of MACHINE_PALETTE) {
        const ground = tintPaneBackground(theme.background!, hex);
        const under = lightenOver(ground, DIMMED_WHITE, IMAGE_OPACITY.max);
        expect(contrastRatio(theme.foreground!, under)).toBeGreaterThanOrEqual(floor);
      }
    },
  );
});

describe("machineWatermarkText", () => {
  it("is the host's name without a trailing note", () => {
    expect(machineWatermarkText("codesense-test-1 (private)")).toBe("codesense-test-1");
    expect(machineWatermarkText("gpu-box")).toBe("gpu-box");
  });
  it("falls back to the address when the label is only a note", () => {
    expect(machineWatermarkText("(private)", "192.0.2.12")).toBe("192.0.2.12");
  });
});

describe("MachineCanvas", () => {
  it("dims the image over a dark terminal and keeps it under the text", () => {
    useMachineAppearanceStore.setState({ images: { box: "data:image/webp;base64,AA" } });
    render(<MachineCanvas machineId="box" hostname="box" background="#0d1117" />);
    const img = screen.getByTestId("machine-canvas-image-box");
    expect(img.style.filter).toContain("brightness(0.55)");
    expect(img.style.opacity).toBe(String(IMAGE_OPACITY.default));
    expect(screen.getByTestId("machine-canvas-box").style.mixBlendMode).toBe("lighten");
    expect(screen.getByTestId("machine-watermark-box").style.opacity).toBe(
      String(WATERMARK_OPACITY.withImage),
    );
  });
  it("anchors the mark to the pane's right edge at mid-height", () => {
    render(<MachineCanvas machineId="box" hostname="box" background="#0d1117" />);
    const mark = screen.getByTestId("machine-watermark-box");
    expect(mark.className).toContain("top-1/2");
    expect(mark.style.transform).toBe("translateY(-50%) rotate(-9deg)");
    expect(mark.style.opacity).toBe(String(WATERMARK_OPACITY.alone));
  });
});

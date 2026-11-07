// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  CHROME_CLUSTER_PAD_W,
  CHROME_CONTROL_W,
  CHROME_HIDDEN_HINT_COMPACT_W,
  CHROME_HIDDEN_HINT_W,
  CHROME_NEW_TAB_W,
  CHROME_OVERFLOW_W,
  CHROME_ROLLUP_W,
  CHROME_IDENTITY_MIN_W,
  MAX_VISIBLE_TABS,
  MAX_VISIBLE_TABS_ZOOMED,
  TAB_NAME_MIN_W,
  TAB_PILL_MIN_W,
  TEXT_XS_CHAR_W,
  fitTileChrome,
  overflowLabel,
  splitForMiddleTruncation,
  type TileChromeFit,
  type TileChromeFitInput,
} from "./tile-chrome-fit";
import { tileWidths } from "./tile-widths.test-util";

/** The shrink-proof width the fitted chrome occupies (everything but the
 *  identity label, which yields). Mirrors the inline sizes the chrome applies. */
function occupiedWidth(input: TileChromeFitInput, fit: TileChromeFit): number {
  let w = CHROME_CLUSTER_PAD_W + CHROME_NEW_TAB_W + fit.visibleTabs * TAB_PILL_MIN_W;
  if (fit.showRollup) w += CHROME_ROLLUP_W;
  // (The roll-up only renders in the tabsInMenu case, where "+N" doesn't.)
  if (fit.visibleTabs < input.tabCount && !fit.tabsInMenu) w += CHROME_OVERFLOW_W;
  if (fit.showKebab) w += CHROME_CONTROL_W;
  if (!fit.collapseControls) w += input.windowControlCount * CHROME_CONTROL_W;
  if (input.hasHiddenHint)
    w += fit.compactHiddenHint ? CHROME_HIDDEN_HINT_COMPACT_W : CHROME_HIDDEN_HINT_W;
  return w;
}

const base: TileChromeFitInput = {
  width: 1200,
  tabCount: 3,
  maxTabs: MAX_VISIBLE_TABS,
  hasIdentity: true,
  hasViewOptions: true,
  windowControlCount: 3,
  hasHiddenHint: false,
};

describe("fitTileChrome", () => {
  it("unmeasured keeps the count-only split with controls inline", () => {
    const fit = fitTileChrome({ ...base, width: null, tabCount: 6 });
    expect(fit).toMatchObject({ visibleTabs: 4, collapseControls: false, showRollup: false });
  });

  it("a wide tile shows every tab and every control inline", () => {
    const fit = fitTileChrome(base);
    expect(fit).toMatchObject({
      visibleTabs: 3,
      collapseControls: false,
      tabsInMenu: false,
      showRollup: false,
    });
  });

  it("the count cap still overflows a wide tile (zoomed raises it)", () => {
    expect(fitTileChrome({ ...base, tabCount: 6 }).visibleTabs).toBe(MAX_VISIBLE_TABS);
    expect(
      fitTileChrome({ ...base, tabCount: 10, maxTabs: MAX_VISIBLE_TABS_ZOOMED }).visibleTabs,
    ).toBe(MAX_VISIBLE_TABS_ZOOMED);
  });

  it("folds window controls into ⋮ before dropping the active tab's pill", () => {
    // 260px: controls inline would leave no room for a pill, folded leaves one.
    const fit = fitTileChrome({ ...base, width: 260 });
    expect(fit.collapseControls).toBe(true);
    expect(fit.visibleTabs).toBe(1);
    expect(fit.showKebab).toBe(true);
    // The "+N" already shows the hidden tabs' status: no roll-up beside it.
    expect(fit.showRollup).toBe(false);
  });

  it("drops the label reserve to keep the active pill (6-up columns @1200 = 200px)", () => {
    const width = tileWidths(6, "columns", 1200)[0];
    const fit = fitTileChrome({ ...base, width });
    expect(fit).toMatchObject({ visibleTabs: 1, collapseControls: true, showRollup: false });
    // The label reserve would not have left room for the pill.
    const withReserve =
      width -
      CHROME_CLUSTER_PAD_W -
      CHROME_NEW_TAB_W -
      CHROME_CONTROL_W -
      CHROME_IDENTITY_MIN_W -
      CHROME_OVERFLOW_W;
    expect(withReserve).toBeLessThan(TAB_PILL_MIN_W);
  });

  it("when the active pill can't be kept at all, controls stay inline if they fit", () => {
    // 160px, one control, no view options: even folded and without the label
    // reserve there's no room for a pill, but inline control + "+N" fit.
    const fit = fitTileChrome({
      ...base,
      width: 160,
      windowControlCount: 1,
      hasViewOptions: false,
    });
    expect(fit.visibleTabs).toBe(0);
    expect(fit.collapseControls).toBe(false);
  });

  it("the ⋮ appears when controls fold even for a kind with no view options", () => {
    const fit = fitTileChrome({ ...base, width: 200, hasViewOptions: false });
    expect(fit.collapseControls).toBe(true);
    expect(fit.showKebab).toBe(true);
  });

  it("at the narrowest, the tabs move into ⋮ and the roll-up keeps status", () => {
    const fit = fitTileChrome({ ...base, width: 80 });
    expect(fit).toMatchObject({ visibleTabs: 0, tabsInMenu: true, showKebab: true });
    expect(fit.showRollup).toBe(true);
  });

  it("a zoomed tile's hidden hint compacts with the controls", () => {
    const fit = fitTileChrome({ ...base, width: 240, hasHiddenHint: true });
    expect(fit.collapseControls).toBe(true);
    expect(fit.compactHiddenHint).toBe(true);
  });

  it("no tabs: nothing overflows and no roll-up", () => {
    const fit = fitTileChrome({ ...base, width: 150, tabCount: 0 });
    expect(fit.visibleTabs).toBe(0);
    expect(fit.showRollup).toBe(false);
    expect(fit.tabsInMenu).toBe(false);
  });

  // The seam: sweep the real mosaic arrangement at 1/2/4/6-up (columns and
  // grid, zoomed and not) across realistic CLI region widths. At every tile the
  // shrink-proof pieces must fit inside the tile (nothing clipped or covered),
  // every tab's status must be on screen, and every control inline or in ⋮.
  const regions = [720, 900, 1200, 1600];
  const layouts = [1, 2, 4, 6].flatMap((n) =>
    (["columns", "grid"] as const).map((preset) => ({ n, preset })),
  );
  for (const { n, preset } of layouts) {
    for (const region of regions) {
      for (const zoomed of [false, true]) {
        for (const tabCount of [1, 3, 6]) {
          const widths = zoomed ? [region] : tileWidths(n, preset, region);
          it(`${n}-up ${preset}${zoomed ? " zoomed" : ""} @${region}px, ${tabCount} tabs: fits`, () => {
            for (const width of widths) {
              const input: TileChromeFitInput = {
                ...base,
                width,
                tabCount,
                maxTabs: zoomed ? MAX_VISIBLE_TABS_ZOOMED : MAX_VISIBLE_TABS,
                windowControlCount: n > 1 ? 3 : 2,
                hasHiddenHint: zoomed && n > 1,
              };
              const fit = fitTileChrome(input);
              expect(occupiedWidth(input, fit)).toBeLessThanOrEqual(width);
              // Status: any tab off its pill → on the "+N", or (folded into ⋮)
              // on the roll-up; never both, never neither.
              if (fit.visibleTabs < tabCount) {
                expect(fit.showRollup).toBe(fit.tabsInMenu);
              } else {
                expect(fit.showRollup).toBe(false);
              }
              // Controls: inline, or folded into a ⋮ that renders.
              if (fit.collapseControls) expect(fit.showKebab).toBe(true);
              // 6-up on a real CLI region keeps every tab reachable without ⋮.
              if (width >= 120) expect(fit.tabsInMenu).toBe(false);
            }
          });
        }
      }
    }
  }
});

describe("splitForMiddleTruncation", () => {
  it("leaves short labels whole", () => {
    expect(splitForMiddleTruncation("zsh 2")).toEqual({ head: "zsh 2", tail: "" });
    expect(splitForMiddleTruncation("SW-EW-3")).toEqual({ head: "SW-EW-3", tail: "" });
  });

  it("keeps the distinguishing tail of sibling labels", () => {
    const a = splitForMiddleTruncation("SW-FX-101-lag");
    const b = splitForMiddleTruncation("SW-FX-101-log");
    expect(a).toEqual({ head: "SW-FX-1", tail: "01-lag" });
    expect(a.tail).not.toBe(b.tail);
    expect(splitForMiddleTruncation("SW-UX-CLI-2").tail).toBe("CLI-2");
    expect(splitForMiddleTruncation("SW-UX-CLI-12").tail).toBe("LI-12");
  });

  it("head + tail always reassemble the label", () => {
    for (const s of ["abcdefghi", "QH6-PARALLEL-3", "x".repeat(200)]) {
      const { head, tail } = splitForMiddleTruncation(s);
      expect(head + tail).toBe(s);
      expect(tail.length).toBeLessThanOrEqual(6);
    }
  });
});

describe("middle truncation never spills (tail capped by the available width)", () => {
  it("a long tab name in a pill at its minimum width fits the name slot", () => {
    expect(TAB_NAME_MIN_W).toBeGreaterThanOrEqual(26);
    const available = Math.floor(TAB_NAME_MIN_W / TEXT_XS_CHAR_W);
    expect(available).toBe(3);
    const name = "claude-code-review-for-SW-UX-CLI-2";
    const { head, tail } = splitForMiddleTruncation(name, available);
    expect(head + tail).toBe(name);
    // Shown: at least one head char + "…" + the tail — never more than fits.
    expect(1 + 1 + tail.length).toBeLessThanOrEqual(available);
    // The uncapped tail (6 chars ≈ 42px) would spill past the 26px slot.
    expect(splitForMiddleTruncation(name).tail.length * TEXT_XS_CHAR_W).toBeGreaterThan(
      TAB_NAME_MIN_W,
    );
  });

  it("for every width, head (≥ 1) + ellipsis + tail fit the available characters", () => {
    const name = "QH6-PARALLEL-3-extra-long-name";
    for (let available = 3; available < name.length; available++) {
      const { tail } = splitForMiddleTruncation(name, available);
      expect(tail.length).toBeGreaterThanOrEqual(1);
      expect(1 + 1 + tail.length).toBeLessThanOrEqual(available);
    }
    // Fits → not split.
    expect(splitForMiddleTruncation(name, name.length).tail).toBe("");
  });
});

describe("overflowLabel", () => {
  it("pluralizes and names what the hidden items hold", () => {
    expect(overflowLabel(1, ["tab", "tabs"])).toBe("1 more tab");
    expect(overflowLabel(3, ["tab", "tabs"])).toBe("3 more tabs");
    expect(overflowLabel(2, ["tab", "tabs"], "1 needs you, 1 working")).toBe(
      "2 more tabs: 1 needs you, 1 working",
    );
    expect(overflowLabel(1, ["epic", "epics"], "Needs approval")).toBe(
      "1 more epic: Needs approval",
    );
  });
});

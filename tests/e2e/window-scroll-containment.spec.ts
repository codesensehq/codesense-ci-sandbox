// SPDX-License-Identifier: MIT
import { expect, test } from "../support/fixtures/tauri";

/**
 * SW-FX-88: the app window must never scroll as a whole on either axis — the
 * viewport (`documentElement`) is pinned and only inner panels own their scroll.
 *
 * The guard is asserted with the correct primitives (see the story's AC-7):
 *  - a REAL wheel gesture must not move the window (`window.scrollX/Y` stay 0);
 *  - computed `overflow` on the root is `hidden` (the propagating pin).
 * It deliberately does NOT assert `scrollHeight <= clientHeight` (unaffected by
 * `overflow:hidden`, which measures content, not scrollability) nor rely on
 * programmatic `window.scrollTo` (which bypasses `overflow:hidden`).
 *
 * The team landing the mocked app boots into (the team's nav, its file prompt and the CLI's
 * "Start an agent" empty state; since SV-PLG-46 an authenticated launch opens the active team
 * directly rather than the "Add a source repo to …" empty state) is the populated surface: its
 * content outgrows a short viewport, so before the root clip existed the whole document scrolled.
 */

const landing = (page: import("@playwright/test").Page) =>
  page.getByRole("button", { name: "Start an agent" });

async function windowScrollOffset(page: import("@playwright/test").Page) {
  return page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }));
}

/** Give the document content taller and wider than the viewport (a block in
 *  <body>), so a wheel the root clip failed to stop WOULD scroll the window.
 *  Without it, a landing that fits the viewport leaves nothing to scroll and the
 *  wheel checks below would pass vacuously. */
async function overflowTheDocument(page: import("@playwright/test").Page) {
  const overflows = await page.evaluate(() => {
    const tall = document.createElement("div");
    tall.style.cssText = "height: 4000px; width: 4000px; flex: none;";
    document.body.appendChild(tall);
    const body = document.body;
    return body.scrollHeight > window.innerHeight && body.scrollWidth > window.innerWidth;
  });
  expect(overflows).toBe(true);
}

async function attemptWheelScroll(page: import("@playwright/test").Page) {
  // Wheel over the TOP chrome (title bar), which is not an inner scroll region, so
  // the gesture targets the document scroll chain rather than being absorbed by a
  // panel's own `overflow:auto`. A real user gesture, not a programmatic scroll.
  const { width } = page.viewportSize() ?? { width: 800 };
  await page.mouse.move(Math.floor(width / 2), 8);
  await page.mouse.wheel(0, 2000);
  await page.mouse.wheel(2000, 0);
}

test.describe("@P1 @E2E Window scroll containment (SW-FX-88)", () => {
  test("the root is structurally pinned — computed overflow is hidden on html/body/#root", async ({
    tauriApp,
  }) => {
    const overflow = await tauriApp.evaluate(() => {
      const of = (el: Element | null) => {
        if (!el) return null;
        const s = getComputedStyle(el);
        return { x: s.overflowX, y: s.overflowY };
      };
      return {
        html: of(document.documentElement),
        body: of(document.body),
        root: of(document.getElementById("root")),
      };
    });
    expect(overflow.html).toEqual({ x: "hidden", y: "hidden" });
    expect(overflow.body).toEqual({ x: "hidden", y: "hidden" });
    expect(overflow.root).toEqual({ x: "hidden", y: "hidden" });
  });

  test("a wheel gesture never scrolls the window at a normal size", async ({ tauriApp }) => {
    await tauriApp.setViewportSize({ width: 1024, height: 640 });
    await expect(landing(tauriApp)).toBeVisible();
    await overflowTheDocument(tauriApp);

    await attemptWheelScroll(tauriApp);

    expect(await windowScrollOffset(tauriApp)).toEqual({ x: 0, y: 0 });
  });

  test("a wheel gesture never scrolls the window in a very small window", async ({ tauriApp }) => {
    await tauriApp.setViewportSize({ width: 640, height: 480 });
    await expect(landing(tauriApp)).toBeVisible();
    await overflowTheDocument(tauriApp);

    await attemptWheelScroll(tauriApp);

    expect(await windowScrollOffset(tauriApp)).toEqual({ x: 0, y: 0 });
  });

  test.describe("a team nav taller than the window", () => {
    const ROWS = 40;
    const LAST_ROW = `folder-${ROWS - 1}`;
    test.use({
      tauriOverrides: {
        list_directory: Array.from({ length: ROWS }, (_, i) => ({
          name: `folder-${i}`,
          path: `/mock/repos/my-project/.context/folder-${i}`,
          is_dir: true,
          extension: null,
        })),
      },
    });

    test("tall content owns its own scroll — nothing is hidden by the root clip", async ({
      tauriApp,
    }) => {
      // The team nav keeps its rows in an internal `overflow-y-auto` region. Under the root clip
      // that region, not the window, is the scroller (so rows stay reachable) at a short height.
      await tauriApp.setViewportSize({ width: 1024, height: 360 });
      await expect(landing(tauriApp)).toBeVisible();

      const firstRow = tauriApp.getByRole("button", { name: "folder-0", exact: true });
      await expect(firstRow).toBeVisible();
      const lastRow = tauriApp.getByRole("button", { name: LAST_ROW, exact: true });
      // Hold the region as a handle: the windowed rows (including this first one) unmount on scroll.
      const scroller = await firstRow
        .locator("xpath=ancestor::*[contains(@class,'overflow-y-auto')][1]")
        .elementHandle();
      expect(scroller).not.toBeNull();

      const range = await scroller!.evaluate((el) => ({
        scrollHeight: el.scrollHeight,
        clientHeight: el.clientHeight,
      }));
      expect(range.scrollHeight).toBeGreaterThan(range.clientHeight);

      // The nav windows its rows, so the last one is not mounted until the region itself scrolls
      // to the end; a real wheel over the region (not the window) is what a user does.
      await expect(lastRow).toHaveCount(0);
      const box = (await scroller!.boundingBox())!;
      await tauriApp.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      for (let i = 0; i < 12; i++) await tauriApp.mouse.wheel(0, 2000);
      await expect(lastRow).toBeInViewport({ ratio: 1 });
      expect(await scroller!.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
      expect(await windowScrollOffset(tauriApp)).toEqual({ x: 0, y: 0 });
    });
  });
});

// SW-SN-15: SW-FX-88 above proves a *user gesture* can't scroll the window. This
// block proves the complementary case — a *programmatic* heading jump can't
// either. The scratch-note outline/sticky/modal jump used `Element.scrollIntoView`,
// which per the CSSOM-view spec walks every scrollable ancestor up to the
// viewport; because an `overflow:hidden` box is still programmatically
// scrollable, the SW-FX-88 root clip did NOT stop it, so the whole shell jumped.
// The fix scrolls the note's own port (`editor.view.dom`) via `scrollTop`. Only a
// real browser exercises ancestor scroll-chaining (jsdom/happy-dom can't), so the
// invariant is pinned here by driving the REAL production `scrollToHeadingPos`
// (the single leaf all three jump surfaces share) against a synthetic
// scroll-port-inside-a-scrollable-ancestor DOM, with a positive control that
// reproduces the pre-fix ancestor displacement. Scope note: this exercises the
// shared scroll leaf in a real browser, NOT the full outline/sticky/modal
// dispatch (those stay in the unit suites) and NOT the shipped app tree.
// Requires the Vite dev server (playwright.config `webServer`): the fixture
// dynamic-imports `/src/...` through Vite's module graph, so it does not run
// against a production/preview build.
test.describe("@P1 @E2E Heading-jump scroll containment (SW-SN-15)", () => {
  // Build the fixture in the page and return the two offsets after the jump: the
  // scrollable ancestor ("app shell") and the note port (`.scratch-note-editor`).
  // `mode: "fix"` drives the real `scrollToHeadingPos`; `mode: "old"` drives the
  // pre-fix `Element.scrollIntoView`.
  async function jumpAndMeasure(page: import("@playwright/test").Page, mode: "fix" | "old") {
    return page.evaluate(async (m) => {
      const shell = document.createElement("div"); // the scrollable "app shell" ancestor
      shell.style.cssText =
        "position:fixed;top:0;left:0;height:400px;width:600px;overflow:auto;background:#fff;z-index:99999;";
      const filler = document.createElement("div"); // makes the shell itself scrollable
      filler.style.cssText = "height:2000px;";
      const port = document.createElement("div"); // = .scratch-note-editor (editor.view.dom)
      port.className = "scratch-note-editor";
      port.style.cssText = "height:300px;overflow-y:auto;";
      const inner = document.createElement("div");
      inner.style.cssText = "height:3000px;position:relative;";
      const heading = document.createElement("h2");
      heading.textContent = "Prompts";
      heading.style.cssText = "position:absolute;top:1500px;";
      inner.appendChild(heading);
      port.appendChild(inner);
      filler.appendChild(port);
      shell.appendChild(filler);
      document.body.appendChild(shell);

      shell.scrollTop = 150; // ancestor pre-scrolled (simulates real app state)
      port.scrollTop = 0;
      try {
        if (m === "fix") {
          const { scrollToHeadingPos } = await import("/src/features/viewer/lib/note-headings.ts");
          scrollToHeadingPos({ view: { dom: port, nodeDOM: () => heading } }, 0);
        } else {
          heading.scrollIntoView({ behavior: "instant", block: "start" });
        }
        return {
          shellScrollTop: shell.scrollTop,
          portScrollTop: port.scrollTop,
          headingOffsetFromPortTop: Math.round(
            heading.getBoundingClientRect().top - port.getBoundingClientRect().top,
          ),
        };
      } finally {
        shell.remove();
      }
    }, mode);
  }

  test("a heading jump scrolls only the note port — the app-shell ancestor never moves", async ({
    tauriApp,
  }) => {
    const r = await jumpAndMeasure(tauriApp, "fix");
    expect(r.shellScrollTop).toBe(150); // the ancestor (whole shell) did NOT move
    expect(r.portScrollTop).toBeGreaterThan(0); // the note's own port scrolled
    expect(r.headingOffsetFromPortTop).toBe(0); // heading landed at the port top
  });

  test("positive control: the pre-fix scrollIntoView DID drag the ancestor (the bug)", async ({
    tauriApp,
  }) => {
    // Guards the assumption the fix relies on: in a real browser the ancestor is
    // genuinely part of the scroll chain, so the fix's "shell unchanged" result is
    // meaningful and not a no-op. If this ever stops moving the shell, the fix test
    // above would lose its teeth and this canary tells us why.
    const r = await jumpAndMeasure(tauriApp, "old");
    expect(r.shellScrollTop).not.toBe(150); // scrollIntoView walked into the ancestor
  });

  // SW-FX-97: SW-SN-15 removed the ancestor-walking `scrollIntoView`, but a
  // gesture/momentum rubber-band at the note port's scroll boundary could still
  // CHAIN its overscroll to the (programmatically-scrollable) app root and shove the
  // whole shell up — the residual "black band at the bottom" jump. `overscroll-behavior:
  // contain` on `.scratch-note-editor` severs that chain. jsdom can't observe
  // overscroll, so assert the remedy resolves on the real, globally-loaded class here.
  // The notes redesign (#2046) moved the note's scroll port from the editor root
  // (`.scratch-note-editor`) to the sheet around it (`.nt-scroll`, see
  // note-headings.ts), and the containment moved with it.
  test("the note port declares overscroll-behavior: contain so a boundary rubber-band can't chain to the shell (SW-FX-97)", async ({
    tauriApp,
  }) => {
    const behavior = await tauriApp.evaluate(() => {
      const port = document.createElement("div");
      port.className = "nt-scroll"; // = the sheet around editor.view.dom in the real app
      document.body.appendChild(port);
      try {
        // The note port only scrolls vertically; `overscrollBehaviorY` is the axis
        // that severs the chain. The `overscroll-behavior: contain` shorthand
        // resolves to `contain` on both longhands.
        return getComputedStyle(port).overscrollBehaviorY;
      } finally {
        port.remove();
      }
    });
    expect(behavior).toBe("contain");
  });
});

// SW-FX-97: the keystone. SW-FX-88 pins the root with `overflow: hidden`, but that
// leaves the document PROGRAMMATICALLY scrollable — a native focus-reveal (clicking a
// note in the rail) or a `scrollIntoView` can shift the document offset and shove the
// whole app shell up (a black band at the bottom that compounds on every switch).
// `useShellScrollContainment` (App-root) snaps the document back to origin on any
// scroll. jsdom can't scroll an `overflow:hidden` root, so this real-browser test is
// the true guard on the fix (the unit test only exercises a MOCKED scrollingElement).
test.describe("@P1 @E2E Shell scroll containment — the document never stays scrolled (SW-FX-97)", () => {
  test("a document scroll (the focus-reveal / scrollIntoView vector) is snapped back to the origin", async ({
    tauriApp,
  }) => {
    const settledTop = await tauriApp.evaluate(async () => {
      const html = document.documentElement;
      const prevOverflow = html.style.overflow;
      // Give the document a real scroll RANGE (the app normally pins it to 0), so
      // `scrollTop` can move — exactly what a focus-reveal does to the live document.
      const filler = document.createElement("div");
      filler.style.cssText = "height:3000px;width:1px;";
      html.appendChild(filler);
      html.style.overflow = "auto";
      try {
        // Simulate the displacement a focus-reveal produces on the real document.
        (document.scrollingElement as HTMLElement).scrollTop = 500;
        // Let the scroll event fire and the App-root guard run.
        await new Promise<void>((resolve) => setTimeout(resolve, 60));
        return (document.scrollingElement as HTMLElement).scrollTop;
      } finally {
        filler.remove();
        html.style.overflow = prevOverflow;
      }
    });
    // The guard reset the offset — the shell can never stay displaced. Without the
    // App-root hook this would remain 500 (the compounding black-band bug).
    expect(settledTop).toBe(0);
  });
});

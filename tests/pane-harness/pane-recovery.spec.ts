// SPDX-License-Identifier: MIT
import type { Page, TestInfo } from "@playwright/test";
import { expect, test, type MountedPane, type PaneFixtures } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

const SAMPLE_PANE = "tests/pane-harness/fixtures/sample-pane.html";

/** Bounded host geometry/state diagnostic. Contains no pane content or credentials. */
async function captureHostGeometry(page: Page) {
  return page.evaluate(() => {
    const iframe = document.querySelector("iframe");
    const host = iframe?.closest("[data-slot-id]");
    const hostRect = host?.getBoundingClientRect();
    const iframeRect = iframe?.getBoundingClientRect();
    const style = iframe ? getComputedStyle(iframe) : null;
    return {
      iframeCount: document.querySelectorAll("iframe").length,
      slotId: host?.getAttribute("data-slot-id") ?? null,
      hostRect: hostRect
        ? { x: hostRect.x, y: hostRect.y, width: hostRect.width, height: hostRect.height }
        : null,
      iframeRect: iframeRect
        ? { x: iframeRect.x, y: iframeRect.y, width: iframeRect.width, height: iframeRect.height }
        : null,
      iframeStyle: style
        ? { opacity: style.opacity, visibility: style.visibility, display: style.display }
        : null,
      iframeSrc: iframe?.getAttribute("src") ?? null,
      activeElement: document.activeElement?.tagName ?? null,
      fallbackStripCount: document.querySelectorAll('[data-testid="slot-fallback-strip"]').length,
    };
  });
}

async function attachGeometry(page: Page, testInfo: TestInfo) {
  await testInfo.attach("host-geometry.json", {
    body: JSON.stringify(await captureHostGeometry(page), null, 2),
    contentType: "application/json",
  });
}

/** Resize the actual pane container that holds the host, so the change survives reveal. */
async function resizeActiveSection(page: Page, width: number, height: number) {
  await page.evaluate(
    ([w, h]) => {
      const sections = [...document.querySelectorAll<HTMLElement>("[data-section]")];
      const active = sections.find((s) => s.clientHeight > 0) ?? sections[0];
      active.style.setProperty("width", `${w}px`, "important");
      active.style.setProperty("height", `${h}px`, "important");
    },
    [width, height] as const,
  );
}

type Size = { width: number; height: number };

type PaneCase = {
  name: string;
  panePath: string;
  fixtures: PaneFixtures;
  contentLocator: string;
  initialSize: Size;
  hiddenSize: Size;
  hideConfirmation: (pane: MountedPane) => Promise<void>;
  upsertWhileHidden: (pane: MountedPane) => Promise<void>;
  assertContent: (pane: MountedPane) => Promise<void>;
};

const cases: PaneCase[] = [
  {
    name: "builtin agent-list",
    panePath: AGENT_LIST_PANE,
    fixtures: agentListFixtures(5),
    contentLocator: '[data-agent-id="s0"]',
    initialSize: { width: 268, height: 480 },
    hiddenSize: { width: 220, height: 360 },
    hideConfirmation: async (pane) => {
      await expect
        .poll(() => pane.probe().then((p) => p.messages))
        .toContain("codesense:visibility:false");
    },
    upsertWhileHidden: async (pane) => {
      const session = agentListFixtures(5).sessions![0];
      await pane.upsert("sessions", {
        ...session,
        rowNames: { primary: "Renamed lead", secondary: "Developer", full: "Renamed lead" },
      });
    },
    assertContent: async (pane) => {
      await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
      await expect(pane.frame.locator('[data-agent-id="s0"] .al-name')).toHaveText("Renamed lead");
    },
  },
  {
    name: "sample user pane",
    panePath: SAMPLE_PANE,
    fixtures: {
      sessions: [
        {
          id: "ada",
          sessionId: "ada",
          handle: "ada",
          kind: "cli",
          alive: true,
          team: { org: "fixture", team: "dev" },
          rowNames: { primary: "Ada", secondary: null, full: "Ada" },
        },
      ],
      theme: { scheme: "light" },
    },
    contentLocator: '#agents li:has-text("Ada")',
    initialSize: { width: 320, height: 480 },
    hiddenSize: { width: 280, height: 360 },
    hideConfirmation: async (pane) => {
      await expect(pane.frame.locator("body")).toHaveAttribute("data-visible", "false");
    },
    upsertWhileHidden: async (pane) => {
      await pane.upsert("sessions", {
        id: "grace",
        sessionId: "grace",
        handle: "grace",
        kind: "cli",
        alive: true,
        team: { org: "fixture", team: "dev" },
        rowNames: { primary: "Grace", secondary: null, full: "Grace" },
      });
    },
    assertContent: async (pane) => {
      await expect(pane.frame.locator('#agents li:has-text("Ada")')).toBeVisible();
      await expect(pane.frame.locator('#agents li:has-text("Grace")')).toBeVisible();
      await expect(pane.frame.locator("#count")).toHaveText("2 sessions");
    },
  },
];

for (const {
  name,
  panePath,
  fixtures,
  contentLocator,
  initialSize,
  hiddenSize,
  hideConfirmation,
  upsertWhileHidden,
  assertContent,
} of cases) {
  test(`${name} pane retains live content and visible host geometry after hide, container resize and bounded idle`, async ({
    mountPane,
    page,
  }, testInfo) => {
    test.setTimeout(45_000);
    const pane = await mountPane(panePath, fixtures);
    try {
      await expect(pane.frame.locator(contentLocator)).toBeVisible();

      const before = await captureHostGeometry(page);
      expect(before.hostRect?.width, "host width before hide").toBeCloseTo(initialSize.width, 0);
      expect(before.hostRect?.height, "host height before hide").toBeCloseTo(initialSize.height, 0);
      expect(before.iframeRect?.width, "iframe width before hide").toBeCloseTo(
        initialSize.width,
        0,
      );
      expect(before.iframeRect?.height, "iframe height before hide").toBeCloseTo(
        initialSize.height,
        0,
      );

      await pane.show(false);
      await hideConfirmation(pane);
      await upsertWhileHidden(pane);
      await resizeActiveSection(page, hiddenSize.width, hiddenSize.height);
      await page.waitForTimeout(20_000);
      await pane.show(true);

      const iframe = page.locator("iframe").first();
      await expect(iframe).toBeVisible();

      const after = await captureHostGeometry(page);
      expect(after.hostRect, "host must have geometry after reveal").not.toBeNull();
      expect(after.iframeRect, "iframe must have geometry after reveal").not.toBeNull();

      const host = after.hostRect!;
      const frame = after.iframeRect!;
      expect(host.width, "host width must match resized container").toBeCloseTo(
        hiddenSize.width,
        0,
      );
      expect(host.height, "host height must match resized container").toBeCloseTo(
        hiddenSize.height,
        0,
      );
      expect(frame.width, "iframe width must match resized container").toBeCloseTo(
        hiddenSize.width,
        0,
      );
      expect(frame.height, "iframe height must match resized container").toBeCloseTo(
        hiddenSize.height,
        0,
      );

      expect(after.iframeStyle?.opacity, "iframe must not be transparent after reveal").not.toBe(
        "0",
      );
      expect(after.iframeStyle?.visibility, "iframe must not be hidden after reveal").not.toBe(
        "hidden",
      );
      expect(after.iframeStyle?.display, "iframe must be displayed after reveal").not.toBe("none");
      await assertContent(pane);
      await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
    } catch (error) {
      await attachGeometry(page, testInfo);
      throw error;
    }
  });
}

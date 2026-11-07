// SPDX-License-Identifier: MIT
/**
 * The dashboard conformance checks. Each mounts a dashboard on the shared fake host (the pane
 * harness) and either passes or throws a ConformanceFailure naming the check and the reason.
 * Policy: passing every check is required before a dashboard becomes a slot's built-in
 * default; any dashboard may still be installed without passing.
 */
import AxeBuilder from "@axe-core/playwright";
import { expect, type Frame } from "@playwright/test";

import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { VERB_REGISTRY } from "../../src/features/dashboard/lib/verb-registry.generated";
import type { Dispatch, MountedPane, PaneFixtures } from "../pane-harness/harness";
import { agentListFixtures } from "../pane-harness/agent-list-fixtures";

import {
  CONTRAST_THRESHOLDS,
  contrastRatio,
  compositeOver,
} from "../../src/shared/lib/color-contrast";
import { hex } from "../pane-harness/colors";
import type { Surface } from "../../src/themes/compiler";

export class ConformanceFailure extends Error {
  constructor(
    readonly check: string,
    readonly reason: string,
  ) {
    super(`${check}: ${reason}`);
  }
}

export type Check = {
  id: string;
  title: string;
  /** Fixtures this check needs on top of the defaults. */
  fixtures?: PaneFixtures;
  run: (pane: MountedPane) => Promise<void>;
};

const FOCUSABLE =
  'button, input:not([type="hidden"]), select, textarea, a[href], [tabindex]:not([tabindex="-1"]), [contenteditable]:not([contenteditable="false"])';
const SETTLE_MS = 600;
/** src-tauri/src/verbs/dispatcher.rs: rolling windows retain calls for < 10_000 ms. */
export const DISPATCHER_BUDGET_WINDOW_MS = 10_000;
const rowMarker = (topic: string, arrival: "before" | "hidden") =>
  `conformance-${topic}-${arrival}`;
const agentFixtures = agentListFixtures(3, "flat");
const agentReferences = agentFixtures.topics!.agentList.filter((row) => row.sessionRef);
const agentReference = (arrival: "before" | "hidden") => ({
  ...agentReferences[0],
  id: rowMarker("agentList", arrival),
  sessionRef: rowMarker("sessions", arrival),
  parentId: null,
  crewParentId: null,
  order: 4,
  crewOrder: 4,
});
const noteRow = (arrival: "before" | "hidden") => {
  const marker = rowMarker("records:notes", arrival);
  return { id: marker, title: marker, body: marker };
};
const inboxTopicRow = (arrival: "before" | "hidden") => {
  const marker = rowMarker("inbox", arrival);
  const row = inboxFixtureRow(arrival === "before" ? 1 : 2, marker, {
    from: marker,
    fromName: marker,
    toName: marker,
    conversation: marker,
    summary: marker,
  });
  return { ...row, id: marker };
};
// Architecture: the backend's agentDefinitions topic holds one `definition` row per agent.
const definitionRow = (arrival: "before" | "hidden") => {
  const marker = rowMarker("agentDefinitions", arrival);
  return {
    id: marker,
    rowType: "definition",
    row: {
      key: marker,
      name: marker,
      description: marker,
      runtime: "claude",
      scope: "team",
      team: "dev",
      avatar: null,
    },
  };
};
const topicRow = (topic: string, arrival: "before" | "hidden") => {
  const marker = rowMarker(topic, arrival);
  if (topic === "agentDefinitions") return definitionRow(arrival);
  if (topic === "inbox") return inboxTopicRow(arrival);
  if (topic === "records:notes") return noteRow(arrival);
  if (topic === "agentList") return agentReference(arrival);
  if (topic === "files")
    return {
      id: `/fixture/${marker}.md`,
      path: `/fixture/${marker}.md`,
      root: "/fixture",
      relativePath: `${marker}.md`,
      name: marker,
      kind: "file",
      scope: "team",
    };
  // Architecture: use one token in every identity/display field. Schema enums and
  // fixed recipient/team routing retain their real values; no alias matching is needed.
  return {
    ...agentRow(marker, marker),
    rowNames: { primary: marker, secondary: marker, full: marker },
    label: marker,
    text: marker,
    createdAtMs: Date.now(),
  };
};

export const agentRow = (id: string, name: string) => ({
  id,
  sessionId: id,
  handle: id,
  kind: "cli",
  alive: true,
  state: "idle",
  displayName: name,
  team: { org: "fixture", team: "dev" },
  rowNames: { primary: name, secondary: null, full: name },
});
// Architecture: each topic exercised by conformance has its marker in the first
// snapshot. Missing fixture coverage must not be mistaken for auxiliary data.
const INITIAL_ROWS = {
  sessions: topicRow("sessions", "before"),
  inbox: inboxTopicRow("before"),
  "records:notes": noteRow("before"),
  agentList: agentReference("before"),
  files: topicRow("files", "before"),
  agentDefinitions: definitionRow("before"),
};
/** What every check mounts with unless it asks for more. */
export const DEFAULT_FIXTURES: PaneFixtures = {
  title: "Conformance",
  activeTab: { "fixture:dev": "/fixture/factory-playbook.html" },
  sessions: [
    agentRow("ada", "Conformance Ada"),
    agentRow("grace", "Conformance Grace"),
    INITIAL_ROWS.sessions,
  ],
  // Architecture: a topic's marker appears only in that topic's rows.
  inbox: [...agentFixtures.inbox!, INITIAL_ROWS.inbox],
  records: [INITIAL_ROWS["records:notes"]],
  topics: {
    ...agentFixtures.topics,
    files: [INITIAL_ROWS.files],
    agentDefinitions: [INITIAL_ROWS.agentDefinitions],
    agentList: [
      ...agentFixtures.topics!.agentList.filter((row) => row.rowType === "state"),
      { ...agentReferences[0], sessionRef: "ada" },
      { ...agentReferences[1], sessionRef: "grace" },
      INITIAL_ROWS.agentList,
    ],
  },
};

const fail = (check: string, reason: string): never => {
  throw new ConformanceFailure(check, reason);
};
const wait = (frame: Frame, ms: number) => frame.page().waitForTimeout(ms);
const text = (frame: Frame) =>
  frame.evaluate(() => (document.body?.innerText ?? "").replace(/\s+/g, " ").trim());
function liveFrame(pane: MountedPane, check: string): Frame {
  return pane.currentFrame() ?? fail(check, "the page is gone: the slot fell back to its built-in");
}
async function settle(pane: MountedPane, check: string) {
  const frame = liveFrame(pane, check);
  await frame.waitForLoadState("load");
  await wait(frame, SETTLE_MS);
  return liveFrame(pane, check);
}
const subscribedTopics = (dispatches: Dispatch[], after = -Infinity) =>
  new Set(
    dispatches
      .filter((d) => d.verb === "subscribe" && d.status === "admit" && d.at > after)
      .map((d) =>
        d.args?.topic === "records" ? `records:${d.args.collection}` : String(d.args?.topic),
      ),
  );
const hostNow = (pane: MountedPane) => pane.page.evaluate(() => performance.now());
const focusedInFrame = (frame: Frame) =>
  frame.evaluate((selector) => {
    const el = document.activeElement;
    return el && el !== document.body && el.matches(selector)
      ? el.tagName.toLowerCase() + (el.id ? `#${el.id}` : "")
      : null;
  }, FOCUSABLE);
async function focusIn(pane: MountedPane, check: string) {
  const frame = await settle(pane, check);
  await pane.page.locator("iframe").focus();
  try {
    await expect.poll(() => focusedInFrame(frame), { timeout: 2_000 }).not.toBeNull();
  } catch {
    fail(check, "focus-in left focus on <body>: no focusable control took it");
  }
  return frame;
}
const hostFocusIsFrame = (pane: MountedPane) =>
  pane.page.evaluate(() => document.activeElement?.tagName === "IFRAME");

const focusInCheck: Check = {
  id: "focus-in",
  title: "focus-in lands on a focusable control",
  run: async (pane) => {
    await focusIn(pane, "focus-in");
  },
};

const escCheck: Check = {
  id: "escape",
  title: "an unhandled Escape returns focus to the host",
  run: async (pane) => {
    await focusIn(pane, "escape");
    await pane.page.keyboard.press("Escape");
    try {
      await expect.poll(() => hostFocusIsFrame(pane), { timeout: 2_000 }).toBe(false);
    } catch {
      fail("escape", "focus stayed in the frame after Escape: the page kept or swallowed it");
    }
  },
};

const flushCheck: Check = {
  id: "flush",
  title: "flush is acknowledged within 300 ms",
  run: async (pane) => {
    await settle(pane, "flush");
    const warnings: string[] = [];
    pane.page.on("console", (message) => {
      if (message.text().startsWith("frame flush timed out")) warnings.push(message.text());
    });
    const ms = await pane.page.evaluate(async () => {
      const start = performance.now();
      await (window as any).control.flushAll(1_000);
      return performance.now() - start;
    });
    if (warnings.length || ms >= 300)
      fail(
        "flush",
        `codesense:flush was not acknowledged within 300 ms (took ${Math.round(ms)} ms)`,
      );
  },
};

const budgetCheck: Check = {
  id: "budget",
  title: "a budget denial shows an error and does not retry",
  // Architecture: dispatcher.rs budgets these classes; reads have no budget.
  fixtures: { budgetDenied: ["metadata", "send", "prefs", "spawn"] },
  run: async (pane) => {
    const frame = await settle(pane, "budget");
    await wait(frame, DISPATCHER_BUDGET_WINDOW_MS);
    liveFrame(pane, "budget");
    const denied = (await pane.dispatches()).filter((d) => d.status === "deny");
    if (!denied.length) return;
    const invocations = (await pane.probe()).invocations;
    for (const cls of new Set(denied.map((d) => d.class))) {
      const rejection = invocations.findIndex(
        (event) => event.kind === "reject" && event.error.includes(`denied: ${cls} budget`),
      );
      if (rejection < 0) fail("budget", `the SDK never received the ${cls} budget rejection`);
      const deniedAt = invocations[rejection].at;
      const retries = invocations
        .slice(rejection + 1)
        .filter(
          (event) =>
            event.kind === "call" &&
            event.at < deniedAt + DISPATCHER_BUDGET_WINDOW_MS &&
            VERB_REGISTRY.some((verb) => verb.name === event.verb && verb.class === cls),
        );
      if (retries.length)
        fail(
          "budget",
          `after "denied: ${cls} budget" the page made ${retries.length} more ${cls} call(s) (${[...new Set(retries.map((d) => d.verb))].join(", ")}): it retries instead of stopping`,
        );
    }
    const shown = await liveFrame(pane, "budget").evaluate(() => {
      const words =
        /denied|budget|error|failed|couldn.?t|can.?t|unavailable|try again|limit|paused/i;
      return (
        Array.from(
          document.querySelectorAll<HTMLElement>(
            '[role="alert"], [role="status"], [aria-live="assertive"], [aria-live="polite"]',
          ),
        ).some((el) => el.checkVisibility() && words.test(el.innerText)) ||
        /denied:\s*\w+ budget/i.test(document.body.innerText)
      );
    });
    if (!shown) fail("budget", `"denied: ${denied[0].class} budget" left no error on the page`);
  },
};

const teamSwitchCheck: Check = {
  id: "team-switch",
  title: "hidden → stale → shown → resync keeps every row",
  run: async (pane) => {
    let frame = await settle(pane, "team-switch");
    const topics = subscribedTopics(await pane.dispatches());
    // Architecture: ui:<slotId> is shared view state, not a list of data rows.
    // A known initial marker also identifies rows fetched through a direct read,
    // such as inboxRollup(), independently of the pane's subscription path.
    const initialText = await text(frame);
    const renderedTopics = Object.keys(INITIAL_ROWS).filter((topic) =>
      initialText.includes(rowMarker(topic, "before")),
    );
    if (!topics.size && !renderedTopics.length) return;
    const rowTopics = [
      ...new Set([...topics].filter((topic) => !topic.startsWith("ui:")).concat(renderedTopics)),
    ];
    for (const topic of rowTopics)
      if (!(topic in INITIAL_ROWS))
        fail("team-switch", `no initial fixture for subscribed topic "${topic}"`);
    // Policy: classify topics only from their initial snapshot, before any updates.
    // A pane with row subscriptions must initially render at least one seeded marker.
    if (rowTopics.length && !renderedTopics.length)
      fail("team-switch", "no subscribed data rendered");
    const messages = async () => (await pane.probe()).messages;
    const heard = (await messages()).length;
    await pane.show(false);
    try {
      await expect
        .poll(async () => (await messages()).slice(heard).includes("codesense:visibility:false"))
        .toBe(true);
    } catch {
      fail("team-switch", "the page was never told it is hidden");
    }
    for (const topic of rowTopics) await pane.upsert(topic, topicRow(topic, "hidden"));
    try {
      await expect
        .poll(async () => {
          const hidden = (await messages()).slice(heard);
          return (
            hidden.includes("codesense:visibility:false") && hidden.includes("codesense:stale")
          );
        })
        .toBe(true);
    } catch {
      fail(
        "team-switch",
        `the hidden page was never told it is stale (heard: ${(await messages()).slice(heard).join(", ") || "nothing"})`,
      );
    }
    const shownAt = await hostNow(pane);
    await pane.show(true);
    frame = liveFrame(pane, "team-switch");
    for (const topic of topics) {
      try {
        await expect
          .poll(async () => subscribedTopics(await pane.dispatches(), shownAt).has(topic), {
            timeout: 3_000,
          })
          .toBe(true);
      } catch {
        fail("team-switch", `shown again, the page never resynced its "${topic}" subscription`);
      }
    }
    for (const topic of renderedTopics) {
      try {
        // A collapsed list may still retain its updated rows while its editor is open.
        await expect
          .poll(() => frame.locator("body").textContent(), { timeout: 3_000 })
          .toContain(rowMarker(topic, "hidden"));
      } catch {
        fail("team-switch", `a "${topic}" row added while hidden never appeared after the resync`);
      }
      if (!(await frame.locator("body").textContent())?.includes(rowMarker(topic, "before")))
        fail("team-switch", `a "${topic}" row shown before hiding was lost after the resync`);
    }
  },
};

const maximizeCheck: Check = {
  id: "maximize",
  title: "maximize and restore without a reload",
  run: async (pane) => {
    const frame = await settle(pane, "maximize");
    const { boot } = await pane.probe();
    for (const overlay of [true, false]) {
      await pane.maximize(overlay);
      await wait(frame, SETTLE_MS);
      const now = pane.currentFrame();
      if (now !== frame || (await pane.probe()).boot !== boot)
        fail("maximize", `the page reloaded on ${overlay ? "maximize" : "restore"}`);
    }
  },
};

const reparentCheck: Check = {
  id: "reparent",
  title: "moving between sections keeps its state and subscriptions",
  run: async (pane) => {
    const frame = await settle(pane, "reparent");
    const before = await text(frame);
    const topics = subscribedTopics(await pane.dispatches());
    const { boot } = await pane.probe();
    const warnings: string[] = [];
    pane.page.on("console", (message) => {
      if (message.text().startsWith("frame flush timed out")) warnings.push(message.text());
    });
    const movedAt = await hostNow(pane);
    await pane.reparent();
    if (warnings.length) fail("reparent", "the page did not acknowledge the flush before the move");
    // A reload during the move is allowed; the page must come back as it was.
    await expect
      .poll(async () => {
        const now = pane.currentFrame();
        return (
          !!now && (await now.evaluate(() => document.readyState).catch(() => "")) === "complete"
        );
      })
      .toBe(true);
    const after = await settle(pane, "reparent");
    for (const topic of topics) {
      if (
        !subscribedTopics(await pane.dispatches(), movedAt).has(topic) &&
        (await pane.probe()).boot !== boot
      )
        fail("reparent", `after the move the page never resumed its "${topic}" subscription`);
    }
    const now = await text(after);
    if (now !== before)
      fail(
        "reparent",
        `the view changed across the move: "${before.slice(0, 80)}" became "${now.slice(0, 80)}"`,
      );
  },
};

const axeCheck: Check = {
  id: "axe",
  title: "axe finds no serious or critical violations, and Tab can leave",
  run: async (pane) => {
    await settle(pane, "axe");
    const { violations } = await new AxeBuilder({ page: pane.page }).include("iframe").analyze();
    const severe = violations.filter((v) => v.impact === "serious" || v.impact === "critical");
    if (severe.length)
      fail(
        "axe",
        severe
          .map(
            (v) =>
              `${v.id} (${v.impact}): ${v.help}; ${v.nodes
                .map((node) => `${node.target.join(" > ")}: ${node.failureSummary}`)
                .join("; ")}`,
          )
          .join("; "),
      );
    await focusIn(pane, "axe");
    for (let i = 0; i < 60 && (await hostFocusIsFrame(pane)); i++)
      await pane.page.keyboard.press("Tab");
    if (await hostFocusIsFrame(pane)) fail("axe", "keyboard trap: 60 Tabs never left the frame");
  },
};

export const themeCheck: Check = {
  id: "theme",
  title: "follows live themes and hidden catch-up without reload, with own-surface contrast",
  run: async (pane) => {
    const frame = await settle(pane, "theme");
    const { boot } = await pane.probe();
    const body = async () => {
      const colors = await frame.locator("body").evaluate((el) => {
        const style = getComputedStyle(el);
        return [style.backgroundColor, style.color];
      });
      return hex(frame, colors);
    };
    // Read the same host declaration as readDocTheme; panes inherit their slot's surface.
    const surface = await (
      await frame.frameElement()
    ).evaluate((el) => el.closest<HTMLElement>("[data-surface]")?.dataset.surface ?? "viewer");
    // Resolve through Vite, just as the app does, including its bundled JSON imports.
    const themes = await pane.page.evaluate(async (surface) => {
      const { themeTokens, BUNDLED_THEMES } = await import("/src/themes/index.ts");
      const { kitRoles } = await import("/src/themes/kit.ts");
      const roles = kitRoles(surface as Surface);
      return (Object.keys(BUNDLED_THEMES) as (keyof typeof BUNDLED_THEMES)[]).flatMap((id) =>
        (["light", "dark"] as const).map((scheme) => ({
          id,
          scheme,
          bodies: ["bg", "surface"].map((background) =>
            [background, "fg"].map((role) => themeTokens(id, scheme)[`--theme-${roles[role]}`]),
          ),
        })),
      );
    }, surface);
    const compiled = (id: string, scheme: "light" | "dark") =>
      themes.find((t) => t.id === id && t.scheme === scheme)!;
    const follows = async (theme: (typeof themes)[number], label: string) => {
      const expected = await Promise.all(theme.bodies.map((colors) => hex(frame, colors)));
      let actual: string[] = [];
      try {
        await expect
          .poll(
            async () => {
              actual = await body();
              return expected.some((colors) => colors.every((color, i) => color === actual[i]));
            },
            { timeout: 2_000 },
          )
          .toBe(true);
      } catch {
        fail(
          "theme",
          `${label} body background/text did not change to its ${surface} palette: got ${actual.join("/")}, expected ${expected.map((colors) => colors.join("/")).join(" or ")}`,
        );
      }
      await wait(frame, SETTLE_MS);
      return actual;
    };
    const noReload = async (label: string) => {
      if ((await pane.probe()).boot !== boot) fail("theme", `${label} reloaded`);
    };
    const contrastFailures: string[] = [];
    for (const scheme of ["light", "dark"] as const) {
      await pane.setTheme(compiled("codesense", scheme));
      await wait(frame, SETTLE_MS);
      for (const id of [...new Set(themes.map((theme) => theme.id))]) {
        const label = `${id}/${scheme}`;
        const theme = compiled(id, scheme);
        await pane.setTheme(theme);
        const target = await follows(theme, label);
        await noReload(`${label} live change`);
        // Change only the palette, keeping the scheme constant on both paths.
        const other = compiled(id === "nord" ? "codesense" : "nord", scheme);
        await pane.setTheme(other);
        await follows(other, `${other.id}/${scheme} before hiding`);
        const heard = (await pane.probe()).messages.length;
        await pane.show(false);
        try {
          await expect
            .poll(async () =>
              (await pane.probe()).messages.slice(heard).includes("codesense:visibility:false"),
            )
            .toBe(true);
        } catch {
          fail("theme", `${label} body was never hidden before the theme change`);
        }
        await pane.setTheme(theme);
        await pane.show(true);
        try {
          await expect.poll(body, { timeout: 2_000 }).toEqual(target);
        } catch {
          fail("theme", `${label} body background/text did not follow hidden catch-up`);
        }
        await noReload(`${label} hidden catch-up`);
        await wait(frame, SETTLE_MS);
        contrastFailures.push(...(await checkPaneContrast(frame, label)));
      }
    }
    if (contrastFailures.length) fail("theme", contrastFailures.join("; "));
  },
};

export async function checkPaneContrast(frame: Frame, label: string) {
  const samples = await frame.evaluate(() => {
    const result: Array<{
      element: string;
      kind: "text" | "nonText";
      ink: string;
      backgrounds: string[];
    }> = [];
    for (const el of [
      document.body,
      ...document.body.querySelectorAll<HTMLElement | SVGElement>("*"),
    ]) {
      const rect = el.getBoundingClientRect();
      const s = getComputedStyle(el);
      if (
        !rect.width ||
        !rect.height ||
        s.visibility !== "visible" ||
        el.closest(
          '[disabled], [aria-disabled="true"], [inert], defs, mask, clipPath, pattern, symbol',
        )
      )
        continue;
      const backgrounds: string[] = [];
      let opacityGroup = false;
      for (let ancestor: Element | null = el; ancestor; ancestor = ancestor.parentElement) {
        const style = getComputedStyle(ancestor);
        backgrounds.unshift(style.backgroundColor);
        // Group opacity changes both ink and background; skip instead of approximating it.
        if (Number(style.opacity) !== 1) opacityGroup = true;
      }
      if (opacityGroup) continue;
      const element = `${el.tagName.toLowerCase()}${el.id ? "#" + el.id : el.classList.length ? "." + [...el.classList].join(".") : ""}`;
      const add = (kind: "text" | "nonText", ink: string, ground = backgrounds) =>
        result.push({ element, kind, ink, backgrounds: ground });
      if (
        [...el.childNodes].some((n) => n.nodeType === Node.TEXT_NODE && n.textContent?.trim()) ||
        el.matches("input, textarea, select")
      )
        add("text", s.color);
      if (el.matches('button, input, textarea, select, [role="button"], a[href]')) {
        for (const side of ["Top", "Right", "Bottom", "Left"]) {
          if (
            parseFloat(s.getPropertyValue(`border-${side.toLowerCase()}-width`)) &&
            !["none", "hidden"].includes(s.getPropertyValue(`border-${side.toLowerCase()}-style`))
          )
            add(
              "nonText",
              s.getPropertyValue(`border-${side.toLowerCase()}-color`),
              backgrounds.slice(0, -1),
            );
        }
      }
      if (
        el instanceof SVGElement &&
        el.matches("path, rect, circle, ellipse, line, polyline, polygon")
      ) {
        for (const ink of [s.fill, s.stroke]) if (ink !== "none") add("nonText", ink);
      }
    }
    return result;
  });
  const colors = [...new Set(samples.flatMap((s) => [s.ink, ...s.backgrounds]))];
  const resolved = new Map(colors.map((c, i) => [c, i]));
  const values = await hex(frame, colors);
  const color = (c: string) => values[resolved.get(c)!];
  const failures: string[] = [];
  for (const sample of samples) {
    const ink = color(sample.ink);
    if (ink.endsWith("00")) continue;
    const bg = sample.backgrounds.reduce((under, c) => compositeOver(color(c), under), "#ffffff");
    const paintedInk = compositeOver(ink, bg);
    const ratio = contrastRatio(paintedInk, bg);
    const threshold = CONTRAST_THRESHOLDS[sample.kind];
    if (ratio < threshold)
      failures.push(
        `${label} ${sample.element} ${sample.kind}: ${ratio.toFixed(2)}:1 < ${threshold}:1 (ink ${paintedInk}, background ${bg})`,
      );
  }
  return [...new Set(failures)];
}

export const CHECKS: readonly Check[] = [
  focusInCheck,
  escCheck,
  flushCheck,
  budgetCheck,
  teamSwitchCheck,
  maximizeCheck,
  reparentCheck,
  axeCheck,
  themeCheck,
];

/** Mount `path` with a check's fixtures and run it. */
export async function runCheck(
  mountPane: (path: string, fixtures?: PaneFixtures) => Promise<MountedPane>,
  path: string,
  check: Check,
  slotId: PaneFixtures["slotId"] = "right-pane",
) {
  await check.run(
    await mountPane(path, {
      ...DEFAULT_FIXTURES,
      records: slotId === "right-pane" ? DEFAULT_FIXTURES.records : undefined,
      ...check.fixtures,
      slotId,
    }),
  );
}

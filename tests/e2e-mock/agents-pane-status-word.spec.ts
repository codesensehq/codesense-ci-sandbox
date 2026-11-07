// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { expect, test, type Locator, type Page } from "@playwright/test";

/**
 * SV-ENG-115 (r2 L8, r3 M): line one of a live row gives way in one order at every Agents pane
 * width, hovered (so its actions show) or not: a launch prompt first, then the name down to a
 * few characters, and never the status word. Measured in real layout.
 */

const WIDTHS = [236, 268, 330, 400];
/** The team world's repro agent, whose session runs no subagents. */
const PLAIN_HANDLE = "BUG-142#repro";
const PLAIN_SESSION = "sess-login-repro";
const LONG_NAME = "a-very-long-agent-name-that-will-not-fit-beside-its-status-word";
const LONG_PROMPT =
  "Own the navigation story end to end: read the spec, fix every route, and report back";

type Case = {
  label: string;
  /** The fields that put the row in this state. */
  fields: Record<string, unknown>;
  word: string | null;
  prompt: boolean;
  /** Too much for the narrow widths: the prompt must have given all its room before the name
   *  gives way, and the name must keep its floor. */
  crowded?: boolean;
};

/** The widths at which a crowded row runs out of room for the prompt. */
const CROWDED_WIDTHS = [236, 268];

const CASES: Case[] = [
  {
    label: "typed by hand, long name",
    fields: {
      kind: "hand-started",
      inboxWaiter: false,
      effectiveStatus: "unknown",
      startedWith: null,
      rowNames: { primary: LONG_NAME, secondary: null, full: LONG_NAME },
    },
    word: " · Activity unknown",
    prompt: false,
  },
  {
    label: "working with a launch prompt",
    fields: {
      kind: "dashboard-launched",
      inboxWaiter: true,
      effectiveStatus: "thinking",
      startedWith: LONG_PROMPT,
      rowNames: { primary: "builder", secondary: null, full: "builder" },
    },
    word: " · Thinking",
    prompt: true,
  },
  {
    label: "a prompt with no status word",
    fields: {
      kind: "dashboard-launched",
      inboxWaiter: true,
      effectiveStatus: "unknown",
      startedWith: LONG_PROMPT,
      rowNames: { primary: "builder", secondary: null, full: "builder" },
    },
    word: null,
    prompt: true,
  },
  {
    label: "a long name working with a long prompt",
    fields: {
      kind: "dashboard-launched",
      inboxWaiter: true,
      effectiveStatus: "thinking",
      startedWith: LONG_PROMPT,
      rowNames: { primary: LONG_NAME, secondary: null, full: LONG_NAME },
    },
    word: " · Thinking",
    prompt: true,
    crowded: true,
  },
];

async function setRow(page: Page, handle: string, fields: Record<string, unknown>) {
  await page.evaluate(
    async ({ handle, fields }) => {
      const { useAgentListProjectionStore } = await new Function(
        "return import('/src/features/viewer/lib/agent-list-projection.ts')",
      )();
      useAgentListProjectionStore.setState((s: { sessions: Record<string, unknown>[] }) => ({
        sessions: s.sessions.map((r) => (r.handle === handle ? { ...r, ...fields } : r)),
      }));
    },
    { handle, fields },
  );
}

async function measure(row: Locator) {
  return row.evaluate((rowEl) => {
    const q = (s: string) => rowEl.querySelector<HTMLElement>(s);
    const title = q(".ag-title-line")!;
    const name = q(".ag-title-name")!;
    const word = q(".ag-title-word");
    const more = q(".ag-title-more");
    const actions = q(".ag-row-actions");
    const t = title.getBoundingClientRect();
    const a = actions?.getBoundingClientRect();
    const shown = actions ? getComputedStyle(actions).opacity !== "0" : false;
    const clipped = (el: HTMLElement | null) => !!el && el.scrollWidth > el.clientWidth + 0.5;
    const w = word?.getBoundingClientRect();
    // The actions cover the word only when they share its line, not a hairline of their 20px
    // hit box (on a narrow row they sit over line two).
    const overlapY = a && w ? Math.min(a.bottom, w.bottom) - Math.max(a.top, w.top) : 0;
    const onWordLine = !!a && shown && overlapY > 3;
    return {
      wordText: word?.textContent ?? null,
      wordClipped: clipped(word),
      wordInsideTitle: !w || w.right <= t.right + 0.5,
      wordClearOfActions: !w || !onWordLine || w.right <= a!.left + 0.5,
      nameClipped: clipped(name),
      nameWidth: name.getBoundingClientRect().width,
      // The name's floor (3ch) in pixels.
      nameFloor: parseFloat(getComputedStyle(name).minWidth),
      promptClipped: clipped(more),
      promptWidth: more ? more.clientWidth : 0,
      promptInsideTitle: !more || more.getBoundingClientRect().right <= t.right + 0.5,
    };
  });
}

test("line one gives way prompt first, then name, never the status word, at every width", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");
  await page.waitForFunction(() => !!window.__stores?.useViewerStore);
  await expect(page.locator(".ag-row:not(.ag-ghost)").first()).toBeVisible();
  // A row pinned by data, not by what the pane has drawn so far: the repro agent's session runs
  // no subagents in the team world's seed (a working row with subagents draws the subagent
  // title, not this plain line), and it asks nothing.
  const seed = readFileSync("src/app/mock/handlers/agents-team.ts", "utf8");
  const withSubagents = seed.slice(
    seed.indexOf("export const AGENT_TEAM_SUBAGENTS"),
    seed.indexOf("/** Agents the host observes with no subagents running"),
  );
  expect(withSubagents, "the seed lists the sessions that run subagents").toContain(
    "sess-login-lead",
  );
  expect(withSubagents, "the repro session runs no subagents").not.toContain(PLAIN_SESSION);
  const handle = PLAIN_HANDLE;
  await expect(page.locator(`.ag-row[data-agent-handle="${handle}"]`).first()).toBeVisible();
  const row = page.locator(`.ag-row[data-agent-handle="${handle}"]`).first();

  for (const c of CASES) {
    await setRow(page, handle, c.fields);
    if (c.word) await expect(row.locator(".ag-title-word")).toHaveText(c.word);
    else await expect(row.locator(".ag-title-word")).toHaveCount(0);
    // Attached, not visible: on a crowded row the prompt is rightly 0px wide.
    if (c.prompt) await expect(row.locator(".ag-title-more")).toBeAttached();

    for (const width of WIDTHS) {
      await page.evaluate(
        (w) => window.__stores!.useViewerStore.getState().setWorkspacePanelWidth(w),
        width,
      );
      await page.waitForTimeout(150);
      for (const hovered of [false, true]) {
        if (hovered) await row.hover();
        else await page.mouse.move(1300, 800);
        await page.waitForTimeout(200);
        const m = await measure(row);
        const where = `${c.label} at ${width}px${hovered ? " hovered" : ""}`;
        if (c.word) {
          expect(m.wordText, `word changed: ${where}`).toBe(c.word);
          expect(m.wordClipped, `word clipped: ${where}`).toBe(false);
          expect(m.wordInsideTitle, `word overflows its line: ${where}`).toBe(true);
          expect(m.wordClearOfActions, `word under the actions: ${where}`).toBe(true);
        }
        if (c.prompt) {
          expect(m.promptInsideTitle, `prompt overflows its line: ${where}`).toBe(true);
          // A prompt this long never fits whole: it shows an ellipsis, never a hard cut.
          expect(m.promptClipped, `prompt not ellipsized: ${where}`).toBe(true);
          // The name gives way only once the prompt has given all its room.
          if (m.nameClipped)
            expect(m.promptWidth, `name cut before prompt: ${where}`).toBeLessThanOrEqual(1);
          if (c.crowded && CROWDED_WIDTHS.includes(width)) {
            // Out of room: the prompt has given all of its, the name gives way down to its floor
            // and no further, and the word stays whole (asserted above).
            expect(
              m.promptWidth,
              `prompt kept room on a crowded row: ${where}`,
            ).toBeLessThanOrEqual(1);
            expect(m.nameClipped, `name not giving way on a crowded row: ${where}`).toBe(true);
            expect(m.nameWidth, `name below its floor: ${where}`).toBeGreaterThanOrEqual(
              m.nameFloor - 0.5,
            );
          }
        } else {
          // No prompt: the long name is what gives way.
          expect(m.nameClipped, `name gives way first: ${where}`).toBe(true);
        }
      }
    }
  }
});

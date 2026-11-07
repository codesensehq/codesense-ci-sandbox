// SPDX-License-Identifier: MIT
import { test, expect } from "../pane-harness/harness";
import type { Frame } from "@playwright/test";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";

// One matrix covers both story and conversation readers.
const fixture =
  process.env.STORY_TRAIL_TEST_FIXTURE ?? writeTrailFixture("/tmp/cde19-state-matrix/trail.html");
const controls: Record<string, string> = {
  sort: "[data-sort=cost]",
  group: "[data-group=task]",
  flat: "[data-flat]",
  tree: "[data-task=lead]",
  details: "[data-open=lead]",
  rate: "[data-session=lead] [data-rate]",
  taskRate: "[data-task=lead] [data-rate]",
  text: "[name=modelPrefix]",
  date: "[name=effectiveFrom]",
  number: "[name=inputPerMtok]",
  save: "[type=submit]",
  cancel: "[data-cancel-rate]",
  retry: "[data-retry]",
  close: "[data-close]",
  link: "#drill-panel [data-panel-key=conversation]",
  panelRate: "[data-panel-rate]",
  panelTurn: '[data-panel-key="turn:38"]',
  panelRetry: "[data-panel-retry]",
  rail: '[data-jump="37"]',
  earlier: "[data-conv-action=earlier]",
  later: "[data-conv-action=later]",
  unhighlight: "[data-conv-action=unhighlight]",
  summary: "summary",
  live: "[data-conv-action=live]",
  back: "[data-conv-action=back]",
  convRetry: "[data-conv-action=retry]",
};
const panelKinds = new Set(["close", "link", "panelRate", "panelTurn", "panelRetry"]);
const conversationKinds = new Set([
  "rail",
  "earlier",
  "later",
  "unhighlight",
  "summary",
  "live",
  "back",
  "convRetry",
]);
const modes = [
  "unchanged",
  "changed",
  "pointer",
  "remove",
  "disabled",
  "busy",
  "hidden",
  "inert",
  "row-removed",
  "async-after-move",
  "reorder",
  "hash-back",
  "panel",
  "host",
  "pointer-blur",
  "native-focus",
  "state",
] as const;
const paint = (frame: Frame) =>
  frame.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
for (const [kind, selector] of Object.entries(controls))
  for (const mode of modes)
    test(`${kind} × ${mode}`, async ({ mountPane }) => {
      const pane = await mountPane(fixture, {
        sessions: [agentRow("lead", "Builder"), agentRow("second", "Other")],
      });
      const frame = pane.frame;
      if (conversationKinds.has(kind)) {
        await exerciseConversation(kind, selector, mode, pane);
        await pane.close();
        return;
      }
      await expect(frame.locator("[data-open=lead]")).toBeVisible();
      await pane.focus();
      await expect(frame.locator("[data-flat]")).toBeFocused();
      if (mode === "reorder") await frame.locator("[data-sort=agent]").click();
      await frame.evaluate(
        ({ reorder, kind }) => {
          const w = window as any,
            cs = w.codesense,
            read = cs.storyChanges;
          w.readCount = 0;
          cs.storyChanges = async () => {
            const data = await read();
            w.readCount++;
            if (reorder) {
              data.tasks.push({ ...data.tasks[1], taskId: "matrix-peer", goal: "Other task" });
              if (w.matrixReorder) data.tasks.reverse();
            }
            for (const row of data.sessions) {
              row.tasks = [data.tasks[1]];
              row.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
              row.models[0].costUsd = null;
            }
            return data;
          };
          cs.setModelRate = async () => 0;
          cs.openFile = async () => ({});
          w.panelReads = 0;
          const readSession = cs.sessionRead;
          cs.sessionRead = async (args) => {
            if (kind === "panelRetry") {
              w.panelReads++;
              throw new Error("Read failed");
            }
            const data = await readSession(args);
            w.panelReads++;
            if (kind === "panelRate") {
              data.cost = { state: "absent", totalUsd: null, source: null, reason: "unpriced" };
              for (const model of data.models) {
                model.costUsd = null;
                model.costSource = null;
              }
            }
            return data;
          };
        },
        { reorder: mode === "reorder", kind },
      );
      await pane.upsert("sessions", agentRow("lead", "Builder"));
      await expect(frame.locator("[data-session=lead] [data-rate]")).toBeVisible();
      if (["text", "date", "number", "save", "cancel", "retry"].includes(kind)) {
        await frame.locator("[data-session=lead] [data-rate]").click();
        await frame.locator("[name=inputPerMtok]").fill("1");
        await frame.locator("[name=outputPerMtok]").fill("2");
        if (kind === "retry") await frame.locator("[type=submit]").click();
      }
      if (panelKinds.has(kind)) {
        await frame.locator("[data-open=lead]").click();
        await expect(frame.locator("#drill-panel [data-loading]")).toBeHidden();
      }
      const target = frame.locator(selector);
      await expect(target).toHaveCount(1);
      await expect(target).toBeVisible();
      await target.focus();
      await target.evaluate((el) => {
        const w = window as any;
        w.matrixControl = el;
        w.matrixRowOwner = (node: Element) => {
          const id = node.closest<HTMLElement>("[data-row-id]")?.dataset.rowId;
          return (
            node.closest("[data-row-focus]") ??
            (id ? document.querySelector('[data-row-focus="' + CSS.escape(id) + '"]') : null) ??
            node.closest("section") ??
            node.parentElement
          );
        };
        w.matrixRow = w.matrixRowOwner(el);
        w.matrixClicks = 0;
        w.matrixEvents = [];
        for (const type of ["pointerdown", "pointerup", "click"])
          document.addEventListener(
            type,
            (event) =>
              w.matrixEvents.push({
                type,
                target: (event.target as HTMLElement).outerHTML.slice(0, 160),
              }),
            true,
          );
        document.addEventListener(
          "click",
          (event) => {
            if (el === event.target || el.contains(event.target as Node)) w.matrixClicks++;
          },
          true,
        );
      });
      const refresh = async (changed = false) => {
        const panelBefore = await frame.evaluate(() => (window as any).panelReads);
        const before = await frame.evaluate(() => (window as any).readCount);
        await pane.upsert(
          "sessions",
          agentRow(
            "lead",
            changed ? (mode === "reorder" ? "ZZZ Builder" : "Builder changed") : "Builder",
          ),
        );
        await expect
          .poll(() => frame.evaluate(() => (window as any).readCount))
          .toBeGreaterThan(before);
        if (panelKinds.has(kind))
          await expect
            .poll(() => frame.evaluate(() => (window as any).panelReads))
            .toBeGreaterThanOrEqual(panelBefore + (kind === "panelRetry" ? 1 : 2));
        await paint(frame);
      };
      if (mode === "pointer") {
        const hit = kind === "tree" ? target.locator(":scope > [data-row-label] .tg") : target;
        await hit.scrollIntoViewIfNeeded();
        const box = (await hit.boundingBox())!;
        await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
        await pane.page.mouse.down();
        await refresh(true);
        const after = await target.boundingBox();
        await pane.page.mouse.up();
        await paint(frame);
        const events = await frame.evaluate(() => (window as any).matrixEvents);
        await expect
          .poll(() => frame.evaluate(() => (window as any).matrixClicks), {
            message: JSON.stringify({ box, after, events }),
          })
          .toBeGreaterThan(0);
        await expect
          .poll(() =>
            frame.evaluate(() => {
              const active = document.activeElement as HTMLElement;
              const anchor = active.querySelector(":scope > [data-row-label]") ?? active;
              const rect = anchor.getBoundingClientRect();
              return rect.top >= -1 && rect.bottom <= innerHeight + 1;
            }),
          )
          .toBe(true);
      } else if (["remove", "disabled", "busy", "hidden", "inert"].includes(mode)) {
        await target.evaluate((el, mode) => {
          if (mode === "remove") el.remove();
          if (mode === "disabled") {
            if ("disabled" in el) (el as HTMLButtonElement).disabled = true;
            else el.setAttribute("aria-disabled", "true");
          }
          if (mode === "busy") el.setAttribute("aria-disabled", "true");
          if (mode === "hidden") (el as HTMLElement).hidden = true;
          if (mode === "inert") el.setAttribute("inert", "");
        }, mode);
        if (mode === "busy") {
          const hit = kind === "tree" ? target.locator(":scope > [data-row-label] .tg") : target;
          await hit.scrollIntoViewIfNeeded();
          const box = (await hit.boundingBox())!;
          await pane.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
        }
        await paint(frame);
      } else if (mode === "row-removed") {
        await frame.evaluate(() => {
          const cs = (window as any).codesense,
            read = cs.storyChanges;
          cs.storyChanges = async () => {
            const data = await read();
            data.sessions = data.sessions.filter((row) => row.session?.sessionId !== "lead");
            data.tasks = data.tasks.filter((task) => task.taskId !== "lead");
            return data;
          };
        });
        await refresh();
      } else if (mode === "async-after-move") {
        if (kind === "save") {
          await frame.evaluate(() => {
            (window as any).codesense.setModelRate = () =>
              new Promise((resolve) => {
                (window as any).finishMatrixRead = () => resolve(1);
              });
          });
          await target.press("Enter");
        } else {
          await frame.evaluate((panel) => {
            const w = window as any,
              cs = w.codesense,
              method = panel ? "sessionRead" : "storyChanges",
              read = cs[method];
            w.matrixHeldPanelBefore = w.panelReads;
            cs[method] = (...args) => {
              cs[method] = read;
              return new Promise((resolve, reject) => {
                w.finishMatrixRead = () => read(...args).then(resolve, reject);
              });
            };
          }, panelKinds.has(kind));
          await pane.upsert("sessions", agentRow("lead", "Builder"));
        }
        await expect
          .poll(() => frame.evaluate(() => typeof (window as any).finishMatrixRead))
          .toBe("function");
        await pane.page.locator("#terminal").click();
        await frame.evaluate(() => (window as any).finishMatrixRead());
        if (panelKinds.has(kind))
          await expect
            .poll(() =>
              frame.evaluate(
                () => (window as any).panelReads - (window as any).matrixHeldPanelBefore,
              ),
            )
            .toBeGreaterThanOrEqual(kind === "panelRetry" ? 1 : 2);
        await paint(frame);
        await expect(pane.page.locator("#terminal")).toBeFocused();
        await pane.close();
        return;
      } else if (mode === "reorder") {
        // Passive ordering is held while a row is focused; explicit sort remains immediate.
        const before = await target.evaluate((el) => {
          const w = window as any,
            id = el.closest<HTMLElement>("[data-row-id]")?.dataset.rowId;
          const row =
            (el.matches("[data-task]") ? el : el.closest("[data-row-focus]")) ??
            (id ? document.querySelector('[data-row-focus="' + CSS.escape(id) + '"]') : null);
          w.matrixReorderContainer = row?.closest("[data-key]")?.parentElement;
          w.matrixReorder = true;
          return w.matrixReorderContainer
            ? [...w.matrixReorderContainer.children].map((node) => node.dataset.key)
            : null;
        });
        if (kind === "tree") expect(before).not.toBeNull();
        await refresh(true);
        if (before)
          expect(
            await frame.evaluate(() =>
              [...(window as any).matrixReorderContainer.children].map((node) => node.dataset.key),
            ),
          ).toEqual(before);
        await pane.page.locator("#terminal").click();
        await paint(frame);
        if (before)
          expect(
            await frame.evaluate(() =>
              [...(window as any).matrixReorderContainer.children].map((node) => node.dataset.key),
            ),
          ).not.toEqual(before);
      } else if (mode === "hash-back") {
        if (kind === "save") {
          await frame.evaluate(() => {
            (window as any).codesense.setModelRate = () =>
              new Promise((resolve) => {
                (window as any).completeAwaySave = () => resolve(1);
              });
          });
          await target.press("Enter");
        }
        await frame.evaluate(() => {
          location.hash = "#session/lead";
        });
        await expect(frame.locator(".conversation-head")).toBeVisible();
        if (kind === "save") {
          await frame.evaluate(() => (window as any).completeAwaySave());
          await paint(frame);
        }
        await frame.evaluate(() => {
          location.hash = "#story/SV-X";
        });
        await expect(frame.locator("h1")).toHaveText("A story");
        if (kind === "save") {
          await expect(frame.locator("#rate-form")).toHaveCount(0);
          await expect(frame.locator("#notice")).toContainText("Rate saved");
        }
      } else if (mode === "panel") {
        if (await frame.locator("#drill-panel").isVisible())
          await frame.locator("[data-close]").press("Escape");
        await frame.locator("[data-open=lead]").click();
        await expect(frame.locator("#drill-panel")).toBeVisible();
        await frame.locator("[data-close]").press("Escape");
        await expect(frame.locator("#drill-panel")).toBeHidden();
      } else if (mode === "host") {
        await pane.page.locator("#terminal").click();
        await refresh();
        await expect(pane.page.locator("#terminal")).toBeFocused();
        await pane.focus();
        await paint(frame);
      } else if (mode === "pointer-blur") {
        await (
          (await frame.locator("#drill-panel").isVisible())
            ? frame.locator("#drill-panel h2")
            : frame.locator(".intro")
        ).click();
        await refresh();
        expect(
          await frame.evaluate(() => (window as any).matrixControl === document.activeElement),
        ).toBe(false);
        await pane.close();
        return;
      } else if (mode === "state") {
        await (
          (await frame.locator("#drill-panel").isVisible())
            ? frame.locator("#drill-panel h2")
            : frame.locator(".intro")
        ).selectText();
        const selected = await frame.evaluate(() => getSelection()?.toString());
        const scroll = await frame.evaluate(() => {
          const el = document.querySelector<HTMLElement>("[data-scroll-key=sessions]")!;
          el.scrollLeft = 70;
          return el.scrollLeft;
        });
        await refresh();
        expect(await frame.evaluate(() => getSelection()?.toString())).toBe(selected);
        expect(
          await frame.locator("[data-scroll-key=sessions]").evaluate((el) => el.scrollLeft),
        ).toBe(scroll);
      } else {
        if (mode === "native-focus" && kind === "date") await target.click();
        await refresh(mode === "changed");
      }
      if (["unchanged", "changed", "state", "native-focus", "panel", "hash-back"].includes(mode)) {
        if (!(kind === "save" && mode === "hash-back"))
          expect(
            await target.evaluate(
              (el) =>
                el === (window as any).matrixControl &&
                (window as any).matrixRowOwner(el) === (window as any).matrixRow,
            ),
          ).toBe(true);
        expect(await frame.evaluate(() => (window as any).matrixRow.isConnected)).toBe(true);
      }
      if (mode !== "state" && mode !== "reorder")
        expect(await frame.evaluate(() => document.activeElement?.tagName)).not.toBe("BODY");
      await pane.close();
    });

async function exerciseConversation(kind: string, selector: string, mode: string, pane: any) {
  const frame: Frame = pane.frame;
  await expect(frame.locator("[data-open=lead]")).toBeVisible();
  await pane.focus();
  await expect(frame.locator("[data-flat]")).toBeFocused();
  await frame.evaluate(() => {
    const w = window as any,
      cs = w.codesense,
      read = cs.sessionRead;
    w.conversationReads = 0;
    cs.openFile = async () => ({});
    cs.gotoAgent = async () => ({});
    const sessions = cs.sessions;
    cs.sessions = async () => [
      ...(await sessions()).map((row) => ({ ...row, sessionId: row.id })),
      { id: "live-lead", sessionId: "lead", state: "running" },
    ];
    cs.sessionRead = async (args) => {
      w.conversationReads++;
      if (w.matrixError) throw new Error("Read failed");
      const data = await read({ ...args, turn: 20, cursor: undefined, limit: 20 });
      data.session.agent = w.matrixChanged ? "Builder changed" : "Builder";
      data.conversation.nextCursor = 40;
      const turn = data.conversation.turns.find((turn) => turn.index === 37);
      turn.tool_calls[0].native_id = "matrix-call";
      turn.tool_calls[0].result = "Recorded result\n".repeat(80);
      if (w.matrixRemoved)
        data.conversation.turns = data.conversation.turns.filter((turn) => turn.index !== 37);
      if (w.matrixReorder) data.conversation.turns.reverse();
      return data;
    };
  });
  await frame.evaluate(() => {
    location.hash = "#session/lead/turn/37";
  });
  await expect(frame.locator("#turn-37")).toBeVisible();
  if (kind === "convRetry") {
    await frame.evaluate(() => {
      (window as any).matrixError = true;
    });
    await pane.upsert("sessions", agentRow("lead", "Builder"));
    await expect(frame.locator(selector)).toBeVisible();
  }
  const target = frame.locator(selector);
  await target.focus();
  await target.evaluate((el) => {
    const w = window as any;
    w.matrixControl = el;
    w.matrixRow = el.closest("[data-row-focus]") ?? el.closest("[data-key]") ?? el.parentElement;
    w.matrixClicks = 0;
    el.addEventListener("click", () => w.matrixClicks++);
  });
  const refresh = async (changed = false) => {
    await frame.evaluate((changed) => {
      (window as any).matrixChanged = changed;
    }, changed);
    const before = await frame.evaluate(() => (window as any).conversationReads);
    await pane.upsert("sessions", agentRow("lead", changed ? "Builder changed" : "Builder"));
    await expect
      .poll(() => frame.evaluate(() => (window as any).conversationReads))
      .toBeGreaterThan(before);
    await paint(frame);
  };
  if (mode === "pointer") {
    await target.scrollIntoViewIfNeeded();
    const box = (await target.boundingBox())!;
    await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await pane.page.mouse.down();
    await refresh(true);
    await pane.page.mouse.up();
    await paint(frame);
    expect(await frame.evaluate(() => (window as any).matrixClicks)).toBeGreaterThan(0);
  } else if (["remove", "disabled", "busy", "hidden", "inert"].includes(mode)) {
    await target.evaluate((el, mode) => {
      if (mode === "remove") el.remove();
      if (mode === "disabled") {
        if ("disabled" in el) (el as HTMLButtonElement).disabled = true;
        else el.setAttribute("aria-disabled", "true");
      }
      if (mode === "busy") el.setAttribute("aria-disabled", "true");
      if (mode === "hidden") (el as HTMLElement).hidden = true;
      if (mode === "inert") el.setAttribute("inert", "");
    }, mode);
    if (mode === "busy") {
      await target.scrollIntoViewIfNeeded();
      const box = (await target.boundingBox())!;
      await pane.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    }
    await paint(frame);
  } else if (mode === "async-after-move") {
    await frame.evaluate(() => {
      const w = window as any,
        cs = w.codesense,
        read = cs.sessionRead;
      cs.sessionRead = (args) =>
        new Promise((resolve, reject) => {
          w.finishMatrixRead = () => read(args).then(resolve, reject);
        });
    });
    await pane.upsert("sessions", agentRow("lead", "Builder"));
    await expect
      .poll(() => frame.evaluate(() => typeof (window as any).finishMatrixRead))
      .toBe("function");
    await pane.page.locator("#terminal").click();
    await frame.evaluate(() => (window as any).finishMatrixRead());
    await paint(frame);
    await expect(pane.page.locator("#terminal")).toBeFocused();
    return;
  } else if (mode === "row-removed") {
    await frame.evaluate(() => {
      (window as any).matrixRemoved = true;
    });
    await refresh();
  } else if (mode === "reorder") {
    const before = await target.evaluate((el) => {
      const w = window as any;
      const row = el.closest("[data-row-focus]") ?? el.closest("[data-key]");
      w.matrixList = row?.parentElement;
      w.matrixReorder = true;
      return w.matrixList ? [...w.matrixList.children].map((node) => node.dataset.key) : null;
    });
    await refresh(true);
    if (before)
      expect(
        await frame.evaluate(() =>
          [...(window as any).matrixList.children].map((node) => node.dataset.key),
        ),
      ).toEqual(before);
    await pane.page.locator("#terminal").click();
    await paint(frame);
    if (before && before.length > 1)
      expect(
        await frame.evaluate(() =>
          [...(window as any).matrixList.children].map((node) => node.dataset.key),
        ),
      ).not.toEqual(before);
  } else if (mode === "hash-back" || mode === "panel") {
    await frame.evaluate(() => {
      location.hash = "#story/SV-X";
    });
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    if (mode === "panel") {
      await frame.locator("[data-open=lead]").click();
      await frame.locator("[data-close]").press("Escape");
    }
    await frame.evaluate(() => {
      location.hash = "#session/lead/turn/37";
    });
    await expect(frame.locator(".conversation-head")).toBeVisible();
    await paint(frame);
  } else if (mode === "host") {
    await pane.page.locator("#terminal").click();
    await refresh();
    await expect(pane.page.locator("#terminal")).toBeFocused();
    await pane.focus();
    await paint(frame);
  } else if (mode === "pointer-blur") {
    await frame.locator("#turn-37 .turn-text").click();
    await refresh();
    expect(
      await frame.evaluate(() => (window as any).matrixControl === document.activeElement),
    ).toBe(false);
    return;
  } else if (mode === "state") {
    await frame.locator("summary").click();
    await frame.locator("#turn-37 .turn-text").selectText();
    const selected = await frame.evaluate(() => getSelection()?.toString());
    const positions = await frame.evaluate(() => {
      const nodes = [".thread", ".turn-rail", ".tool-output:not([hidden])"].map(
        (selector) => document.querySelector<HTMLElement>(selector)!,
      );
      nodes.forEach((node) => {
        node.scrollTop = 40;
      });
      return nodes.map((node) => node.scrollTop);
    });
    await refresh();
    expect(await frame.evaluate(() => getSelection()?.toString())).toBe(selected);
    expect(
      await frame.evaluate(() =>
        [".thread", ".turn-rail", ".tool-output:not([hidden])"].map(
          (selector) => document.querySelector(selector)!.scrollTop,
        ),
      ),
    ).toEqual(positions);
    await expect(frame.locator("details")).toHaveAttribute("open", "");
  } else {
    await refresh(mode === "changed");
  }
  if (["unchanged", "changed", "state", "native-focus", "panel", "hash-back"].includes(mode)) {
    expect(await target.evaluate((el) => el === (window as any).matrixControl)).toBe(true);
    expect(await frame.evaluate(() => (window as any).matrixRow.isConnected)).toBe(true);
  }
  if (!["state", "reorder"].includes(mode))
    expect(await frame.evaluate(() => document.activeElement?.tagName)).not.toBe("BODY");
}

// Layout and topology changes are page-wide hazards during a held gesture.
for (const change of ["reparent", "growth"] as const)
  test(`held gesture × ${change}`, async ({ mountPane }) => {
    const pane = await mountPane(fixture, {
        sessions: [agentRow("lead", "Builder"), agentRow("second", "Reviewer")],
      }),
      frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await pane.focus();
    await expect(frame.locator("[data-flat]")).toBeFocused();
    await frame.evaluate((change) => {
      const w = window as any,
        cs = w.codesense,
        read = cs.storyChanges;
      w.gestureReads = 0;
      w.gestureChanged = false;
      cs.storyChanges = async () => {
        const data = await read();
        w.gestureReads++;
        data.tasks.push(
          { ...data.tasks[1], taskId: "other", goal: "Other task" },
          { ...data.tasks[1], taskId: "child", goal: "Child task", parentId: "lead" },
        );
        if (change === "reparent" && w.gestureChanged)
          data.tasks.find((task) => task.taskId === "lead").parentId = "other";
        for (const row of data.sessions) {
          row.tools = { used: [{ name: "Read", calls: 1 }], usedReason: null };
          if (row.session.sessionId === "second") {
            row.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
            row.models[0].costUsd = null;
          } else if (change === "growth" && w.gestureChanged)
            row.tools.used = [
              "Read",
              "Bash",
              "Edit",
              "Grep",
              "Glob",
              "Write",
              "Task",
              "Agent",
              "mcp__codesense__session_read",
              "mcp__codesense__task_update",
            ].map((name) => ({ name, calls: 20 }));
        }
        return data;
      };
    }, change);
    await pane.upsert("sessions", agentRow("lead", "Builder"));
    await expect(frame.locator("[data-task=other]")).toBeVisible();
    const target = frame.locator(
        change === "reparent" ? "[data-task=lead]" : "[data-session=second] [data-rate]",
      ),
      hit = change === "reparent" ? target.locator(":scope > [data-row-label] .tg") : target;
    await target.focus();
    await hit.scrollIntoViewIfNeeded();
    await target.evaluate((el) => {
      const w = window as any;
      w.heldNode = el;
      w.heldClicks = 0;
      el.addEventListener("click", () => w.heldClicks++);
      w.gestureChanged = true;
    });
    const box = (await hit.boundingBox())!,
      before = await frame.evaluate(() => (window as any).gestureReads);
    await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await pane.page.mouse.down();
    await pane.upsert("sessions", agentRow("lead", "Changed builder"));
    await expect
      .poll(() => frame.evaluate(() => (window as any).gestureReads))
      .toBeGreaterThan(before);
    await paint(frame);
    expect(
      await target.evaluate(
        (el) => el === (window as any).heldNode && (window as any).heldNode.isConnected,
      ),
    ).toBe(true);
    const after = (await hit.boundingBox())!;
    expect(after.y).toBe(box.y);
    await pane.page.mouse.up();
    await paint(frame);
    expect(await frame.evaluate(() => (window as any).heldClicks)).toBe(1);
    if (change === "growth") await expect(frame.locator("#rate-form")).toBeVisible();
    else {
      await expect(frame.locator("[data-task=lead]")).toHaveAttribute("aria-expanded", "false");
      await expect(frame.locator("[data-task=lead]")).toHaveCount(1);
      await expect(frame.locator("[data-task=other] [data-task=lead]")).toHaveCount(1);
    }
    await pane.close();
  });

for (const kind of ["context-menu", "drag-away"] as const)
  test(`gesture release × ${kind}`, async ({ mountPane }) => {
    const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
      frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await pane.focus();
    await expect(frame.locator("[data-flat]")).toBeFocused();
    const target = frame.locator("[data-open=lead]");
    await target.scrollIntoViewIfNeeded();
    const box = (await target.boundingBox())!;
    if (kind === "context-menu") {
      await frame.locator(".intro").click({ button: "right" });
      await expect(pane.page.getByRole("menu")).toBeVisible();
      await pane.page.keyboard.press("Escape");
      await expect(pane.page.getByRole("menu")).toBeHidden();
    } else {
      await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await pane.page.mouse.down();
      const host = (await pane.page.locator("#terminal").boundingBox())!;
      await pane.page.mouse.move(host.x + host.width / 2, host.y + host.height / 2, { steps: 8 });
      await pane.page.mouse.up();
    }
    await pane.upsert("sessions", agentRow("lead", "Changed builder"));
    await expect(frame.locator("[data-open=lead]")).toHaveText("Changed builder");
    if (kind === "drag-away") await expect(frame.locator("#drill-panel")).toBeHidden();
    await pane.close();
  });

for (const gesture of ["middle-button", "link-drag"] as const)
  test(`completed gesture × ${gesture}`, async ({ mountPane }) => {
    const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
      frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toHaveText("Builder");
    await pane.focus();
    await expect(frame.locator("[data-flat]")).toBeFocused();
    let target = frame.locator("[data-group=task]"),
      agentLabel = frame.locator("[data-open=lead]"),
      method = "storyChanges";
    if (gesture === "link-drag") {
      await frame.locator("[data-open=lead]").click();
      target = frame.locator("#drill-panel a");
      if (!(await target.count())) {
        await frame.evaluate(() => {
          location.hash = "#session/lead";
        });
        await expect(frame.locator("#turn-0")).toBeVisible();
        target = frame.locator(".turn-rail a").first();
        agentLabel = frame.locator(".conversation-head h1");
        method = "sessionRead";
      }
    }
    await frame.evaluate((method) => {
      const w = window as any,
        cs = w.codesense,
        read = cs[method];
      w.gestureReads = 0;
      w.completedEvents = [];
      cs[method] = async (...args) => {
        const data = await read(...args);
        w.gestureReads++;
        return data;
      };
      for (const type of ["pointerdown", "pointerup", "auxclick", "click", "dragstart", "dragend"])
        document.addEventListener(type, () => w.completedEvents.push(type), true);
    }, method);
    await target.scrollIntoViewIfNeeded();
    const box = (await target.boundingBox())!;
    if (gesture === "middle-button")
      await pane.page.mouse.click(box.x + box.width / 2, box.y + box.height / 2, {
        button: "middle",
      });
    else {
      await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await pane.page.mouse.down();
      await pane.page.mouse.move(box.x + box.width / 2 - 60, box.y + box.height / 2 + 90, {
        steps: 15,
      });
      await pane.page.mouse.up();
    }
    const events = await frame.evaluate(() => (window as any).completedEvents);
    expect(events).toContain(gesture === "middle-button" ? "auxclick" : "dragend");
    const reads = await frame.evaluate(() => (window as any).gestureReads);
    await pane.upsert("sessions", agentRow("lead", "After gesture"));
    await expect
      .poll(() => frame.evaluate(() => (window as any).gestureReads))
      .toBeGreaterThan(reads);
    await expect(agentLabel).toHaveText("After gesture");
    await pane.close();
  });

test("priced completion updates the cached story while its route is away", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-open=lead]")).toBeVisible();
  await pane.focus();
  await expect(frame.locator("[data-flat]")).toBeFocused();
  await frame.evaluate(() => {
    const w = window as any,
      read = w.codesense.storyChanges;
    w.holdPricing = false;
    w.pricingAccepted = false;
    w.pricedReads = [];
    w.codesense.storyChanges = async () => {
      const data = await read();
      if (!w.pricingAccepted) {
        for (const row of data.sessions) {
          row.cost = { state: "absent", totalUsd: null, reason: "unpriced" };
          row.models[0].costUsd = null;
        }
        return data;
      }
      if (!w.holdPricing) return data;
      return new Promise((resolve) => {
        w.pricedReads.push(() => resolve(data));
      });
    };
    w.codesense.setModelRate = async () => 0;
  });
  await pane.upsert("sessions", agentRow("lead", "Builder"));
  await frame.locator("[data-session=lead] [data-rate]").click();
  await frame.locator("[name=inputPerMtok]").fill("1");
  await frame.locator("[name=outputPerMtok]").fill("2");
  await frame.locator("[type=submit]").click();
  await expect(frame.locator("#notice")).toContainText("If acceptance is required");
  await frame.evaluate(() => {
    const w = window as any;
    w.holdPricing = true;
    w.pricingAccepted = true;
  });
  await pane.upsert("sessions", agentRow("lead", "Builder"));
  await expect.poll(() => frame.evaluate(() => (window as any).pricedReads.length)).toBe(1);
  await frame.locator("[data-open=lead]").click();
  const placeholder = await frame.locator("#drill-panel a").count();
  if (placeholder) {
    await frame.locator("#drill-panel a").click();
    await expect(frame.locator("h1")).toHaveText("Conversation");
  } else {
    await frame.evaluate(() => {
      location.hash = "#session/lead";
    });
    await expect(frame.locator(".conversation-head")).toBeVisible();
  }
  await frame.evaluate(() => (window as any).pricedReads[0]());
  await expect.poll(() => frame.evaluate(() => (window as any).pricedReads.length)).toBe(2);
  await expect(frame.locator(".conversation-head")).toBeVisible();
  await frame.evaluate(() => (window as any).pricedReads[1]());
  await paint(frame);
  expect(await frame.evaluate(() => (window as any).pricedReads.length)).toBe(2);
  if (placeholder) await frame.locator('main a[href="#story/SV-X"]').click();
  else
    await frame.evaluate(() => {
      location.hash = "#story/SV-X";
    });
  await expect(frame.locator("h1")).toHaveText("A story");
  await expect(frame.locator("[data-session=lead]")).toContainText("$2.00");
  await expect(frame.locator("#notice")).not.toContainText("If acceptance is required");
  await expect(frame.locator("[data-retry]")).toHaveCount(0);
  await pane.close();
});

test("returning before a turn read completes reveals the requested row", async ({ mountPane }) => {
  const pane = await mountPane(fixture, { height: 500, sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-open=lead]")).toBeVisible();
  await pane.focus();
  await expect(frame.locator("[data-flat]")).toBeFocused();
  await frame.evaluate(() => {
    location.hash = "#session/lead/turn/0";
  });
  await expect(frame.locator("#turn-0")).toBeVisible();
  const scroll = await frame.locator(".thread").evaluate((node) => {
    node.scrollTop = node.scrollHeight;
    return node.scrollTop;
  });
  expect(scroll).toBeGreaterThan(0);
  await frame.evaluate(() => {
    const w = window as any,
      cs = w.codesense,
      read = cs.sessionRead;
    let hold = true;
    cs.sessionRead = (args) => {
      if (hold && args.turn === 37) {
        hold = false;
        return new Promise((resolve, reject) => {
          w.finishTurnRead = () => read(args).then(resolve, reject);
        });
      }
      return read(args);
    };
    location.hash = "#session/lead/turn/37";
  });
  await expect
    .poll(() => frame.evaluate(() => typeof (window as any).finishTurnRead))
    .toBe("function");
  await frame.evaluate(() => {
    location.hash = "#story/SV-X";
  });
  await expect(frame.locator("[data-open=lead]")).toBeVisible();
  await frame.evaluate(() => {
    location.hash = "#session/lead/turn/37";
  });
  await expect(frame.locator(".conversation-head")).toBeVisible();
  await frame.evaluate(() => (window as any).finishTurnRead());
  await expect(frame.locator(".turn").first()).toHaveAttribute("id", "turn-20");
  await expect
    .poll(() =>
      frame.locator("#turn-37 .turn-head").evaluate((node) => {
        const row = node.getBoundingClientRect(),
          thread = node.closest(".thread")!.getBoundingClientRect();
        return row.top >= thread.top - 1 && row.bottom <= thread.bottom + 1;
      }),
    )
    .toBe(true);
  await pane.close();
});

for (const kind of ["later", "rail"] as const)
  test(`held ${kind} click survives a pending passive read`, async ({ mountPane }) => {
    const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
      frame = pane.frame;
    await expect(frame.locator("[data-open=lead]")).toBeVisible();
    await frame.evaluate(() => {
      location.hash = "#session/lead/turn/20";
    });
    await expect(frame.locator("#turn-20")).toBeVisible();
    await frame.evaluate((kind) => {
      const w = window as any,
        cs = w.codesense,
        read = cs.sessionRead;
      let passive = false,
        user = false;
      w.holdGestureRead = false;
      cs.sessionRead = (args) => {
        if (w.holdGestureRead && args.cursor === 20 && !passive) {
          passive = true;
          return new Promise((resolve, reject) => {
            w.finishPassiveGestureRead = () => read(args).then(resolve, reject);
          });
        }
        if ((kind === "later" ? args.cursor === 40 : args.turn === 21) && !user) {
          user = true;
          return new Promise((resolve, reject) => {
            w.finishUserGestureRead = () => read(args).then(resolve, reject);
          });
        }
        return read(args);
      };
    }, kind);
    const target = frame.locator(
      kind === "later" ? "[data-conv-action=later]" : '[data-jump="21"]',
    );
    await target.scrollIntoViewIfNeeded();
    const box = (await target.boundingBox())!;
    await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await expect(target).not.toHaveAttribute("aria-disabled", "true");
    await pane.page.mouse.down();
    await frame.evaluate(() => {
      (window as any).holdGestureRead = true;
    });
    await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
    await expect
      .poll(() => frame.evaluate(() => typeof (window as any).finishPassiveGestureRead))
      .toBe("function");
    await pane.page.mouse.up();
    await expect
      .poll(() => frame.evaluate(() => location.hash))
      .toBe(kind === "later" ? "#session/lead?from=40" : "#session/lead?from=20&turn=21");
    if (kind === "later") await expect(target).toHaveAttribute("aria-disabled", "true");
    else await expect(target).not.toHaveAttribute("aria-disabled", "true");
    await frame.evaluate(() => (window as any).finishPassiveGestureRead());
    if (kind === "later") {
      await expect
        .poll(() => frame.evaluate(() => typeof (window as any).finishUserGestureRead))
        .toBe("function");
      await expect(target).toHaveAttribute("aria-disabled", "true");
    }
    const other = frame.locator(kind === "later" ? '[data-jump="21"]' : "[data-conv-action=later]");
    await expect(other).not.toHaveAttribute("aria-disabled", "true");
    if (kind === "later") await frame.evaluate(() => (window as any).finishUserGestureRead());
    await expect(frame.locator(".turn").first()).toHaveAttribute(
      "id",
      kind === "later" ? "turn-40" : "turn-20",
    );
    if (kind === "rail") await expect(frame.locator("#turn-21")).toHaveClass(/highlight/);
    await pane.close();
  });

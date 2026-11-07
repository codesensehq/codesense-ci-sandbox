// SPDX-License-Identifier: MIT
import type { Page } from "@playwright/test";
import { expect, test } from "./harness";

const PANE = "tests/pane-harness/fixtures/sample-pane.html";
const paths = ["/Users/me/first file.txt", "/Users/me/second.png"];
async function emit(
  page: Page,
  phase: "enter" | "over" | "drop" | "leave",
  position: { x: number; y: number },
) {
  await page.evaluate(
    ({ phase, position, paths }) => {
      (window as any).emit(`tauri://drag-${phase}`, { position, paths });
    },
    { phase, position, paths },
  );
}
for (const slotId of ["right-pane", "dock"] as const) {
  test(`${slotId} receives native drag feedback and paths only on drop`, async ({ mountPane }) => {
    const pane = await mountPane(PANE, { slotId });
    await pane.frame.evaluate(() => {
      (window as any).fileDropMessages = [];
      addEventListener("message", (event) => {
        if (
          event.source === parent &&
          ["codesense:file-drag", "codesense:file-drop"].includes(event.data?.type)
        ) {
          (window as any).fileDropMessages.push(event.data);
        }
      });
    });
    const element = await pane.frame.frameElement();
    const box = (await element.boundingBox())!;
    const position = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
    const messages = () => pane.frame.evaluate(() => (window as any).fileDropMessages);
    await emit(pane.page, "enter", position);
    await emit(pane.page, "over", position);
    await expect.poll(messages).toEqual([
      { type: "codesense:file-drag", phase: "enter" },
      { type: "codesense:file-drag", phase: "over" },
    ]);
    await emit(pane.page, "over", { x: 0, y: 0 });
    await expect.poll(messages).toHaveLength(3);
    expect((await messages()).at(-1)).toEqual({ type: "codesense:file-drag", phase: "leave" });
    await emit(pane.page, "enter", position);
    await emit(pane.page, "drop", position);
    await expect.poll(messages).toHaveLength(6);
    expect((await messages()).slice(-2)).toEqual([
      { type: "codesense:file-drop", paths },
      { type: "codesense:file-drag", phase: "leave" },
    ]);
  });
}

test("a pane without files:read does not receive native paths", async ({ mountPane }) => {
  const pane = await mountPane(PANE, { effectiveGrant: ["sessions:read"] });
  const element = await pane.frame.frameElement();
  const box = (await element.boundingBox())!;
  const position = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  await emit(pane.page, "enter", position);
  await emit(pane.page, "drop", position);
  // A normal round trip ensures earlier host messages have reached the frame.
  await pane.flush();
  const probe = await pane.probe();
  expect(probe.messages.filter((type) => type.startsWith("codesense:file-"))).toEqual([]);
});

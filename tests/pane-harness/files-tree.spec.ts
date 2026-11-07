// SPDX-License-Identifier: MIT
import { watchRow, rowStability } from "../support/row-stability";
import { expect, test, type MountedPane } from "./harness";

const PANE = "src/features/shell/slots/builtin/files-tree.html";
const file = (name: string, extra = {}) => ({
  id: `/fixture/${name}`,
  path: `/fixture/${name}`,
  name,
  kind: "file",
  root: "/fixture",
  ...extra,
});
// Host commands such as Collapse all reach the pane as codesense:command messages.
async function command(pane: MountedPane, id: string) {
  await pane.page.locator("iframe").evaluate((el: HTMLIFrameElement, c: string) => {
    el.contentWindow!.postMessage({ type: "codesense:command", id: c }, "*");
  }, id);
}
const ui = (extra = {}) => ({ id: "ui:file-nav", scope: "team", expanded: [], ...extra });
const mount = { slotId: "file-nav" as const, width: 268, height: 452 };

test("5,000 files render only the window, with natural ordering, roving focus and live updates", async ({
  mountPane,
}) => {
  const rows = Array.from({ length: 5000 }, (_, i) => file(`File-${i}.md`));
  const pane = await mountPane(PANE, { ...mount, topics: { files: rows, "ui:file-nav": [ui()] } });
  const tree = pane.frame.getByRole("tree");
  await expect(pane.frame.getByRole("treeitem").first()).toContainText("File-0.md");
  expect(await pane.frame.getByRole("treeitem").count()).toBeLessThan(40);
  await expect(pane.frame.locator("[data-cs-virtual-list]")).toHaveCSS("height", "120000px");
  await pane.frame.getByRole("treeitem").first().focus();
  await pane.page.keyboard.press("End");
  await expect(pane.frame.locator('[data-path="/fixture/File-4999.md"]')).toBeFocused();
  expect(await pane.frame.getByRole("treeitem").count()).toBeLessThan(40);
  expect(await tree.locator('[tabindex="0"]').count()).toBe(1);
  await pane.upsert("files", file("File-4999.md", { gitState: "modified" }));
  await expect(pane.frame.locator('[data-path="/fixture/File-4999.md"]')).toHaveClass(
    /git-modified/,
  );
});

test("tree navigation, filter, inherited badge, drill and shared selection", async ({
  mountPane,
}) => {
  const folder = file("folder", { kind: "dir" });
  const child = file("folder/nested.md", {
    name: "nested.md",
    inheritedFrom: "parent",
    kind: "story",
    storyId: "F-2",
    title: "Nested work",
    status: "review",
  });
  const pane = await mountPane(PANE, {
    ...mount,
    topics: {
      files: [
        file("File-10.md"),
        folder,
        file("File-2.md"),
        child,
        file("zz-epic.md", { kind: "epic", storyId: "E-1", title: "Roadmap", storyCount: 3 }),
      ],
      "ui:file-nav": [ui()],
    },
  });
  const first = pane.frame.getByRole("treeitem").first();
  await expect(first).toContainText("folder");
  const epic = pane.frame.locator('[data-path="/fixture/zz-epic.md"]');
  await expect(epic.locator(".title")).toHaveCSS("font-weight", "500");
  await expect(epic.locator(".id")).toHaveCSS("font-weight", "400");
  await pane.frame.getByLabel("Filter files").focus();
  const fieldRing = await pane.frame
    .locator(".field")
    .evaluate((el) => getComputedStyle(el).outlineWidth);
  expect(parseFloat(fieldRing)).toBeGreaterThan(0);
  await first.focus();
  await expect(first).toHaveCSS("outline-width", fieldRing);
  await pane.page.keyboard.press("ArrowRight");
  await expect(pane.frame.getByLabel("Inherited from parent")).toBeVisible();
  const nested = pane.frame.locator('[data-path="/fixture/folder/nested.md"]');
  await expect(nested).toHaveAttribute("aria-level", "2");
  await expect(nested).not.toContainText("review");
  await expect(nested.locator(".badge")).toHaveAttribute("role", "img");
  await expect(nested.locator(".badge")).toHaveText("parent");
  await expect(nested.locator(".badge svg")).toHaveAttribute("width", "10");
  await expect(nested).not.toHaveAttribute("title");
  await pane.page.keyboard.press("ArrowRight");
  await expect(nested).toBeFocused();
  await pane.page.keyboard.press("ArrowLeft");
  await expect(first).toBeFocused();
  await pane.frame.getByLabel("Filter files").fill("nested");
  await expect(nested).toBeVisible();
  await expect(nested.locator("mark")).toHaveText("Nested");
  await expect(nested.locator("mark")).toHaveCSS("font-weight", "600");
  await pane.frame.getByLabel("Filter files").fill("F-2");
  await expect(nested.locator(".id mark")).toHaveText("F-2");
  await pane.frame
    .locator(".field")
    .getByRole("button", { name: "Clear filter", exact: true })
    .click();
  await expect(pane.frame.getByLabel("Filter files")).toHaveValue("");
  await pane.frame.getByLabel("Filter files").press("Escape");
  await expect(pane.frame.getByLabel("Filter files")).toHaveValue("");
  await pane.writeUi({ selected: child.path });
  await expect(nested).toHaveAttribute("aria-selected", "true");
  await first.hover();
  await first.getByRole("button", { name: "Drill into folder" }).click();
  await expect(pane.frame.getByRole("navigation", { name: "File breadcrumbs" })).toBeVisible();
  await expect(
    pane.frame.getByRole("navigation").getByRole("button", { name: "dev", exact: true }),
  ).toBeVisible();
  await expect(
    pane.frame.getByRole("button", { name: "Up one level" }).locator("svg"),
  ).toBeVisible();
  await pane.frame.getByRole("button", { name: "Up one level" }).click();
  await expect(pane.frame.getByRole("navigation")).toBeHidden();
});

test("preview, pin, row tags, direct keyboard dialogs, native filter menu and collapse command", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: {
      files: [file("a.md")],
      "ui:file-nav": [
        ui({
          sources: { stories: [{ id: "F-2", name: "F-2" }], agents: [{ id: "ori", name: "Ori" }] },
        }),
      ],
    },
  });
  const row = pane.frame.getByRole("treeitem");
  await expect(row).toHaveAttribute(
    "data-codesense-row",
    JSON.stringify({ kind: "file", path: "/fixture/a.md" }),
  );
  await row.click();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "openFile").length)
    .toBe(1);
  await row.dblclick();
  await expect
    .poll(async () =>
      (await pane.verbs()).some((v) => v.verb === "openFile" && (v.args as any).preview === false),
    )
    .toBe(true);
  await row.click({ modifiers: ["Meta"] });
  await expect
    .poll(async () => (await pane.verbs()).find((v) => v.verb === "sendPrompt"))
    .toEqual({
      verb: "sendPrompt",
      args: { handle: "active", text: "/fixture/a.md", submit: false },
    });
  await row.focus();
  await pane.page.keyboard.press("F2");
  await expect
    .poll(async () =>
      (await pane.verbs()).some(
        (v) => v.verb === "showMenu" && (v.args as any).builtinAction === "builtin:rename",
      ),
    )
    .toBe(true);
  await pane.frame.getByRole("button", { name: "View, show and arrange" }).click();
  await expect
    .poll(async () =>
      (await pane.verbs()).some(
        (v) =>
          v.verb === "showMenu" &&
          (v.args as any).type === "native" &&
          (v.args as any).nativeSurface === "nav-filter",
      ),
    )
    .toBe(true);
  const collapses = async () =>
    (await pane.verbs()).filter(
      (v) =>
        v.verb === "setUiState" &&
        Array.isArray((v.args as any).patch?.expanded) &&
        (v.args as any).patch.expanded.length === 0,
    ).length;
  const before = await collapses();
  await command(pane, "collapse-all");
  await expect.poll(collapses).toBe(before + 1);
});

test("live themes restyle the sidebar without reloading; empty and filter states", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: { files: [file("a.md", { gitState: "added" })], "ui:file-nav": [ui()] },
  });
  await expect(pane.frame.getByRole("treeitem")).toBeVisible();
  await pane.writeUi({ selected: "/fixture/a.md" });
  const selected = pane.frame.getByRole("treeitem");
  const selectedColor = () => selected.evaluate((el) => getComputedStyle(el).backgroundColor);
  const selectionBefore = await selectedColor();
  await expect(selected).toHaveCSS("font-weight", "500");
  expect(
    await selected.evaluate((el) => {
      const sample = document.createElement("span");
      sample.style.backgroundColor = "var(--cs-sidebar-selected)";
      el.append(sample);
      const matches =
        getComputedStyle(sample).backgroundColor === getComputedStyle(el).backgroundColor;
      sample.remove();
      return matches;
    }),
  ).toBe(true);
  const boot = (await pane.probe()).boot;
  const color = () =>
    pane.frame.locator("body").evaluate((el) => getComputedStyle(el).backgroundColor);
  const before = await color();
  await pane.setTheme({ id: "github", scheme: "light" });
  await expect.poll(color).not.toBe(before);
  await expect.poll(selectedColor).not.toBe(selectionBefore);
  expect((await pane.probe()).boot).toBe(boot);
  await pane.frame.getByLabel("Filter files").fill("no-match");
  await expect(pane.frame.getByText('Nothing matches "no-match".')).toBeVisible();
  await pane.frame.locator(".field").getByRole("button", { name: "Clear filter" }).click();
  await expect(pane.frame.getByRole("treeitem")).toBeVisible();
});

test("an in-frame drag moves into the folder through files.move", async ({ mountPane }) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: { files: [file("a.md"), file("target", { kind: "dir" })], "ui:file-nav": [ui()] },
  });
  await pane.frame
    .locator('[data-path="/fixture/a.md"]')
    .dragTo(pane.frame.locator('[data-path="/fixture/target"]'));
  await expect
    .poll(async () => (await pane.verbs()).find((v) => v.verb === "files.move"))
    .toEqual({ verb: "files.move", args: { from: "/fixture/a.md", to: "/fixture/target/a.md" } });
});

test("private UI scope survives partial updates and shows its empty recovery", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: { files: [], "ui:file-nav": [ui({ scope: "private" })] },
  });
  await expect(pane.frame.getByText("Nothing private in dev yet.")).toBeVisible();
  await expect(pane.frame.locator("#scope-chip")).toContainText("Private");
  await pane.writeUi({ selected: "/fixture/private.md" });
  await expect(pane.frame.getByText("Nothing private in dev yet.")).toBeVisible();
  await expect(pane.frame.locator("#scope-chip")).toContainText("Private");
  await pane.frame.locator("#empty").getByRole("button", { name: "Show team files" }).click();
  await expect(pane.frame.locator("#scope-chip")).toBeHidden();
});

test("the private scope chip clears back to team files from its own button", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: { files: [file("a.md")], "ui:file-nav": [ui({ scope: "private" })] },
  });
  const chip = pane.frame.locator("#scope-chip");
  await expect(chip).toContainText("Private");
  await expect(chip.locator("button svg")).toBeVisible();
  await chip.getByRole("button", { name: "Show team files", exact: true }).click();
  await expect(chip).toBeHidden();
});

test("active selection loads unopened folders then permits Collapse all", async ({ mountPane }) => {
  const folder = file("folder", { kind: "dir" });
  const child = file("folder/a.md", { name: "a.md" });
  const pane = await mountPane(PANE, {
    ...mount,
    topics: { files: [folder, child], "ui:file-nav": [ui()] },
  });
  await expect(pane.frame.getByRole("treeitem")).toHaveCount(1);
  await pane.writeUi({ selected: child.path });
  await expect(pane.frame.locator('[data-path="/fixture/folder/a.md"]')).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await command(pane, "collapse-all");
  await expect(pane.frame.locator('[data-path="/fixture/folder"]')).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(pane.frame.locator('[data-path="/fixture/folder/a.md"]')).toHaveCount(0);
});

test("a pointed file keeps its row, focus and selection while git states and counts stream", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, {
    ...mount,
    topics: {
      files: [file("a.md"), file("b.md", { kind: "epic", storyCount: 1 })],
      "ui:file-nav": [ui()],
    },
  });
  const row = pane.frame.locator('[data-path="/fixture/b.md"]');
  await expect(row).toBeVisible();
  await pane.writeUi({ selected: "/fixture/b.md" });
  await expect(row).toHaveAttribute("aria-selected", "true");
  await row.focus();
  await watchRow(pane.frame, row);
  const start = Date.now();
  let count = 0;
  for (; Date.now() - start < 5000; count++) {
    await pane.upsert("files", file("a.md", { gitState: count % 2 ? "added" : "modified" }));
    await pane.upsert(
      "files",
      file("b.md", {
        kind: "epic",
        storyCount: count + 2,
        gitState: count % 2 ? "modified" : "added",
      }),
    );
    await pane.page.waitForTimeout(100);
  }
  await expect(row.locator(".count")).toHaveText(String(count + 1));
  const result = await rowStability(pane.frame);
  console.log("files-tree stability", result);
  expect(result.frames).toBeGreaterThan(5);
  expect(result).toMatchObject({
    replacements: 0,
    pointerReplacements: 0,
    removals: 0,
    sameRow: true,
    samePointer: true,
  });
  await expect(row).toBeFocused();
  await expect(row).toHaveAttribute("aria-selected", "true");
  await pane.upsert("files", file("b.md", { kind: "epic", gitState: "deleted" }));
  await expect(row).not.toHaveAttribute("data-codesense-row");
  await expect(row).toHaveAttribute("draggable", "false");
  await row.click();
  expect((await pane.verbs()).filter((v) => v.verb === "openFile")).toHaveLength(0);
});

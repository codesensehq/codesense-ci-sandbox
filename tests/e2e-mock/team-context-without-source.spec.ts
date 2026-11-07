// SPDX-License-Identifier: MIT
import { expect, test } from "@playwright/test";

test("a zero-source team builds context without connecting code", async ({ page }) => {
  // Persisted test filesystem + production-shaped watcher events. Native
  // persistence and macOS responsiveness are verified separately.
  await page.addInitScript(() => {
    type Internals = {
      invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
      runCallback: (id: number, payload: unknown) => void;
    };
    let installed: Internals;
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      get: () => installed,
      set: (internals: Internals) => {
        installed = internals;
        const original = internals.invoke.bind(internals);
        const listeners = new Map<number, { event: string; callback: number }>();
        const files: Record<string, string> = JSON.parse(localStorage.getItem("sv1-files") ?? "{}");
        internals.invoke = async (command, args = {}) => {
          if (command === "plugin:event|listen") {
            const id = (await original(command, args)) as number;
            listeners.set(id, { event: String(args.event), callback: Number(args.handler) });
            return id;
          }
          if (command === "plugin:event|unlisten") listeners.delete(Number(args.eventId));
          const path = String(args.path ?? "");
          if (command === "create_file" || command === "write_content_file") {
            files[path] = String(args.content);
            localStorage.setItem("sv1-files", JSON.stringify(files));
            for (const [id, listener] of listeners) {
              if (listener.event === "context-file-change")
                internals.runCallback(listener.callback, {
                  event: listener.event,
                  id,
                  payload: { repo_path: "", file_path: path },
                });
            }
            return;
          }
          if (command === "read_file" && path in files) return files[path];
          if (command === "scan_pipeline_tree")
            return {
              dirPath: "",
              dirName: "content",
              absoluteDirPath: args.teamContentPath,
              stories: [],
              subEpics: [],
              orderError: null,
              mainDoc: null,
              docs: Object.keys(files)
                .filter((p) => p.startsWith(`${args.teamContentPath}/`))
                .map((filePath) => ({ filePath, fileName: filePath.split("/").pop() })),
            };
          return original(command, args);
        };
      },
    });
  });
  await page.goto("/?scenario=noSourceRepos");
  await page.waitForFunction(() =>
    Boolean(window.__stores?.useViewerStore && window.__stores?.useOrgStore),
  );
  await page.waitForFunction(() =>
    window.__stores!.useOrgStore.getState().orgs.some((o) => o.slug === "acme/widgets"),
  );
  await page.evaluate(() => {
    const { useOrgStore: org, useViewerStore: viewer } = window.__stores!;
    org.setState({ orgs: [...org.getState().orgs] });
    org.getState().setActiveOrg("acme/widgets");
    org.getState().setActiveTeam("default");
    viewer.getState().setLayoutMode("viewer");
    viewer.getState().closeAllTabs("acme/widgets:default");
    viewer.setState({ workspaceTraySeenCoachmark: true });
  });
  await expect(page.getByTestId("left-nav-scope-row")).toBeVisible();
  await expect(page.getByText("Connect your code", { exact: true })).toHaveCount(0);
  await expect(page.getByTestId("left-nav-empty")).toBeVisible();
  await page.getByRole("button", { name: "New file", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "New file" })).toBeVisible();
  await page.getByRole("textbox", { name: "File name" }).fill("idea.md");
  await expect(page.getByRole("button", { name: "Create", exact: true })).toBeEnabled();
  await page.getByRole("button", { name: "Create", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "New file" })).toHaveCount(0);
  await expect(page.getByTestId("left-nav-empty")).toHaveCount(0);
  await expect(
    page.getByRole("list").getByRole("button", { name: "idea", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  const editor = page.locator('.cm-content[contenteditable="true"]').first();
  await editor.fill("# Source-free plan\n\nSaved without an agent or application code.");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() => page.evaluate(() => Object.values(JSON.parse(localStorage.getItem("sv1-files")!))))
    .toEqual(["# Source-free plan\n\nSaved without an agent or application code."]);
  await expect(page.getByLabel("Unsaved changes", { exact: true })).toHaveCount(0);
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("sv1-files")!));
  expect(Object.values(saved)).toEqual([
    "# Source-free plan\n\nSaved without an agent or application code.",
  ]);
  await page.getByRole("button", { name: "Done editing", exact: true }).click();

  // The first time this test's preview/editor path renders on a fresh dev server, it goes through
  // the server's cold, on-demand module transform. Under load that can outlast the default 5 s
  // expect timeout (the test passes alone and on warm repeats), so this one assertion gets a
  // longer budget.
  await expect(page.getByRole("heading", { name: "Source-free plan" })).toBeVisible({
    timeout: 30_000,
  });
  await page.reload();
  await page.waitForFunction(() =>
    window.__stores?.useOrgStore.getState().orgs.some((o) => o.slug === "acme/widgets"),
  );
  await page.evaluate(() => {
    const { useOrgStore: org, useViewerStore: viewer } = window.__stores!;
    org.getState().setActiveOrg("acme/widgets");
    org.getState().setActiveTeam("default");
    viewer.getState().setLayoutMode("viewer");
    viewer.setState({ workspaceTraySeenCoachmark: true });
  });
  await page.getByRole("list").getByRole("button", { name: "idea", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Source-free plan" })).toBeVisible();
  const sources = await page.evaluate(
    () =>
      window
        .__stores!.useOrgStore.getState()
        .orgs.find((o) => o.slug === "acme/widgets")!
        .teams.find((t) => t.name === "default")!.source_repos,
  );
  expect(sources).toEqual([]);
});

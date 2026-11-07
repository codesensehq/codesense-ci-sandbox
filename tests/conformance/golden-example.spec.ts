// SPDX-License-Identifier: MIT
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type MountedPane } from "../pane-harness/harness";

const golden =
  "codesense-docs/getting-started/content/examples/dashboards/live-list-with-command.html";

async function toggleFilter(pane: MountedPane, file = golden) {
  const html = readFileSync(file, "utf8");
  const [command] = JSON.parse(/name="codesense:commands"\s+content='([^']+)'/.exec(html)![1]);
  await pane.page.evaluate(async (command) => {
    // @ts-expect-error -- browser import served by the fixture Vite host.
    const { postDashboardCommand } = await import("/src/features/dashboard/lib/frame-lifecycle.ts");
    postDashboardCommand("live-list-with-command.html", "right-pane", {
      id: command.id,
      label: command.label,
      dashboardTitle: "Live sessions",
    });
  }, command);
}

test("a copied team pane stays live, renders pushed rows and handles its declared command", async ({
  mountPane,
}) => {
  const root = mkdtempSync(join(tmpdir(), "golden-pane-team-"));
  try {
    const panes = join(root, "team/panes");
    mkdirSync(panes, { recursive: true });
    const file = join(panes, "live-list-with-command.html");
    copyFileSync(golden, file);
    const pane = await mountPane(file, {
      title: "Live sessions",
      width: 260,
      sessions: [
        { id: "working", sessionId: "working", displayName: "Builder", state: "running" },
        { id: "idle", sessionId: "idle", displayName: "Reviewer", state: "idle" },
      ],
    });
    await expect(pane.frame.getByRole("status")).toContainText("All sessions · 2 sessions");
    await pane.upsert("sessions", {
      id: "new",
      sessionId: "new",
      displayName: "New arrival",
      state: "running",
    });
    await expect(pane.frame.getByText("New arrival")).toBeVisible();
    await expect(pane.frame.locator("[data-codesense-row]").first()).toHaveAttribute(
      "data-codesense-row",
      /"kind":"agent"/,
    );
    // The browser fixture is the real host/shim delivery seam, not an MCP server.
    // Rust pane_tools covers canonical pane_write → pane_open → pane_list separately.
    // Deliver the exact declared id through the same ready-gated host queue as command_run.
    await toggleFilter(pane, file);
    await expect(pane.frame.getByRole("status")).toContainText("Running only · 2 sessions");
    await expect(pane.frame.getByText("Reviewer", { exact: true })).toHaveCount(0);
    await expect(pane.frame.getByText("Builder", { exact: true })).toBeVisible();
    await pane.page.waitForTimeout(3200);
    await expect(pane.page.getByTestId("slot-fallback-strip")).toHaveCount(0);
    await expect(pane.frame.getByRole("button", { name: "Show all sessions" })).toBeVisible();
    await pane.close();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("live pushes keep the focused session, and filtering it returns focus to the filter", async ({
  mountPane,
}) => {
  const pane = await mountPane(golden, {
    sessions: [
      { id: "working", sessionId: "working", displayName: "Builder", state: "running" },
      { id: "idle", sessionId: "idle", displayName: "Reviewer", state: "idle" },
    ],
  });
  const reviewer = pane.frame.getByRole("listitem").filter({ hasText: "Reviewer" });
  await pane.focus();
  await expect(pane.frame.getByRole("button", { name: "Show running only" })).toBeFocused();
  await reviewer.focus();
  await expect(reviewer).toBeFocused();
  await pane.upsert("sessions", {
    id: "working",
    sessionId: "working",
    displayName: "Builder updated",
    state: "running",
  });
  await expect(pane.frame.getByText("Builder updated")).toBeVisible();
  await expect(reviewer).toBeFocused();
  await toggleFilter(pane);
  await expect(reviewer).toHaveCount(0);
  await expect(pane.frame.getByRole("button", { name: "Show all sessions" })).toBeFocused();
  await pane.close();
});

test("a changed dock subscription view clears old sessions while its snapshot is pending", async ({
  mountPane,
}) => {
  const pane = await mountPane(golden, {
    slotId: "dock",
    sessions: [{ id: "old", sessionId: "old", displayName: "Previous team", state: "running" }],
  });
  await expect(pane.frame.getByText("Previous team")).toBeVisible();
  await pane.page.evaluate(() => {
    const api = (window as any).__TAURI_INTERNALS__;
    const invoke = api.invoke;
    api.invoke = async (command: string, args: any, ...rest: any[]) => {
      // A team-safe grant resolves to the newly bound team's projection.
      if (command === "live_hub_view_key") return "team:fixture/ops";
      if (command === "live_hub_subscribe" && args.topic === "sessions") {
        await new Promise<void>((resolve) => {
          (window as any).resumeSessions = resolve;
        });
        const snapshot = await invoke(command, args, ...rest);
        return {
          ...snapshot,
          snapshot: [
            { id: "new", sessionId: "new", displayName: "Current team", state: "running" },
          ],
          total: 1,
        };
      }
      return invoke(command, args, ...rest);
    };
  });
  await pane.setTeam("fixture", "ops");
  await expect(pane.frame.locator("#location")).toContainText("fixture/ops");
  await expect(pane.frame.getByText("Previous team")).toHaveCount(0);
  await expect(pane.frame.getByRole("status")).toHaveText("Catching up…");
  await expect
    .poll(() => pane.page.evaluate(() => typeof (window as any).resumeSessions))
    .toBe("function");
  await pane.page.evaluate(() => (window as any).resumeSessions());
  await expect(pane.frame.getByText("Current team")).toBeVisible();
  await expect(pane.frame.getByRole("status")).toContainText("All sessions · 1 session");
  await pane.close();
});

test("showing unchanged sessions settles the catching-up state", async ({ mountPane }) => {
  const pane = await mountPane(golden, {
    sessions: [{ id: "same", displayName: "Unchanged session", state: "running" }],
  });
  await expect(pane.frame.getByRole("status")).toContainText("All sessions · 1 session");
  await pane.show(false);
  await expect(pane.frame.locator("#status")).toHaveText("Hidden — updates paused");
  await pane.show(true);
  await expect(pane.frame.getByText("Unchanged session")).toBeVisible();
  await expect(pane.frame.getByRole("status")).toContainText("All sessions · 1 session");
  await pane.close();
});

test("an Open-policy team switch keeps the unchanged all-sessions view", async ({ mountPane }) => {
  const pane = await mountPane(golden, {
    slotId: "dock",
    sessions: [{ id: "global", displayName: "Global session", state: "running" }],
  });
  await expect(pane.frame.getByText("Global session")).toBeVisible();
  // Use the fixture's default Open grant and unchanged `all` view key.
  await pane.setTeam("fixture", "ops");
  await expect(pane.frame.locator("#location")).toContainText("fixture/ops");
  await expect(pane.frame.getByText("Global session")).toBeVisible();
  await expect(pane.frame.getByRole("status")).toContainText("All sessions · 1 session");
  await pane.close();
});

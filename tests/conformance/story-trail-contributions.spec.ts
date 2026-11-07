// SPDX-License-Identifier: MIT
import { test, expect } from "../pane-harness/harness";
import { agentRow } from "./checks";
import { writeTrailFixture } from "../../scripts/story-trail-conformance.mjs";
const fixture =
  process.env.STORY_TRAIL_TEST_FIXTURE ??
  writeTrailFixture("/tmp/cde19-pr3-contributions/trail.html", { contributions: true });

test("contributions keep recorded facts, native disclosure and paged rows", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-contribution-meta]")).toContainText("Merged");
  await expect(frame.locator("[data-file-count]")).toHaveText("2 files");
  await expect(frame.locator('[aria-label="Files changed"]')).toContainText("+12 −3");
  await expect(frame.locator("[data-commits]")).toContainText("Builder");
  await frame.locator("[data-untouched] summary").click();
  await expect(frame.locator("[data-untouched] details")).toHaveAttribute("open", "");
  await frame
    .locator("[data-commits] li")
    .first()
    .evaluate((node) => {
      (window as any).firstContributionRow = node;
    });
  await frame.locator("[data-more-commits]").click();
  await expect(frame.locator("[data-commits] li")).toHaveCount(2);
  await expect(frame.locator("[data-more-commits]")).toHaveCount(0);
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  await expect(frame.locator("[data-open=lead]")).toHaveText("Builder refreshed");
  expect(
    await frame
      .locator("[data-commits] li")
      .first()
      .evaluate((node) => node === (window as any).firstContributionRow),
  ).toBe(true);
  await expect(frame.locator("[data-untouched] details")).toHaveAttribute("open", "");
  await pane.close();
});

test("removed worktree and unknown planned or line facts remain explicit", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-file-count]")).toHaveText("2 files");
  await frame.evaluate(() => {
    const cs = (window as any).codesense,
      read = cs.storyChanges;
    cs.storyChanges = async (args) => {
      const data = await read(args);
      data.worktrees = [{ path: "/removed", state: "removed", basis: "exact" }];
      data.planned = null;
      data.unavailable.planned = "Touchpoints not recorded";
      data.unavailable.fileLines = "story spans branches with no single final tree";
      for (const file of data.actual) {
        file.lines = null;
        file.linesReason = data.unavailable.fileLines;
      }
      return data;
    };
  });
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  await expect(frame.locator("[data-worktree-note]")).toContainText("worktree was removed");
  await expect(frame.locator("[data-planned-reason]")).toHaveText("Touchpoints not recorded");
  await expect(frame.locator("[data-lines-reason]")).toHaveText(
    "story spans branches with no single final tree",
  );
  await expect(frame.locator("[data-file-planned]").first()).toHaveText("");
  await expect(frame.locator("[data-task=lead]")).toBeVisible();
  await expect(frame.locator("[data-commits] li")).toHaveCount(1);
  await pane.close();
});

test("a changed commit cursor keeps loaded history until explicit reload", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await frame.locator("[data-more-commits]").click();
  await expect(frame.locator("[data-commits] li")).toHaveCount(2);
  await frame.evaluate(() => {
    const cs = (window as any).codesense,
      read = cs.storyChanges;
    cs.storyChanges = async (args) => {
      const data = await read(args);
      if (!args.cursor) {
        data.commits = [
          { ...data.commits[0], sha: "d".repeat(40), subject: "New recorded change" },
        ];
        data.total = 3;
        data.nextCursor = "changed-cursor";
      }
      return data;
    };
  });
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  await expect(frame.locator("[data-reload-commits]")).toBeVisible();
  await expect(frame.locator("[data-commits] li")).toHaveCount(3);
  await expect(frame.locator("[data-changes-error]")).toContainText("Commit history changed");
  await frame.locator("[data-reload-commits]").click();
  await expect(frame.locator("[data-commits] li")).toHaveCount(1);
  await expect(frame.locator("[data-commits]")).toContainText("New recorded change");
  await expect(frame.locator("[data-changes-error]")).toBeHidden();
  await pane.close();
});

test("a held Load more click wins over a pending passive story read", async ({ mountPane }) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-file-count]")).toHaveText("2 files");
  await frame.evaluate(() => {
    const w = window as any,
      cs = w.codesense,
      read = cs.storyChanges;
    let passive = false,
      more = false;
    w.holdContributionRead = false;
    cs.storyChanges = (args) => {
      if (w.holdContributionRead && !args.cursor && !passive) {
        passive = true;
        return new Promise((resolve, reject) => {
          w.finishContributionRead = () => read(args).then(resolve, reject);
        });
      }
      if (args.cursor && !more) {
        more = true;
        return new Promise((resolve, reject) => {
          w.finishMoreRead = () => read(args).then(resolve, reject);
        });
      }
      return read(args);
    };
  });
  const target = frame.locator("[data-more-commits]");
  await target.scrollIntoViewIfNeeded();
  await expect(target).not.toHaveAttribute("aria-disabled", "true");
  const box = (await target.boundingBox())!;
  await pane.page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await pane.page.mouse.down();
  await frame.evaluate(() => {
    (window as any).holdContributionRead = true;
  });
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  await expect
    .poll(() => frame.evaluate(() => typeof (window as any).finishContributionRead))
    .toBe("function");
  await pane.page.mouse.up();
  await expect
    .poll(() => frame.evaluate(() => typeof (window as any).finishMoreRead))
    .toBe("function");
  await expect(target).toHaveAttribute("aria-disabled", "true");
  await frame.evaluate(() => (window as any).finishMoreRead());
  await expect(frame.locator("[data-commits] li")).toHaveCount(2);
  await frame.evaluate(() => (window as any).finishContributionRead());
  await expect(frame.locator("[data-commits] li")).toHaveCount(2);
  await pane.close();
});

test("recorded diff actions preserve slug and revision and expose typed missing-commit advice", async ({
  mountPane,
}) => {
  const pane = await mountPane(fixture, { sessions: [agentRow("lead", "Builder")] }),
    frame = pane.frame;
  await expect(frame.locator("[data-file-count]")).toHaveText("2 files");
  await frame.evaluate(() => {
    const w = window as any,
      cs = w.codesense,
      read = cs.storyChanges;
    cs.storyChanges = async (args) => ({ ...(await read(args)), sourceRepo: "example/project" });
    w.diffActions = [];
    cs.openDiff = async (args) => {
      w.diffActions.push(args);
      return args.rev === "WORKTREE"
        ? { status: "opened" }
        : {
            status: "commitNotLocal",
            reason: "This commit isn't on this Mac yet",
            fetchCommand: "git fetch origin " + args.rev,
          };
    };
    cs.openWorkspace = async (args) => {
      w.workspaceAction = args;
      return {};
    };
  });
  await pane.upsert("sessions", agentRow("lead", "Builder refreshed"));
  const live = frame.locator('[data-diff-rev="WORKTREE"]').first();
  await live.click();
  await expect
    .poll(() => frame.evaluate(() => (window as any).diffActions[0]))
    .toEqual({ repo: "example/project", rev: "WORKTREE", path: "src/app.ts", storyId: "SV-X" });
  const committed = frame.locator('[data-diff-rev="' + "a".repeat(40) + '"]').first();
  await committed.click();
  await expect(frame.locator("[data-diff-result]")).toContainText(
    "This commit isn't on this Mac yet",
  );
  await expect(frame.locator('[aria-label="Fetch command"]')).toHaveValue(
    "git fetch origin " + "a".repeat(40),
  );
  await expect(frame.locator('[aria-label="Fetch command"]')).toHaveAttribute("readonly", "");
  await frame.locator("[data-open-workspace]").click();
  await expect
    .poll(() => frame.evaluate(() => (window as any).workspaceAction))
    .toEqual({ storyId: "SV-X" });
  await pane.close();
});

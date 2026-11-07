// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

function chromeFixtures() {
  const f = agentListFixtures(8, "teams");
  const refs = f.topics!.agentList.filter((r) => r.sessionRef);
  for (const r of refs.slice(4)) r.teamKey = "fixture:ops";
  const state = f.topics!.agentList.find((r) => r.rowType === "state")!;
  state.teamOrder = ["fixture:dev", "fixture:ops"];
  state.teamNames = {
    "fixture:dev": { org: "Fixture org", team: "dev" },
    "fixture:ops": { org: "Fixture org", team: "ops" },
  };
  state.pane = {
    openTeams: ["fixture:dev", "fixture:ops"],
    rails: { "fixture:dev": [{ railKey: "r", title: "Current", kind: "custom" }] },
    railKeys: { "fixture:dev": { ws0: "r" } },
  };
  Object.assign(f.topics!["ui:dock"][0], {
    search: { query: "", status: "all" },
    hiddenTeams: [],
    agentFilter: null,
    workspaceFilter: null,
    teamFocus: null,
  });
  for (const row of f.sessions!) {
    row.agentKey = "worker";
  }
  return f;
}

test("native and pane share search, filters and hidden teams; search owns its keys", async ({
  mountPane,
}) => {
  const pane = await mountPane(AGENT_LIST_PANE, chromeFixtures()),
    frame = pane.frame;
  await pane.writeUi({ hiddenTeams: ["fixture:ops"], search: { query: "Agent 2", status: "all" } });
  await expect(frame.locator("#al-search")).toHaveValue("Agent 2");
  await expect(frame.locator("#count")).toHaveText("1/8");
  await expect(frame.getByText("Team: dev", { exact: true })).toBeVisible();
  await frame.locator("#al-search").focus();
  await frame.locator("#al-search").press("ArrowDown");
  await expect(frame.locator("#al-search")).toBeFocused();
  await frame.locator("#al-search").press("Escape");
  await expect(frame.locator("#al-search")).toHaveValue("");
  await frame.locator("#al-search").fill("Agent 1");
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ topic: "ui:dock", patch: { search: { query: "Agent 1", status: "all" } } });
  await frame.getByRole("button", { name: "Filter and arrange", exact: true }).click();
  await expect(frame.getByRole("checkbox", { name: "dev", exact: true })).toBeDisabled();
  await frame.getByRole("checkbox", { name: "ops", exact: true }).check();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ patch: { hiddenTeams: [] } });
  await frame.getByRole("checkbox", { name: "ops", exact: true }).press("Escape");
  await expect(
    frame.getByRole("button", { name: "Filter and arrange", exact: true }),
  ).toBeFocused();
});

test("tabbing out of Filter and arrange closes it without moving focus back", async ({
  mountPane,
  browserName,
}) => {
  const pane = await mountPane(AGENT_LIST_PANE, chromeFixtures()),
    frame = pane.frame,
    toggle = frame.locator("#al-filter"),
    menu = frame.getByRole("dialog", { name: "Filter and arrange agents" });
  // Safari's all-controls keyboard traversal uses Option+Tab.
  const backwards = browserName === "webkit" ? "Alt+Shift+Tab" : "Shift+Tab",
    forwards = browserName === "webkit" ? "Alt+Tab" : "Tab";
  await toggle.click();
  await expect(menu.getByRole("checkbox", { name: "dev", exact: true })).toBeFocused();
  await menu.getByRole("checkbox", { name: "dev", exact: true }).press(backwards);
  await expect(toggle).toBeFocused();
  await expect(menu).toHaveCount(0);
  await toggle.press(backwards);
  await expect(frame.locator("#al-search")).toBeFocused();

  await toggle.click();
  const last = menu.getByRole("radio", { name: "By team", exact: true });
  await last.focus();
  await last.press(forwards);
  await expect(menu).toHaveCount(0);
  await expect(frame.locator(".al-waiting")).toBeFocused();

  await toggle.click();
  await expect(menu).toBeVisible();
  await toggle.click();
  await expect(menu).toHaveCount(0);
});

for (const width of [236, 268, 400])
  test(`compact header at ${width}px shares the arrangement`, async ({ mountPane }, testInfo) => {
    const f = chromeFixtures();
    f.width = width;
    const pane = await mountPane(AGENT_LIST_PANE, f),
      frame = pane.frame;
    const title = await frame.locator("#al-title").boundingBox(),
      arrange = await frame.locator("#al-arrange").boundingBox();
    expect(
      Math.abs(arrange!.y + arrange!.height / 2 - title!.y - title!.height / 2),
    ).toBeLessThanOrEqual(1);
    await frame.locator("#al-arrange").selectOption("manager");
    await expect(frame.locator("#al-arrange")).toHaveValue("manager");
    await expect
      .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
      .toMatchObject({ patch: { arrange: "manager" } });
    await frame.locator("#al-filter").click();
    await frame.locator("main").screenshot({ path: testInfo.outputPath(`chrome-${width}.png`) });
    await expect(frame.getByRole("radio", { name: "By status" })).toBeEnabled();
    await frame.getByRole("radio", { name: "Ready", exact: false }).click();
    await expect(frame.getByText("No agents ready", { exact: true })).toBeVisible();
    await expect(frame.getByRole("button", { name: "Show all agents", exact: true })).toBeVisible();
  });

test("team funnel restores its prior scope and rail scope stays local", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, chromeFixtures()),
    frame = pane.frame;
  await frame.getByRole("button", { name: "Filter to dev", exact: true }).focus();
  await frame.getByRole("button", { name: "Filter to dev", exact: true }).click();
  await expect(frame.locator("#count")).toHaveText("4/8");
  await expect(frame.getByText("Team: dev", { exact: true })).toBeVisible();
  await frame.getByRole("button", { name: "Filter to dev", exact: true }).click();
  await expect(frame.locator("#count")).toHaveText("8");
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").length)
    .toBe(2);
  const before = 2;
  await frame.locator("#al-scope").selectOption({ label: "dev · Current (custom group)" });
  await expect(frame.locator("#count")).toHaveText("4/8");
  expect((await pane.verbs()).filter((v) => v.verb === "setUiState")).toHaveLength(before);
});

test("pins, host markers, faces and inert role chips appear in both arrangements", async ({
  mountPane,
}) => {
  const f = chromeFixtures();
  f.topics!["ui:dock"][0].pins = {
    "fixture:dev": f.sessions!.slice(0, 6).map((s) => ({
      kind: "session",
      teamKey: "fixture:dev",
      sessionId: s.id,
      handle: s.handle,
      name: s.handle,
    })),
  };
  (
    f.topics!.agentList.find((r) => r.rowType === "state")!.pane as Record<string, unknown>
  ).pinnedRefs = ["r0", "r1", "r2", "r3", "r4", "r5"];
  f.sessions!.forEach((s, i) => (s.pinned = i < 6));
  // Keep all pin entries in their own team.
  f.topics!.agentList.filter((r) => r.sessionRef).forEach((r) => (r.teamKey = "fixture:dev"));
  Object.assign(f.sessions![0], {
    tabHidden: true,
    headless: true,
    dueWake: { name: "Inbox sweep", deadlineMs: Date.now() + 60000 },
    face: { kind: "image", src: "/kit/v1/avatars/builtin-funny/01.svg" },
    faceTag: 2,
    rolePiece: { piece: "manager", label: "Manager" },
    roleHoldings: ["Lead", "Builder", "Reviewer"].map((label) => ({
      label,
      name: label,
      title: label,
    })),
  });
  const pane = await mountPane(AGENT_LIST_PANE, f),
    frame = pane.frame;
  await expect(frame.getByText("Pinned", { exact: true }).first()).toBeVisible();
  await expect(frame.getByRole("button", { name: "+1 more pinned", exact: true })).toBeVisible();
  const copy = frame.locator('[data-row-id="pinned:r0"]');
  await expect(copy.locator(".al-face")).toHaveAttribute("src", /avatars/);
  await expect(copy.locator(".al-tag")).toHaveText("2");
  await expect(copy.locator('.al-marker[data-kind="due"]')).toBeVisible();
  await expect(copy.locator('.al-marker[data-kind="hidden"]')).toHaveCount(1);
  await expect(copy.locator('.al-marker[data-kind="headless"]')).toHaveCount(0);
  await expect(copy.locator('.al-marker[data-kind="pin"]')).toHaveCount(0);
  await expect(copy.locator(".al-role")).toHaveCount(2);
  await expect(copy.locator(".al-role button")).toHaveCount(0);
  await expect(copy.locator(".al-holds")).toContainText("+1");
  await expect(copy.locator('[aria-current="true"]')).toHaveCount(0);
  await frame.getByRole("button", { name: "+1 more pinned", exact: true }).click();
  await expect(frame.getByRole("button", { name: "Show fewer pinned", exact: true })).toBeVisible();
  await frame.locator("#al-arrange").selectOption("manager");
  await expect(frame.locator('[data-row-id="pinned:r0"] .al-holds')).toBeHidden();
  await expect(frame.locator('[data-row-id="r0"] .al-holds')).toBeVisible();
  await expect(copy.locator(".al-main")).toHaveAttribute(
    "aria-label",
    /Holds: Lead, Builder, Reviewer/,
  );
});

test("empty states follow native precedence and expose their recovery", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, chromeFixtures()),
    frame = pane.frame;
  await pane.writeUi({ hiddenTeams: ["fixture:dev", "fixture:ops"] });
  await expect(frame.getByText("Every team is hidden", { exact: true })).toBeVisible();
  await pane.writeUi({ agentFilter: { agentKey: "worker", name: "Worker" } });
  await expect(frame.getByText("Worker runs only in hidden teams", { exact: true })).toBeVisible();
  await pane.writeUi({ agentFilter: { agentKey: "absent", name: "Gone" } });
  await expect(frame.getByText("No Gone sessions running", { exact: true })).toBeVisible();
  await pane.writeUi({ agentFilter: null, hiddenTeams: [] });
  await expect(frame.locator("#count")).toHaveText("8");
  await frame.locator("#al-search").fill("no match");
  await expect(frame.getByText("Nothing matches", { exact: true })).toBeVisible();
  await frame.getByRole("button", { name: "Clear search, filters and teams", exact: true }).click();
  await expect(frame.locator("#count")).toHaveText("8");
  // Wait for the pane's clear to reach the shared owner before the next native write.
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ patch: { workspaceFilter: null, search: { query: "", status: "all" } } });
  await pane.writeUi({});
  const meta = chromeFixtures().topics!.agentList.find((r) => r.rowType === "state")!;
  await pane.upsert("agentList", {
    ...meta,
    pane: {
      workspaceWait: {
        title: "Waiting for Build Mac",
        sentence:
          "This workspace runs on Build Mac, which is offline. Its agents show once it reconnects.",
        busy: false,
      },
    },
  });
  await pane.writeUi({ workspaceFilter: { teamKey: "fixture:dev", workspaceId: "missing" } });
  await expect(frame.getByText("Waiting for Build Mac", { exact: true })).toBeVisible();
  await expect(
    frame.getByRole("button", { name: "Clear the workspace filter", exact: true }),
  ).toBeVisible();
});

test("only new session ids after settling ease in, never virtual remounts", async ({
  mountPane,
}) => {
  const f = agentListFixtures(1, "teams");
  const pane = await mountPane(AGENT_LIST_PANE, f),
    frame = pane.frame;
  await expect(frame.locator(".al-enter")).toHaveCount(0);
  await frame.waitForTimeout(850);
  await pane.upsert("sessions", { ...f.sessions![0], rowNames: { primary: "Updated" } });
  await expect(frame.locator(".al-enter")).toHaveCount(0);
  await pane.upsert("sessions", { ...f.sessions![0], id: "new", sessionId: "new", handle: "new" });
  await pane.upsert("agentList", {
    ...f.topics!.agentList[0],
    id: "new-ref",
    sessionRef: "new",
    order: 2,
  });
  await expect(frame.locator('[data-row-id="new-ref"]')).toHaveClass(/al-enter/);
  await frame.locator("#al-arrange").selectOption("manager");
  await expect(frame.locator('[data-row-id="new-ref"]')).not.toHaveClass(/al-enter/);
});

test("Run history loads only when opened and reads structured and plain output", async ({
  mountPane,
}) => {
  const f = chromeFixtures();
  f.runs = [
    {
      outcome: {
        run_id: "plain",
        story_id: "routine:acme/dev/inbox-sweep@1",
        ended_at: new Date().toISOString(),
        artifact_ref: "output",
        summary: "Inbox cleared",
      },
      validated: "completed",
    },
    {
      outcome: {
        run_id: "structured",
        story_id: "SV-1#review",
        ended_at: new Date(Date.now() - 1000).toISOString(),
        structured_declared: true,
        summary: "Ready for review",
      },
      validated: "needs-attention",
      has_structured_transcript: true,
    },
  ] as never;
  f.runReads = {
    plain: { artifact: "Sweep output", transcript: null, outcome: null, artifactTruncated: false },
    structured: {
      artifact: null,
      transcript: { turns: [{ role: "assistant", text: "Review finished" }] },
      outcome: null,
      artifactTruncated: false,
    },
  };
  const pane = await mountPane(AGENT_LIST_PANE, f),
    frame = pane.frame;
  expect((await pane.verbs()).filter((v) => v.verb === "runs")).toHaveLength(0);
  await frame.locator("#al-history").click();
  await expect(frame.locator("#al-title")).toHaveText("Run history");
  await expect(frame.locator("#count")).toHaveText("2");
  await expect(frame.locator("#al-arrange")).toBeHidden();
  await frame.getByRole("button", { name: /inbox-sweep/ }).click();
  await expect(frame.getByText("Sweep output", { exact: true })).toBeVisible();
  await frame.getByRole("button", { name: "Back to runs", exact: true }).click();
  await frame.getByRole("button", { name: /SV-1 · Review/ }).click();
  await expect(frame.getByText("Review finished", { exact: true })).toBeVisible();
  await frame.getByRole("button", { name: "Back to agents", exact: true }).click();
  await expect(frame.locator("#al-search")).toBeVisible();
});

test("search uses the host native corpus and marks retained Manager ancestors", async ({
  mountPane,
}) => {
  const f = chromeFixtures();
  f.topics!["ui:dock"][0].arrange = "manager";
  f.sessions![2].searchText = "Nightly inbox codex";
  const pane = await mountPane(AGENT_LIST_PANE, f),
    frame = pane.frame;
  await frame.locator("#al-search").fill("Nightly codex");
  await expect(frame.locator("#count")).toHaveText("1/8");
  await expect(frame.getByText("Manager context", { exact: true })).toHaveCount(2);
  await expect(frame.locator('[data-row-id="r2"] .al-main')).not.toHaveAttribute("data-context");
});

test("history refresh preserves Find-runs focus and selected output; activation focuses history", async ({
  mountPane,
}) => {
  const f = chromeFixtures();
  f.runs = [
    {
      outcome: {
        run_id: "run",
        story_id: "SV-1",
        ended_at: new Date().toISOString(),
        artifact_ref: "output",
      },
      validated: "completed",
    },
  ] as never;
  f.runReads = {
    run: {
      artifact: "Keep this transcript",
      transcript: null,
      outcome: null,
      artifactTruncated: false,
    },
  };
  const pane = await mountPane(AGENT_LIST_PANE, f),
    frame = pane.frame;
  await frame.locator("#al-history").click();
  const find = frame.getByRole("searchbox", { name: "Find runs" });
  await find.fill("SV-1");
  const before = (await pane.verbs()).filter((v) => v.verb === "runs").length;
  await pane.upsert("sessions", { ...f.sessions![0], alive: false });
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "runs").length)
    .toBeGreaterThan(before);
  await expect(find).toBeFocused();
  await expect(find).toHaveValue("SV-1");
  await frame.getByRole("button", { name: /SV-1/ }).click();
  await expect(frame.getByText("Keep this transcript", { exact: true })).toBeVisible();
  const opened = (await pane.verbs()).filter((v) => v.verb === "runs").length;
  await pane.upsert("sessions", { ...f.sessions![1], alive: false });
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "runs").length)
    .toBeGreaterThan(opened);
  await expect(frame.getByText("Keep this transcript", { exact: true })).toBeVisible();
  await pane.page.locator("body").click({ position: { x: 1, y: 1 } });
  await pane.focus();
  await expect(find).toBeFocused();
});

for (const via of ["switch", "command", "menu"])
  test(`arrangement preserves the first visible agent and offset via ${via}`, async ({
    mountPane,
  }) => {
    const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(50, "teams")),
      frame = pane.frame;
    await frame.locator("#list").evaluate((el) => {
      el.scrollTop = 950;
    });
    const anchor = () =>
      frame.locator("#list").evaluate((box) => {
        const top = box.getBoundingClientRect().top;
        const row = [...box.querySelectorAll<HTMLElement>("[data-row-id]")].find(
          (el) => el.querySelector(".al-main") && el.getBoundingClientRect().bottom > top,
        )!;
        return row
          ? { id: row.dataset.rowId, within: row.getBoundingClientRect().top - top }
          : null;
      });
    await expect.poll(async () => (await anchor())?.within ?? 1).toBeLessThanOrEqual(0);
    const before = await anchor();
    if (via === "command")
      await pane.page
        .locator("iframe")
        .evaluate((el: HTMLIFrameElement) =>
          el.contentWindow!.postMessage(
            { type: "codesense:command", id: "switch-arrangement" },
            "*",
          ),
        );
    else if (via === "menu") {
      await frame.locator("#al-filter").click();
      await frame.getByRole("radio", { name: "By manager", exact: true }).click();
    } else await frame.locator("#al-arrange").selectOption("manager");
    await expect(frame.locator("#al-arrange")).toHaveValue("manager");
    await expect.poll(anchor).toEqual(before);
  });

test("Status uses native sections and server order, and round trips the native dock setting", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(4, "teams");
  fixtures.sessions![3].effectiveStatus = "idle";
  fixtures.views = {
    [JSON.stringify(["agentList", "all|status"])]: [
      ...[2, 1, 0, 3].map((i, order) => ({
        ...fixtures.topics!.agentList[i],
        order,
        statusGroup: i === 2 ? 0 : i === 3 ? 2 : 1,
      })),
      fixtures.topics!.agentList.find((r) => r.rowType === "state")!,
    ],
  };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    frame = pane.frame;
  await pane.writeUi({ arrange: "status" });
  await expect(frame.locator(".al-section")).toHaveText(["Needs you", "Working", "Ready"]);
  expect(
    await frame
      .locator(".al-row")
      .evaluateAll((rows) => rows.map((row) => row.getAttribute("data-agent-id"))),
  ).toEqual(["s2", "s1", "s0", "s3"]);
  await frame.locator("#al-filter").click();
  await expect(frame.getByRole("radio", { name: "By status", exact: true })).toBeEnabled();
  await expect(frame.getByRole("radio", { name: "By status", exact: true })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await frame.getByRole("radio", { name: "By team", exact: true }).click();
  await frame.getByRole("radio", { name: "By status", exact: true }).click();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ patch: { arrange: "status" } });
  await expect
    .poll(() =>
      pane.page.evaluate(async () => {
        const { liveHub } = await import("/src/features/dashboard/lib/live-hub.ts");
        return (await liveHub.snapshot("ui:dock", "all")).snapshot[0].arrange;
      }),
    )
    .toBe("status");
  await pane.writeUi({ arrange: "manager" });
  await expect(frame.locator(".al-section").filter({ hasText: "Working" })).toHaveCount(0);
  await expect
    .poll(async () =>
      (await pane.verbs()).some(
        (v) => v.verb === "unsubscribe" && (v.args as any).order === "status",
      ),
    )
    .toBe(true);
});

test("Status arrangement preserves the CP2 agent anchor after the view arrives", async ({
  mountPane,
  page,
}) => {
  const fixtures = agentListFixtures(80, "teams");
  const first = {
    topic: "agentList",
    viewKey: "all",
    rev: 0,
    snapshot: structuredClone(fixtures.topics!.agentList),
  };
  await page.addInitScript((snapshot) => {
    (window as any).__codesenseFirstPaint = snapshot;
  }, first);
  fixtures.views = {
    [JSON.stringify(["agentList", "all|status"])]: [
      ...fixtures
        .topics!.agentList.filter((r) => r.sessionRef)
        .map((r, order) => ({ ...r, parentId: null, crewParentId: null, order, statusGroup: 1 })),
      fixtures.topics!.agentList.find((r) => r.rowType === "state")!,
    ],
  };
  for (const ref of fixtures.topics!.agentList.filter((r) => r.sessionRef)) {
    ref.crewOrder = 79 - Number(String(ref.sessionRef).slice(1));
    ref.workspaceRank = 7 - Number(String(ref.workspaceRef).slice(2));
  }
  const pane = await mountPane(AGENT_LIST_PANE, fixtures),
    frame = pane.frame;
  await frame.locator("#list").evaluate((el) => {
    el.scrollTop = 950;
  });
  const anchor = () =>
    frame.locator("#list").evaluate((box) => {
      const top = box.getBoundingClientRect().top;
      const row = [...box.querySelectorAll<HTMLElement>("[data-row-id]")].find(
        (el) => el.querySelector(".al-main") && el.getBoundingClientRect().bottom > top,
      );
      return row ? { id: row.dataset.rowId, within: row.getBoundingClientRect().top - top } : null;
    });
  await expect.poll(async () => (await anchor())?.within ?? 1).toBeLessThanOrEqual(0);
  const before = await anchor();
  await frame.locator("#al-filter").click();
  await frame.getByRole("radio", { name: "By status", exact: true }).click();
  await expect(frame.getByRole("button", { name: "Arrange by team", exact: true })).toBeVisible();
  await expect.poll(anchor).toEqual(before);
  await frame.getByRole("radio", { name: "By team", exact: true }).click();
  await expect(frame.getByRole("button", { name: "Arrange by team", exact: true })).toHaveCount(0);
  await expect.poll(anchor).toEqual(before);
});

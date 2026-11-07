// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

for (const arrange of ["teams", "manager"]) {
  test(`subagents override idle and stalled, with needs-you and crash precedence in ${arrange}`, async ({
    mountPane,
  }) => {
    const fixtures = agentListFixtures(6, arrange);
    fixtures.height = 1000;
    for (const [i, status, model, subs] of [
      [0, "idle", "opus", 2],
      [1, "stalled", "opus", 1],
      [2, "idle", "codex", 2],
      [3, "paused", "opus", 1],
      [4, "crashed", "codex", 2],
      [5, "blocked", "codex", 1],
    ] as const) {
      Object.assign(fixtures.sessions![i], {
        effectiveStatus: status,
        model,
        subagents: subs,
        needsYouKind: null,
        pendingAsk: null,
      });
    }
    const pane = await mountPane(AGENT_LIST_PANE, fixtures);
    for (const [id, count] of [
      ["s0", 2],
      ["s1", 1],
      ["s2", 2],
    ] as const) {
      const row = pane.frame.locator(`[data-agent-id="${id}"]`);
      await expect(row.locator(".al-state")).toContainText("Working");
      await expect(row.locator(".al-state .al-subs")).toHaveText(String(count));
      await expect(row.locator(".al-dot")).toHaveAttribute(
        "aria-label",
        `Working · ${count} ${count === 1 ? "subagent" : "subagents"}`,
      );
    }
    for (const id of ["s3", "s4", "s5"]) {
      const row = pane.frame.locator(`[data-agent-id="${id}"]`);
      await expect(row.locator(".al-state")).not.toContainText("Working");
      await expect(row.locator(".al-state .al-subs")).toHaveCount(0);
    }
  });

  test(`needs-you updates in place in ${arrange}`, async ({ mountPane }) => {
    const fixtures = agentListFixtures(5, arrange);
    fixtures.height = 1000;
    const pane = await mountPane(AGENT_LIST_PANE, fixtures);
    const order = () =>
      pane.frame
        .locator(".al-row")
        .evaluateAll((rows) => rows.map((r) => (r as HTMLElement).dataset.agentId));
    await expect(pane.frame.locator('[data-agent-id="s4"]')).toBeVisible();
    const before = await order();
    const top = (await pane.frame.locator('[data-agent-id="s3"]').boundingBox())!.y;
    await pane.upsert("sessions", {
      ...fixtures.sessions![3],
      effectiveStatus: "awaiting-answer",
      needsYouKind: "question",
      pendingAsk: "Choose a branch",
    });
    await expect(pane.frame.locator('[data-agent-id="s3"]')).toHaveAttribute("data-waiting", "");
    expect(await order()).toEqual(before);
    expect((await pane.frame.locator('[data-agent-id="s3"]').boundingBox())!.y).toBe(top);
  });

  for (const action of ["Quick view", "Messages"]) {
    test(`${action} is available on hover and focus in ${arrange}`, async ({ mountPane }) => {
      const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(1, arrange));
      const row = pane.frame.locator('[data-agent-id="s0"]');
      await row.locator(".al-main").hover();
      const button = row.getByRole("button", {
        name: `${action} ${action === "Quick view" ? "of" : "for"} Agent 0`,
        exact: true,
      });
      await button.focus();
      await expect(button).toBeFocused();
      const style = await row.evaluate((r) => ({
        actions: getComputedStyle(r.querySelector(".al-acts")!).opacity,
        state: getComputedStyle(r.querySelector(".al-state")!).visibility,
      }));
      expect(Number(style.actions)).toBe(1);
      expect(style.state).toBe("hidden");
      await button.click();
      await expect(
        pane.page.getByTestId(action === "Quick view" ? "agent-quick-send" : "agent-inbox-chat"),
      ).toBeVisible();
    });
  }
}

test("dealt fallback faces match the host resolver when face facts have not arrived", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(5, "teams");
  fixtures.height = 1000;
  fixtures.sessions!.forEach((s, i) => {
    s.subagents = 0;
    s.displayName = ["Builder", "Sam 🦆", "Mary", "Reviewer", "Fallback"][i];
    if (i === 2) s.agentRevision = "codesense:dev:reviewer@abc123";
    if (i === 3) s.avatar = "builtin-funny:coffee-bot";
  });
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const expected = await pane.page.evaluate(async (sessions) => {
    const { resolveAgentFace } = await import("/src/features/viewer/lib/agent-list-view.ts");
    return sessions.map((s) =>
      resolveAgentFace(s as never, { name: String((s.rowNames as { primary: string }).primary) }),
    );
  }, fixtures.sessions!);
  for (let i = 0; i < expected.length; i++) {
    expect(expected[i].kind).toBe("image");
    const face = expected[i] as { kind: "image"; ref: string };
    await expect(
      pane.frame.locator(
        `[data-agent-id="s${i}"] .al-face img, [data-agent-id="s${i}"] img.al-face`,
      ),
    ).toHaveAttribute("src", `/kit/v1/avatars/${face.ref.replace(":", "/")}.svg`);
  }
});

test("markdown activity is one line and long or missing names stay contained", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(3, "teams");
  fixtures.width = 236;
  fixtures.sessions![0].progress = {
    note: "**Review** the `pane`\n- Keep **layout** stable",
    phase: "dev",
  };
  fixtures.sessions![1].rowNames = {
    primary: "A developer with a remarkably long display name",
    full: "A developer with a remarkably long display name",
  };
  delete fixtures.sessions![2].rowNames;
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await expect(pane.frame.locator('[data-agent-id="s0"] .al-main')).toHaveAttribute(
    "title",
    /Review the pane Keep layout stable/,
  );
  await expect(pane.frame.locator('[data-agent-id="s2"] .al-name')).toHaveText("agent-2");
  const spills = await pane.frame.locator(".al-name, .al-ask").evaluateAll(
    (elements) =>
      elements.filter((el) => {
        const row = el.closest(".al-item")!.getBoundingClientRect(),
          box = el.getBoundingClientRect();
        return box.right > row.right || box.bottom > row.bottom;
      }).length,
  );
  expect(spills).toBe(0);
});

test("oneLine preserves word boundaries across markdown lines and emphasis", async ({
  mountPane,
}) => {
  const cases = [
    [
      "Review the pane\n- Keep layout stable\n  * Preserve focus\n\t+ Keep names",
      "Review the pane Keep layout stable Preserve focus Keep names",
    ],
    [
      "Review the pane\n1. Keep layout stable\n  2) Preserve focus",
      "Review the pane Keep layout stable Preserve focus",
    ],
    ["- Review the pane\n\n- Keep layout stable", "Review the pane Keep layout stable"],
    [
      "**Review** **the** _pane_\n~~Keep~~ ***layout*** __stable__",
      "Review the pane Keep layout stable",
    ],
    [
      "Use agent_name and `pane_id`\n```ts\nconst agent_id = 1;\n```\n- Keep **layout** stable",
      "Use agent_name and pane_id const agent_id = 1; Keep layout stable",
    ],
  ];
  const fixtures = agentListFixtures(cases.length, "teams");
  fixtures.height = 1000;
  for (const [i, [note]] of cases.entries()) {
    Object.assign(fixtures.sessions![i], {
      effectiveStatus: "tool-running",
      needsYouKind: null,
      pendingAsk: null,
      progress: { note, phase: "dev" },
    });
  }
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  for (const [i, [, expected]] of cases.entries()) {
    await expect(pane.frame.locator(`[data-agent-id="s${i}"] .al-main`)).toHaveAttribute(
      "title",
      new RegExp(expected),
    );
  }
});

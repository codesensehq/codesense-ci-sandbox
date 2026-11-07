// SPDX-License-Identifier: MIT
// SV-AGP-9 QA: a 50-agent world (pins, roles, crews, subagents, needs-you, blocked, offline,
// crashed, ended) built on the preview world, plus an in-page 100ms delta stream.
import type { Page } from "@playwright/test";
import { previewWorld } from "./agent-list-visual-fixtures";

export function qaWorld(arrange = "teams", pinCount = 7) {
  const world = previewWorld({ arrange, selected: "s2" }) as any;
  const base = world.sessions.length;
  const statuses = ["running", "thinking", "tool-running", "idle", "running", "thinking"];
  const refs = world.topics.agentList.filter((r: any) => r.rowType !== "state");
  const state = world.topics.agentList.find((r: any) => r.rowType === "state");
  for (let i = base; i < 50; i++) {
    const team = i % 3 ? "codesense:riverside" : "codesense:core";
    const ws = `QA-${i % 5}`;
    const lead = (i - base) % 4 === 0;
    const parent = lead ? null : refs[refs.length - ((i - base) % 4)];
    world.sessions.push({
      id: "s" + i,
      sessionId: "s" + i,
      handle: `qa-agent-${i}`,
      kind: "cli",
      alive: true,
      state: "running",
      effectiveStatus: statuses[i % statuses.length],
      needsYouKind: null,
      pendingAsk: null,
      team: { org: "codesense", team: team.split(":")[1] },
      machine: "This Mac",
      machineId: "local",
      model: i % 2 ? "codex" : "opus",
      rowNames: { primary: `QA agent ${i}`, secondary: "Developer", full: `QA agent ${i}` },
      displayName: `QA agent ${i}`,
      searchText: `QA agent ${i} Developer ${ws}`,
      subagents: i % 7 === 0 ? 2 : 0,
      launchedBy: parent ? world.sessions[Number(parent.sessionRef.slice(1))].handle : null,
      startedAtMs: Date.now() - i * 60000,
      progress: { note: `Working on ${ws}` },
      pinned: false,
      ...(i % 9 === 0
        ? { roleHoldings: [{ role: "builder", name: "Builder", label: "Builder" }] }
        : {}),
    });
    const ref = {
      id: "r" + i,
      sessionRef: "s" + i,
      inboxRef: "counts:team:" + team.replace(":", "/"),
      teamKey: team,
      parentId: parent && parent.teamKey === team && parent.workspaceRef === ws ? parent.id : null,
      parentSource: parent ? "launcher" : null,
      order: i,
      workspaceRef: ws,
      workspaceRank: 0,
      workspace: {
        id: ws,
        label: ws,
        kind: "story",
        storyStatus: "in-progress",
        stageRole: "active",
        stageLabel: "In progress",
        sourceRepoSlug: "alderco/codesense",
      },
      crewParentId: null as string | null,
      crewOrder: i,
      crewLead: true,
    };
    if (ref.parentId) {
      ref.crewParentId = ref.parentId;
      ref.crewLead = false;
    }
    refs.push(ref);
    if (!(state.pane.workspaceOrder[team] ??= []).includes(ws))
      state.pane.workspaceOrder[team].push(ws);
  }
  for (const r of refs)
    r.workspaceRank = r.workspaceRef
      ? state.pane.workspaceOrder[r.teamKey].indexOf(r.workspaceRef)
      : 99;
  // Unread mail on a few rows for the messages badge.
  for (const inbox of world.inbox) {
    const team = inbox.id.split("/").pop();
    for (const s of world.sessions)
      if (s.team.team === team && Number(s.id.slice(1)) % 6 === 1) {
        inbox.recipients[s.handle] = 3;
        inbox.needsYouBySender[s.handle] = 2;
        inbox.sessions[s.handle] = { unread: 3, groupUnread: 0, groupReadId: 0 };
      }
  }
  // Pins: a manual order with more than five, for "+N more pinned".
  const pinIds = ["r8", "r15", "r1", "r30", "r12", "r40", "r22"].slice(0, pinCount);
  state.pane.pinnedRefs = pinIds;
  for (const s of world.sessions) s.pinned = pinIds.includes("r" + s.id.slice(1));
  // The canonical pins (what Pin/Unpin toggle), in the same manual order, per team.
  const pins: Record<string, unknown[]> = {};
  for (const id of pinIds) {
    const ref = refs.find((r: any) => r.id === id);
    const s = world.sessions.find((x: any) => x.id === ref.sessionRef);
    (pins[ref.teamKey] ??= []).push({
      kind: "session",
      teamKey: ref.teamKey,
      handle: s.handle,
      sessionId: s.sessionId,
      name: s.rowNames?.primary ?? s.handle,
    });
  }
  world.topics["ui:dock"][0].pins = pins;
  world.topics.agentList = [...refs, state];
  world.height = 900;
  return world;
}

/** Start a host-side 100ms delta stream over a few sessions (status, note, heartbeat). */
export async function startStream(
  page: Page,
  world: any,
  ids = ["s0", "s3", "s6", "s17", "s19", "s25", "s33"],
) {
  const rows = world.sessions.filter((s: any) => ids.includes(s.id));
  await page.evaluate((rows) => {
    const w = window as any;
    let i = 0;
    const statuses = ["tool-running", "thinking", "running"];
    w.__qaStreamTicks = 0;
    w.__qaStream = setInterval(() => {
      i++;
      for (const [k, s] of rows.entries())
        w.upsert("sessions", {
          ...s,
          effectiveStatus: statuses[(i + k) % 3],
          progress: { note: `Live step ${i}` },
          activityAtMs: Date.now(),
          heartbeatAtMs: Date.now(),
        });
      w.__qaStreamTicks++;
    }, 100);
  }, rows);
}

export async function stopStream(page: Page) {
  return page.evaluate(() => {
    clearInterval((window as any).__qaStream);
    return (window as any).__qaStreamTicks as number;
  });
}

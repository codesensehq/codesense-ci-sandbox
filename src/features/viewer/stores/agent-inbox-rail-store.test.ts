// SPDX-License-Identifier: MIT
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { beforeEach, describe, expect, it } from "vitest";

import {
  cleanupAgentInboxRailTab,
  closeAgentInboxRail,
  openAgentInboxRail,
  setAgentInboxRailLens,
  setAgentInboxRailWidth,
  useAgentInboxRailStore,
} from "./agent-inbox-rail-store";

const SID = "sid-triage";

beforeEach(() => {
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    processAgentBySession: { "sid-triage": "codex", "sid-a": "codex", "sid-b": "codex" },
  });
  useAgentInboxRailStore.setState({ bySession: {}, widthByTab: {}, lensByTab: {} });
});

describe("agent inbox rail store", () => {
  it("closes an exited agent's rail even while its pane is unmounted", () => {
    useAgentStateStore.getState().setProcessAgent(SID, "claude");
    openAgentInboxRail(SID);
    expect(useAgentInboxRailStore.getState().bySession[SID]).toBeDefined();
    useAgentStateStore
      .getState()
      .recordSignal(SID, { tier: 1, signal: { kind: "claude-hook", event: "SessionEnd" } });
    expect(useAgentInboxRailStore.getState().bySession[SID]).toBeUndefined();
  });

  it("open adds an entry with composeRequest 0", () => {
    openAgentInboxRail(SID);
    expect(useAgentInboxRailStore.getState().bySession[SID]).toEqual({ composeRequest: 0 });
  });

  it("open with compose bumps composeRequest each call", () => {
    openAgentInboxRail(SID, { compose: true });
    openAgentInboxRail(SID, { compose: true });
    expect(useAgentInboxRailStore.getState().bySession[SID]?.composeRequest).toBe(2);
  });

  it("plain re-open of an entry is a no-op (identity preserved)", () => {
    openAgentInboxRail(SID);
    const before = useAgentInboxRailStore.getState().bySession[SID];
    openAgentInboxRail(SID);
    const after = useAgentInboxRailStore.getState().bySession[SID];
    expect(after).toBe(before);
  });

  it("close drops the entry", () => {
    openAgentInboxRail(SID, { compose: true });
    closeAgentInboxRail(SID);
    expect(useAgentInboxRailStore.getState().bySession[SID]).toBeUndefined();
  });

  it("each session is isolated", () => {
    openAgentInboxRail("sid-a", { compose: true });
    openAgentInboxRail("sid-b");
    expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toEqual({ composeRequest: 1 });
    expect(useAgentInboxRailStore.getState().bySession["sid-b"]).toEqual({ composeRequest: 0 });
    closeAgentInboxRail("sid-a");
    expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeUndefined();
    expect(useAgentInboxRailStore.getState().bySession["sid-b"]).toEqual({ composeRequest: 0 });
  });
});

describe("SW-IB1-B2 — widthByTab", () => {
  const TAB = "tab-rust-build";
  const TAB_B = "tab-triage";

  it("returns undefined before any width is written", () => {
    expect(useAgentInboxRailStore.getState().widthByTab[TAB]).toBeUndefined();
  });

  it("setAgentInboxRailWidth stores a width keyed by tab id", () => {
    setAgentInboxRailWidth(TAB, 560);
    expect(useAgentInboxRailStore.getState().widthByTab[TAB]).toBe(560);
  });

  it("writing the same value preserves slice identity (no churn on live drags)", () => {
    setAgentInboxRailWidth(TAB, 460);
    const before = useAgentInboxRailStore.getState().widthByTab;
    setAgentInboxRailWidth(TAB, 460);
    const after = useAgentInboxRailStore.getState().widthByTab;
    expect(after).toBe(before);
  });

  it("tabs are isolated", () => {
    setAgentInboxRailWidth(TAB, 500);
    setAgentInboxRailWidth(TAB_B, 300);
    expect(useAgentInboxRailStore.getState().widthByTab[TAB]).toBe(500);
    expect(useAgentInboxRailStore.getState().widthByTab[TAB_B]).toBe(300);
  });

  it("cleanupTab removes the stored width", () => {
    setAgentInboxRailWidth(TAB, 500);
    cleanupAgentInboxRailTab(TAB);
    expect(useAgentInboxRailStore.getState().widthByTab[TAB]).toBeUndefined();
    expect(TAB in useAgentInboxRailStore.getState().widthByTab).toBe(false);
  });

  it("cleanupTab on an unknown tab is a no-op (slice identity preserved)", () => {
    setAgentInboxRailWidth(TAB, 500);
    const before = useAgentInboxRailStore.getState().widthByTab;
    cleanupAgentInboxRailTab("tab-never-written");
    const after = useAgentInboxRailStore.getState().widthByTab;
    expect(after).toBe(before);
  });

  it("width persists across session changes within the same tab", () => {
    // tabId stays constant; the owning sessionId can change (re-attach, etc.) —
    // widthByTab[tabId] must be independent of session identity.
    setAgentInboxRailWidth(TAB, 560);
    openAgentInboxRail("sid-first");
    closeAgentInboxRail("sid-first");
    openAgentInboxRail("sid-replacement");
    expect(useAgentInboxRailStore.getState().widthByTab[TAB]).toBe(560);
  });

  it("remembers each tab's lens and drops it with the tab", () => {
    setAgentInboxRailLens(TAB, "wire");
    setAgentInboxRailLens(TAB_B, "spool");
    expect(useAgentInboxRailStore.getState().lensByTab).toEqual({
      [TAB]: "wire",
      [TAB_B]: "spool",
    });
    const before = useAgentInboxRailStore.getState();
    setAgentInboxRailLens(TAB, "wire");
    expect(useAgentInboxRailStore.getState()).toBe(before);
    cleanupAgentInboxRailTab(TAB);
    expect(TAB in useAgentInboxRailStore.getState().lensByTab).toBe(false);
  });
});

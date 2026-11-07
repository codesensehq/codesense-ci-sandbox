// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";
import type { AgentRow } from "@/tauri/commands";

import {
  agentRowLine,
  agentRowStatus,
  modelWord,
  needsModelEndpoint,
  shortAccount,
} from "./agent-row-state";
import type { RunningInstance } from "./agent-running";

const ACCOUNT_OK = {
  spawn_eligible: true,
  source: "runtime_default",
  reason: "ok",
  label: null,
  account_id: null,
  bound_account_id: null,
  store_error: null,
};

function row(over: Partial<AgentRow> = {}): AgentRow {
  return {
    key: "acme/api/triage",
    name: "Triage",
    runtime: "claude",
    archived: false,
    running_now: 0,
    model: null,
    account: ACCOUNT_OK,
    ...over,
  } as unknown as AgentRow;
}

function instance(over: Partial<RunningInstance> = {}): RunningInstance {
  return {
    handle: "h",
    name: "h",
    state: "running",
    stateWord: "Working",
    where: "Team folder",
    machine: null,
    since: null,
    unread: 0,
    ...over,
  };
}

describe("agentRowStatus", () => {
  it("is ready for an idle agent that can launch", () => {
    expect(agentRowStatus(row(), [])).toEqual({
      state: "ready",
      live: 0,
      needsYou: 0,
      canLaunch: true,
      fix: null,
    });
  });

  it("counts its live instances as running", () => {
    const s = agentRowStatus(row({ running_now: 2 }), []);
    expect(s.state).toBe("running");
    expect(s.live).toBe(2);
  });

  it("counts the instances it can see when the backend count is behind", () => {
    const s = agentRowStatus(row(), [instance({ handle: "a" }), instance({ handle: "b" })]);
    expect(s.live).toBe(2);
    expect(s.state).toBe("running");
  });

  it("is needs when an instance of this agent waits on you, over running", () => {
    const s = agentRowStatus(row({ running_now: 2 }), [
      instance({ handle: "a", state: "needs" }),
      instance({ handle: "b" }),
    ]);
    expect(s.state).toBe("needs");
    expect(s.needsYou).toBe(1);
  });

  it("is broken when the account can't launch, and says it can't launch", () => {
    const s = agentRowStatus(
      row({ account: { ...ACCOUNT_OK, spawn_eligible: false, reason: "auth-required" } as never }),
      [],
    );
    expect(s.state).toBe("broken");
    expect(s.canLaunch).toBe(false);
  });

  it("is archived over everything else", () => {
    const s = agentRowStatus(row({ archived: true, running_now: 1 }), [
      instance({ state: "needs" }),
    ]);
    expect(s.state).toBe("archived");
  });
});

describe("modelWord and shortAccount", () => {
  it("names the model, or the CLI default", () => {
    expect(modelWord(row({ model: "opus" }))).toBe("opus");
    expect(modelWord(row())).toBe("CLI default");
  });

  it("names the pinned account, the Active one marked so, or your own login", () => {
    expect(
      shortAccount({ ...ACCOUNT_OK, source: "binding", label: "Work" } as never, "claude"),
    ).toBe("on Work");
    expect(
      shortAccount({ ...ACCOUNT_OK, resolved_source: "active", label: "Work" } as never, "claude"),
    ).toBe("on Work (Active)");
    expect(shortAccount(ACCOUNT_OK as never, "claude")).toBe("your own login");
  });

  it("never reads a refused Active account as your own login", () => {
    const refused = {
      ...ACCOUNT_OK,
      resolved_source: "active",
      spawn_eligible: false,
      reason: "account-missing",
    } as never;
    expect(agentRowStatus(row({ account: refused }), []).state).toBe("broken");
    expect(agentRowLine(row({ account: refused }), "broken", null)).toBe(
      "Claude · the account no longer exists",
    );
  });
});

describe("agentRowLine", () => {
  it("is runtime, model, account and next run", () => {
    expect(agentRowLine(row({ model: "opus" }), "ready", "09:00")).toBe(
      "Claude · opus · your own login · Next 09:00",
    );
  });

  it("leaves the next run out when there is none", () => {
    expect(agentRowLine(row(), "ready", null)).toBe("Claude · CLI default · your own login");
  });

  it("gives the reason for a row that can't launch", () => {
    const line = agentRowLine(
      row({ account: { ...ACCOUNT_OK, spawn_eligible: false, reason: "auth-required" } as never }),
      "broken",
      null,
    );
    expect(line).toBe("Claude · the account needs you to sign in again");
  });

  it("says a row waits on you, and an archived one is archived", () => {
    expect(agentRowLine(row(), "needs", null)).toBe("Waiting on you");
    expect(agentRowLine(row({ archived: true }), "archived", null)).toBe("Archived");
  });
});

describe.each(REGISTERED_AGENT_CLIS)(
  "a $runtime agent with no model",
  ({ runtime, modelEndpoint }) => {
    const bare = row({ runtime, model: null });
    const label = agentRowLine(bare, "ready", null).split(" · ")[0];

    it("can't launch only when its CLI needs a model endpoint", () => {
      expect(needsModelEndpoint(bare)).toBe(modelEndpoint);
      const status = agentRowStatus(bare, []);
      expect(status.state).toBe(modelEndpoint ? "broken" : "ready");
      expect(status.canLaunch).toBe(!modelEndpoint);
      expect(status.fix).toBe(modelEndpoint ? "endpoint" : null);
    });

    it("says what it lacks, never CLI default, and that a scheduled run can't start", () => {
      const status = agentRowStatus(bare, []);
      const line = agentRowLine(bare, status.state, "18:00");
      if (modelEndpoint) {
        expect(line).toBe(`${label} · Needs a model endpoint · Next 18:00 can't start`);
        expect(agentRowLine(bare, status.state, null)).toBe(`${label} · Needs a model endpoint`);
      } else expect(line).toBe(`${label} · CLI default · your own login · Next 18:00`);
    });

    it("with a model names it and launches", () => {
      const named = row({ runtime, model: "qwen2.5:3b" });
      expect(needsModelEndpoint(named)).toBe(false);
      expect(agentRowStatus(named, []).state).toBe("ready");
      expect(agentRowLine(named, "ready", null)).toBe(`${label} · qwen2.5:3b · your own login`);
    });
  },
);

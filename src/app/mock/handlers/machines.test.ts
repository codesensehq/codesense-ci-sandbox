// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import type {
  InstallTerminalServerResult,
  MachineDraftInput,
  MachineSummary,
  MachineTestReport,
} from "@/tauri/commands";
import type { MachineFileRead, MachineLayoutSnapshot } from "@/tauri/draft-commands";
import type { FileEntry } from "@/tauri/commands";

import { createMockBackend } from "../mock-backend";
import { resolveScenario } from "../scenario";
import { machineHandlers, resolveMachineFixture } from "./machines";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "../fixtures";

const ZERO = { connectMs: 0, listMs: 0, slowListMs: 0, verySlowListMs: 0 };
const NOW = 1_700_000_000_000;

function backend(key: "none" | "shared" | "demo" = "demo") {
  const h = machineHandlers(key, ZERO, () => NOW);
  return {
    h,
    list: (team: string | null = MOCK_TEAM_NAME, orgSlug = MOCK_ORG_SLUG) =>
      h.list_machines({ orgSlug, team }) as MachineSummary[],
    call: <T>(cmd: string, args: Record<string, unknown> = {}) =>
      Promise.resolve(h[cmd](args)) as Promise<T>,
  };
}

function draft(host: string, over: Partial<MachineDraftInput> = {}): MachineDraftInput {
  return {
    label: "",
    host,
    port: 22,
    username: "dev",
    jumpHost: null,
    signIn: { method: "agent" },
    trustedHostKeyFp: null,
    ...over,
  };
}

describe("resolveMachineFixture", () => {
  it("defaults to none (the zero-friction floor) and accepts shared/demo", () => {
    expect(resolveMachineFixture("")).toBe("none");
    expect(resolveMachineFixture("?machines=bogus")).toBe("none");
    expect(resolveMachineFixture("?machines=shared")).toBe("shared");
    expect(resolveMachineFixture("?machines=demo")).toBe("demo");
  });
});

describe("list_machines", () => {
  it("none → This Mac only", () => {
    expect(
      backend("none")
        .list()
        .map((m) => m.machineId),
    ).toEqual(["local"]);
  });

  it("orders This Mac, then the team's shared machines, then personal ones", () => {
    const rows = backend().list();
    expect(rows[0].kind).toBe("local");
    const kinds = rows.slice(1).map((m) => m.scope?.kind);
    expect(kinds.indexOf("personal")).toBeGreaterThan(kinds.lastIndexOf("team"));
  });

  it("shares a team machine only with that team; personal machines follow you", () => {
    const other = backend().list("docs");
    expect(other.some((m) => m.scope?.kind === "team")).toBe(false);
    expect(other.some((m) => m.scope?.kind === "personal")).toBe(true);
  });

  it("a same-named team in another org doesn't see the shared machines", () => {
    const otherOrg = backend().list(MOCK_TEAM_NAME, "someone-else/other");
    expect(otherOrg.some((m) => m.scope?.kind === "team")).toBe(false);
  });

  it("demo covers every status state", () => {
    const states = new Set(
      backend()
        .list()
        .map((m) => m.status.state),
    );
    for (const s of [
      "connected",
      "idle",
      "reconnecting",
      "unreachable",
      "needsCredential",
      "authFailed",
      "hostKeyChanged",
      "notInstalled",
      "versionMismatch",
    ]) {
      expect(states.has(s as MachineSummary["status"]["state"]), s).toBe(true);
    }
  });

  it("a shared declaration never carries a username or credential for a member who hasn't set it up", () => {
    const buildFarm = backend()
      .list()
      .find((m) => m.machineId === "build-farm")!;
    expect(buildFarm.binding).toBeNull();
    expect(buildFarm.status.state).toBe("needsCredential");
  });
});

describe("connection lifecycle", () => {
  it("retrying an unreachable machine stays honest: still unreachable, attempt counted, backoff grows", async () => {
    const b = backend();
    const after = await b.call<MachineSummary>("connect_machine", { machineId: "devbox-01" });
    expect(after.status).toMatchObject({ state: "unreachable", attempt: 4, retryInSecs: 60 });
  });

  it("idle / reconnecting connect; disconnect goes idle", async () => {
    const b = backend();
    expect(
      (await b.call<MachineSummary>("connect_machine", { machineId: "archive-box" })).status.state,
    ).toBe("connected");
    expect(
      (await b.call<MachineSummary>("connect_machine", { machineId: "edge-01" })).status.state,
    ).toBe("connected");
    expect(
      (await b.call<MachineSummary>("disconnect_machine", { machineId: "lab-pi" })).status.state,
    ).toBe("idle");
  });

  it("uses a one-use prepare token for supported replacement", async () => {
    const b = backend();
    await expect(
      Promise.resolve().then(() =>
        b.call("prepare_machine_terminal_server_replacement", { machineId: "old-vm" }),
      ),
    ).rejects.toThrow(/not safely available/);
    const prepared = await b.call<{ serverToken: string; count: number }>(
      "prepare_machine_terminal_server_replacement",
      { machineId: "frozen-box" },
    );
    expect(prepared.count).toBe(2);
    await expect(
      b.call("restart_machine_terminal_server", {
        machineId: "frozen-box",
        serverToken: "forged",
      }),
    ).rejects.toThrow(/invalid or stale/);
    const again = await b.call<{ serverToken: string }>(
      "prepare_machine_terminal_server_replacement",
      { machineId: "frozen-box" },
    );
    await expect(
      b.call("restart_machine_terminal_server", {
        machineId: "frozen-box",
        serverToken: again.serverToken,
      }),
    ).resolves.toEqual({ status: "replaced", count: 2 });
  });

  it("the layout snapshot carries the census (even while unreachable) and epoch-ended tabs", async () => {
    const b = backend();
    const devbox = await b.call<MachineLayoutSnapshot>("get_machine_layout_snapshot", {
      machineId: "devbox-01",
    });
    expect(devbox.workspaces.map((w) => w.name)).toEqual(["auth-fix", "api-v2", "perf-sweep"]);
    const lab = await b.call<MachineLayoutSnapshot>("get_machine_layout_snapshot", {
      machineId: "lab-pi",
    });
    expect(lab.endedByRestart).toEqual([
      { workspaceName: "train", tab: "watch", cwd: "/home/dev/src/lab" },
    ]);
  });

  it("opens a terminal only on a connected machine — never elsewhere", async () => {
    const b = backend();
    await b.call("open_machine_terminal", { machineId: "gpu-box-2", cwd: "~", command: null });
    const snap = await b.call<MachineLayoutSnapshot>("get_machine_layout_snapshot", {
      machineId: "gpu-box-2",
    });
    expect(snap.workspaces.some((w) => w.name === "~")).toBe(true);
    expect(() =>
      b.h.open_machine_terminal({ machineId: "devbox-01", cwd: "~", command: null }),
    ).toThrow(/not connected/);
  });
});

describe("test_machine_connection — every failure names its next step (Sally §5)", () => {
  const test = (input: MachineDraftInput) =>
    backend().call<MachineTestReport>("test_machine_connection", { input });
  const step = (r: MachineTestReport, id: string) => r.steps.find((s) => s.id === id)!;

  it("first sight of a host key asks for Trust before anything else", async () => {
    const r = await test(draft("devbox-02.int"));
    expect(step(r, "hostKey").status).toBe("needsTrust");
    expect(step(r, "hostKey").fingerprint).toMatch(/^SHA256:/);
    expect(step(r, "auth").status).toBe("skipped");
  });

  it("a trusted new host passes up to a missing terminal server, with an install plan", async () => {
    const first = await test(draft("devbox-02.int"));
    const fp = step(first, "hostKey").fingerprint!;
    const r = await test(draft("devbox-02.int", { trustedHostKeyFp: fp }));
    expect(r.steps.map((s) => s.status)).toEqual(["ok", "ok", "ok", "ok", "ok", "warn"]);
    expect(r.installPlan?.targetDir).toBe("~/.codesense/host/1.0.0-mock");
  });

  it("timeout → Edit address / Use jump host / Retry", async () => {
    const r = await test(draft("timeout.int"));
    expect(step(r, "reachable")).toMatchObject({
      status: "fail",
      nextSteps: ["editAddress", "useJumpHost", "retry"],
    });
  });

  it("auth rejected → Choose key / Change user / Copy public key", async () => {
    const trusted = async (host: string, user: string) => {
      const fp = step(await test(draft(host)), "hostKey").fingerprint!;
      return test(draft(host, { username: user, trustedHostKeyFp: fp }));
    };
    const r = await trusted("devbox-02.int", "root");
    expect(step(r, "auth")).toMatchObject({
      status: "fail",
      nextSteps: ["chooseKey", "changeUser", "copyPublicKey"],
    });
  });

  it("team-pin mismatch → blocked, no bypass", async () => {
    const r = await test(draft("mitm.int"));
    expect(step(r, "hostKey")).toMatchObject({ status: "blocked", nextSteps: [] });
  });

  it("unsupported OS → file view only; disk full is named", async () => {
    const trustedTest = async (host: string) => {
      const fp = step(await test(draft(host)), "hostKey").fingerprint!;
      return test(draft(host, { trustedHostKeyFp: fp }));
    };
    expect((await trustedTest("box.bsd")).fileViewOnly).toBe(true);
    expect(step(await trustedTest("full.int"), "dataDir").detail).toMatch(/disk full/);
  });
});

describe("install_terminal_server", () => {
  it("an interrupted upload is resumable, then succeeds", async () => {
    const b = backend();
    const input = draft("flaky-vm.int");
    const first = await b.call<InstallTerminalServerResult>("install_terminal_server", { input });
    expect(first).toMatchObject({ ok: false, code: "interrupted" });
    const second = await b.call<InstallTerminalServerResult>("install_terminal_server", { input });
    expect(second).toEqual({ ok: true, version: "1.0.0", logoutWarning: null });
  });
});

describe("save_machine", () => {
  it("closing mid-flow saves a draft that lists as Needs setup", async () => {
    const b = backend("none");
    const saved = await b.call<MachineSummary>("save_machine", {
      machineId: null,
      input: draft("devbox-02.int", { label: "devbox-02" }),
      share: { kind: "personal" },
      connect: false,
    });
    expect(saved.status.state).toBe("needsCredential");
    expect(b.list().map((m) => m.machineId)).toContain("devbox-02");
  });

  it("connect saves it set up (idle until connected); a key file binds only its path, a kept passphrase its handle", async () => {
    const b = backend("none");
    const saved = await b.call<MachineSummary>("save_machine", {
      machineId: null,
      input: draft("devbox-02.int", {
        label: "devbox-02",
        signIn: { method: "keyFile", keyPath: "~/.ssh/id" },
      }),
      share: { kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME },
      connect: true,
    });
    expect(saved.status).toEqual({ state: "idle" });
    expect(saved.binding?.keyPath).toBe("~/.ssh/id");
    expect(saved.binding?.credentialRef).toBeNull();
    const handle = await b.call<{ provider: string; account: string }>(
      "capture_machine_passphrase",
      { keyPath: "~/.ssh/locked" },
    );
    const locked = await b.call<MachineSummary>("save_machine", {
      machineId: saved.machineId,
      input: draft("devbox-02.int", {
        signIn: { method: "keychainPassphrase", keyPath: "~/.ssh/locked", credentialRef: handle },
      }),
      share: { kind: "personal" },
      connect: true,
    });
    expect(locked.binding?.credentialRef).toEqual(handle);
    expect(
      await b.call("capture_machine_passphrase", { keyPath: "~/.ssh/cancel" }),
      "a dismissed dialog keeps nothing",
    ).toBeNull();
    expect(saved.scope).toEqual({ kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME });
  });

  it("hide / unshare / remove", async () => {
    const b = backend();
    await b.call("set_machine_hidden", { machineId: "build-farm", hidden: true });
    expect(b.list().find((m) => m.machineId === "build-farm")?.binding?.hidden).toBe(true);
    const unshared = await b.call<MachineSummary>("unshare_machine", { machineId: "gpu-box-2" });
    expect(unshared.scope).toEqual({ kind: "personal" });
    await b.call("remove_machine", { machineId: "scratch-vm" });
    expect(b.list().some((m) => m.machineId === "scratch-vm")).toBe(false);
  });

  it("auto-connect is explicit, reversible, and preserved by re-enrollment", async () => {
    const b = backend();
    await b.call("set_machine_auto_connect", { machineId: "archive-box", autoConnect: false });
    expect(b.list().find((m) => m.machineId === "archive-box")?.binding?.autoConnect).toBe(false);
    await b.call("set_machine_auto_connect", { machineId: "archive-box", autoConnect: true });
    const resaved = await b.call<MachineSummary>("save_machine", {
      machineId: "archive-box",
      input: draft("archive-box.int"),
      share: { kind: "personal" },
      connect: true,
    });
    expect(resaved.binding?.autoConnect).toBe(true);
    await b.call("set_machine_auto_connect", { machineId: "archive-box", autoConnect: false });
    expect(b.list().find((m) => m.machineId === "archive-box")?.binding?.autoConnect).toBe(false);
  });

  it("auto-connect is personal for an enrolled team member", async () => {
    const member = backend("shared");
    await member.call<MachineSummary>("save_machine", {
      machineId: "gpu-box-2",
      input: draft("gpu-box-2.int"),
      share: { kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME },
      connect: true,
    });
    await member.call("set_machine_auto_connect", {
      machineId: "gpu-box-2",
      autoConnect: true,
    });
    expect(member.list().find((m) => m.machineId === "gpu-box-2")?.binding?.autoConnect).toBe(true);
  });

  it("auto-connect denies This Mac and missing/needs-setup bindings", async () => {
    const demo = backend();
    expect(() =>
      demo.call("set_machine_auto_connect", { machineId: "local", autoConnect: true }),
    ).toThrow(/This Mac/);

    const none = backend("none");
    const draftRow = await none.call<MachineSummary>("save_machine", {
      machineId: null,
      input: draft("draft.int", { label: "draft" }),
      share: { kind: "personal" },
      connect: false,
    });
    expect(() =>
      none.call("set_machine_auto_connect", { machineId: draftRow.machineId, autoConnect: true }),
    ).toThrow(/Set up/);

    expect(() =>
      none.call("set_machine_auto_connect", { machineId: "missing-machine", autoConnect: true }),
    ).toThrow(/unknown machine/);
  });
});

describe("remote files", () => {
  it("lists dirs-first through the machine, and refuses while it isn't connected", async () => {
    const b = backend();
    const rows = await b.call<FileEntry[]>("machine_fs_list", {
      machineId: "gpu-box-2",
      dirPath: "/home/dev/src/codesense",
    });
    expect(rows.map((r) => r.name)).toEqual([
      "node_modules",
      "src",
      "Cargo.toml",
      "README.md",
      "server.log",
    ]);
    await expect(
      b.call("machine_fs_list", { machineId: "devbox-01", dirPath: "/home/dev/src/codesense" }),
    ).rejects.toThrow(/not connected/);
  });

  it("reads up to maxBytes and flags a truncated (gated) file", async () => {
    const read = await backend().call<MachineFileRead>("machine_fs_read", {
      machineId: "gpu-box-2",
      path: "/home/dev/src/codesense/server.log",
      maxBytes: 1024,
    });
    expect(read.content.length).toBe(1024);
    expect(read).toMatchObject({ size: 2_400_000, truncated: true });
  });
});

describe("mock integrity (review fixes)", () => {
  it("a new machine never overwrites an existing id", async () => {
    const b = backend();
    const saved = await b.call<MachineSummary>("save_machine", {
      machineId: null,
      input: draft("elsewhere.int", { label: "gpu-box-2" }),
      share: { kind: "personal" },
      connect: false,
    });
    expect(saved.machineId).toBe("gpu-box-2-2");
    expect(b.list().find((m) => m.machineId === "gpu-box-2")?.status.state).toBe("connected");
  });

  it("a member's setup never rewrites the shared declaration", async () => {
    const b = backend("shared");
    const saved = await b.call<MachineSummary>("save_machine", {
      machineId: "build-farm",
      input: draft("elsewhere.int", { label: "renamed" }),
      share: { kind: "personal" },
      connect: true,
    });
    expect(saved.label).toBe("build-farm");
    expect(saved.declaration?.host).toBe("build-farm.int");
    expect(saved.scope).toEqual({ kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME });
  });

  it("a changed host key stays blocked however the host is typed", async () => {
    const r = await backend().call<MachineTestReport>("test_machine_connection", {
      input: draft("CI-RUNNER.int."),
    });
    expect(r.steps.find((s) => s.id === "hostKey")?.status).toBe("blocked");
  });

  it("setting up a teammate's machine keeps you a non-publisher", async () => {
    const b = backend("shared");
    const saved = await b.call<MachineSummary>("save_machine", {
      machineId: "build-farm",
      input: draft("build-farm.int"),
      share: { kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME },
      connect: true,
    });
    expect(saved.canPublish).toBe(false);
  });

  it("opening a tab on one machine never changes another's census", async () => {
    const b = backend();
    await b.call("open_machine_terminal", {
      machineId: "gpu-box-2",
      cwd: "/home/dev/src/codesense",
      command: null,
    });
    const devbox = await b.call<MachineLayoutSnapshot>("get_machine_layout_snapshot", {
      machineId: "devbox-01",
    });
    expect(devbox.workspaces.find((w) => w.name === "auth-fix")?.tabs).toEqual(["claude", "shell"]);
  });

  it("~ reads as the machine's home: repeated New terminal reuses one home workspace", async () => {
    const b = backend();
    await b.call("open_machine_terminal", { machineId: "lab-pi", cwd: "~", command: null });
    await b.call("open_machine_terminal", { machineId: "lab-pi", cwd: "~", command: null });
    const lab = await b.call<MachineLayoutSnapshot>("get_machine_layout_snapshot", {
      machineId: "lab-pi",
    });
    expect(lab.workspaces.filter((w) => w.name === "~")).toHaveLength(1);
  });

  it("a machine whose host key changed stays blocked when re-tested", async () => {
    const r = await backend().call<MachineTestReport>("test_machine_connection", {
      input: draft("ci-runner.int"),
    });
    expect(r.steps.find((s) => s.id === "hostKey")?.status).toBe("blocked");
  });
});

describe("composed into the mock backend", () => {
  it("leaves This Mac's RT-6b restart to the terminal-server world", async () => {
    vi.useFakeTimers();
    try {
      const liveSessions = 4;
      const router = createMockBackend(
        resolveScenario(""),
        `?terminalServer=incompatible&liveSessions=${liveSessions}&machines=demo`,
      );
      // The RT-6b restart (no machine argument) reports the local terminals it closed.
      const local = router("restart_terminal_server", {});
      await vi.runAllTimersAsync();
      await expect(local).resolves.toBe(liveSessions);
      const prepared = await router("prepare_machine_terminal_server_replacement", {
        machineId: "frozen-box",
      });
      const remote = router("restart_machine_terminal_server", {
        machineId: "frozen-box",
        serverToken: (prepared as { serverToken: string }).serverToken,
      });
      await vi.runAllTimersAsync();
      await expect(remote).resolves.toMatchObject({
        status: "replaced",
        count: 2,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("never lets This Mac be disconnected, restarted remotely, hidden, unshared or removed", async () => {
    const { call } = backend();
    for (const cmd of [
      "disconnect_machine",
      "restart_machine_terminal_server",
      "set_machine_hidden",
      "unshare_machine",
      "remove_machine",
    ]) {
      const attempt = Promise.resolve().then(() => call(cmd, { machineId: "local", hidden: true }));
      await expect(attempt).rejects.toThrow(/This Mac/);
    }
  });
});

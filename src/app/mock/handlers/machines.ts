// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { FileEntry } from "@/tauri/commands";
import {
  LOCAL_MACHINE_ID,
  type InstallTerminalServerResult,
  type MachineDraftInput,
  type MachineScope,
  type MachineSummary,
  type TerminalServerInfo,
  type MachineTestReport,
  type MachineTestStep,
  type SshLocalSources,
  type StrayServer,
  type TerminalServersUpdateRun,
  type UnreadableMachineFile,
} from "@/tauri/commands";
import {
  type MachineEndedTab,
  type MachineFileRead,
  type MachineLayoutSnapshot,
  type MachineWorkspaceDescriptor,
} from "@/tauri/draft-commands";
import { MOCK_ORG_SLUG, MOCK_TEAM_NAME } from "../fixtures";

/**
 * The Remote Machines mock backend. Answers the machine registry commands
 * (`commands.ts`, "Remote Machines") and the still-draft connection / layout /
 * file commands (`src/tauri/draft-commands.ts`) from an in-memory machine registry
 * + a small per-machine filesystem, so the picker, the Connections Machines group,
 * the Add-machine flow, every status state and the remote file view are clickable
 * on `pnpm dev:mock` without SSH.
 *
 * The `?machines=` URL param picks the world:
 *   - `none` (default) — only This Mac: the zero-friction floor (no segment).
 *   - `shared`         — the team shares two machines, you have none of your own
 *                        and can't publish (quiet `Needs setup` rows, muted segment).
 *   - `demo`           — one machine per status state, personal + shared, >7 rows.
 */
export type MachineFixtureKey = "none" | "shared" | "demo";

export function resolveMachineFixture(search: string): MachineFixtureKey {
  const raw = new URLSearchParams(search).get("machines");
  return raw === "shared" || raw === "demo" ? raw : "none";
}

/** Artificial latency so the optimistic switch + the tree's 150 ms / 2 s latency
 *  states are visible. Tests pass zeros. */
export interface MachineLatency {
  connectMs: number;
  listMs: number;
  slowListMs: number;
  verySlowListMs: number;
}

const REALISTIC_LATENCY: MachineLatency = {
  connectMs: 600,
  listMs: 60,
  slowListMs: 700,
  verySlowListMs: 2600,
};

export const mockMachineStats = () => ({
  load1: 3.04,
  cpuCount: 8,
  memUsedBytes: 12.1e9,
  memTotalBytes: 16e9,
  diskFreeBytes: 84e9,
  diskTotalBytes: 460e9,
  diskPath: "/home/dev/src",
  sampledAt: Date.now(),
  machineStatsSupported: true,
});

interface MachineRecord {
  summary: MachineSummary;
  snapshot: MachineLayoutSnapshot;
}

const HOME = "/home/dev";
const TEAM: MachineScope = { kind: "team", orgSlug: MOCK_ORG_SLUG, team: MOCK_TEAM_NAME };
const PERSONAL: MachineScope = { kind: "personal" };

function workspace(
  name: string,
  repo: string,
  branch: string | null,
  tabs: string[],
): MachineWorkspaceDescriptor {
  return {
    workspaceId: `ws-${name}`,
    name,
    worktreePath: `${HOME}/src/${repo}`,
    branch,
    tabs,
  };
}

function fingerprintOf(host: string): string {
  let h = 0;
  for (const ch of host) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return `SHA256:${h.toString(36).padStart(7, "x")}Qm4e`;
}

interface Seed {
  id: string;
  scope: MachineScope | null;
  status: MachineSummary["status"];
  workspaces?: MachineWorkspaceDescriptor[];
  attention?: number;
  agentCallbacks?: boolean;
  endedByRestart?: MachineEndedTab[];
  setUp?: boolean;
  /** SW-AGR-19: its terminal server while connected (default: this app's build). */
  terminalServer?: TerminalServerInfo;
}

/** SW-AGR-19: the mock app's release, and a server that is its own build. */
export const MOCK_APP_VERSION = "0.0.22";
const MOCK_STARTED_AT = Date.UTC(2026, 9, 8, 15, 30);

export function mockTerminalServer(
  isLocal: boolean,
  over: Partial<TerminalServerInfo> = {},
): TerminalServerInfo {
  return {
    serverVersion: MOCK_APP_VERSION,
    appVersion: MOCK_APP_VERSION,
    upToDate: true,
    relation: "same",
    announcedVersion: isLocal ? "0.1.0" : "0.1.0+0123456789ab",
    devBuild: false,
    ...(isLocal
      ? {
          serverPath: "/Applications/CodeSense.app/Contents/MacOS/codesense-session-server",
          appServerPath: "/Applications/CodeSense.app/Contents/MacOS/codesense-session-server",
          startedAtMs: MOCK_STARTED_AT,
          pid: 49806,
        }
      : { buildDigest: "0123456789ab" }),
    ...over,
  };
}

function buildRecord(seed: Seed, canPublishTeam: boolean): MachineRecord {
  const isLocal = seed.id === LOCAL_MACHINE_ID;
  const host = `${seed.id}.int`;
  const summary: MachineSummary = {
    machineId: seed.id,
    kind: isLocal ? "local" : "ssh",
    label: isLocal ? "This Mac" : seed.id,
    scope: seed.scope,
    declaration: isLocal
      ? null
      : {
          host,
          port: 22,
          jumpHost: null,
          defaultBaseDir: "~/src",
          pinnedHostKeyFp: fingerprintOf(host),
          pinnedServerId: seed.setUp === false ? null : `mock-server-${seed.id}`,
        },
    binding:
      isLocal || seed.setUp === false
        ? null
        : {
            username: "dev",
            keyPath: null,
            credentialRef: null,
            autoConnect: false,
            hidden: false,
          },
    canPublish: seed.scope?.kind === "team" ? canPublishTeam : true,
    status: seed.status,
    attention: seed.attention ?? 0,
    capabilities: { agentCallbacks: seed.agentCallbacks ?? true },
    ...(seed.terminalServer ? { terminalServer: seed.terminalServer } : {}),
  };
  return {
    summary,
    snapshot: {
      machineId: seed.id,
      serverId: `srv-${seed.id}`,
      epoch: 1,
      // Each machine owns its copy: opening a tab on one never touches another.
      workspaces: (seed.workspaces ?? []).map((w) => ({ ...w, tabs: [...w.tabs] })),
      endedByRestart: seed.endedByRestart ?? [],
    },
  };
}

const AUTH_FIX = workspace("auth-fix", "codesense", "feat/auth-fix", ["claude", "shell"]);
const API_V2 = workspace("api-v2", "api", "feat/api-v2", ["claude"]);

function seedsFor(key: MachineFixtureKey, now: number): Seed[] {
  const local: Seed = {
    id: LOCAL_MACHINE_ID,
    scope: null,
    status: { state: "connected", workspaces: 4 },
  };
  if (key === "none") return [local];
  if (key === "shared") {
    return [
      local,
      { id: "gpu-box-2", scope: TEAM, status: { state: "needsCredential" }, setUp: false },
      { id: "build-farm", scope: TEAM, status: { state: "needsCredential" }, setUp: false },
    ];
  }
  return [
    local,
    {
      id: "gpu-box-2",
      scope: TEAM,
      status: { state: "connected", workspaces: 2 },
      workspaces: [AUTH_FIX, API_V2],
      attention: 1,
    },
    { id: "build-farm", scope: TEAM, status: { state: "needsCredential" }, setUp: false },
    {
      id: "ci-runner",
      scope: TEAM,
      status: {
        state: "hostKeyChanged",
        pinnedFp: fingerprintOf("ci-runner.int"),
        presentedFp: fingerprintOf("ci-runner.rebuilt"),
      },
    },
    {
      id: "devbox-01",
      scope: PERSONAL,
      status: {
        state: "unreachable",
        lastReachedAtMs: now - 3 * 60_000,
        attempt: 3,
        retryInSecs: 15,
      },
      workspaces: [AUTH_FIX, API_V2, workspace("perf-sweep", "codesense", "feat/perf", ["claude"])],
      attention: 2,
    },
    {
      id: "edge-01",
      scope: PERSONAL,
      status: { state: "reconnecting", attempt: 2 },
      workspaces: [workspace("edge-deploy", "edge", "main", ["shell"])],
    },
    {
      id: "lab-pi",
      scope: PERSONAL,
      status: { state: "connected", workspaces: 1 },
      workspaces: [workspace("train", "lab", "main", ["shell"])],
      agentCallbacks: false,
      endedByRestart: [{ workspaceName: "train", tab: "watch", cwd: `${HOME}/src/lab` }],
    },
    {
      id: "old-vm",
      scope: PERSONAL,
      status: { state: "versionMismatch", serverVersion: "0.8.2", appNeeds: "1.0", sessions: 5 },
      workspaces: [workspace("legacy", "legacy", "main", ["shell", "claude", "logs", "db", "srv"])],
    },
    {
      id: "frozen-box",
      scope: PERSONAL,
      status: { state: "terminalServerFrozen", count: 2, replacementSupported: true },
      workspaces: [workspace("frozen", "legacy", "main", ["shell", "logs"])],
    },
    { id: "scratch-vm", scope: PERSONAL, status: { state: "notInstalled" } },
    { id: "staging-box", scope: PERSONAL, status: { state: "authFailed", reason: "key rejected" } },
    { id: "archive-box", scope: PERSONAL, status: { state: "idle" } },
  ];
}

/** A worktree's shape on every machine: numbers are file sizes in bytes. */
type Tree = { [name: string]: Tree | number };
const WORKTREE: Tree = {
  src: {
    commands: { "machines.rs": 5400, "mod.rs": 1200 },
    shared: { "codec.rs": 3100, "server.rs": 8200 },
    "main.rs": 900,
  },
  node_modules: { ".pnpm": {}, react: { "index.js": 300 } },
  "Cargo.toml": 1100,
  "README.md": 2400,
  "server.log": 2_400_000,
};

/** `a.rs` → `rs`; a dotfile (`.bashrc`), a trailing dot or no dot → null. */
function extensionOf(name: string): string | null {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot + 1) : null;
}

/** Directories whose listing is slow on a remote box (for the latency states). */
const SLOW_DIRS = new Set(["commands"]);
const VERY_SLOW_DIRS = new Set(["node_modules"]);

function resolveTree(snapshot: MachineLayoutSnapshot, absPath: string): Tree | number | null {
  const root = snapshot.workspaces.find(
    (w) => absPath === w.worktreePath || absPath.startsWith(`${w.worktreePath}/`),
  );
  if (!root) return null;
  const rel = absPath.slice(root.worktreePath.length).split("/").filter(Boolean);
  let node: Tree | number = WORKTREE;
  for (const seg of rel) {
    if (typeof node === "number" || !(seg in node)) return null;
    node = node[seg];
  }
  return node;
}

function fileContent(machine: string, path: string): string {
  const name = path.split("/").pop() ?? path;
  if (name.endsWith(".rs")) {
    return `// ${name} — on ${machine}\nuse std::io;\n\npub fn serve() -> io::Result<()> {\n    Ok(())\n}\n`;
  }
  if (name.endsWith(".log")) return `${new Date(0).toISOString()} INFO terminal server ready\n`;
  return `# ${name}\n\nRead from ${machine}:${path}\n`;
}

function delay<T>(ms: number, value: () => T): Promise<T> {
  if (ms <= 0) return Promise.resolve().then(value);
  return new Promise((resolve, reject) =>
    setTimeout(() => {
      try {
        resolve(value());
      } catch (e) {
        reject(e);
      }
    }, ms),
  );
}

function slug(label: string): string {
  return (
    label
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "machine"
  );
}

const VERSION = "1.0.0-mock";
const STAGING = `~/.codesense/host/.incoming-${VERSION}`;
const INSTALL_PLAN = {
  sizeBytes: 12 * 1024 * 1024,
  targetDir: `~/.codesense/host/${VERSION}`,
  commands: [
    `ssh <host> sh -c "mkdir ${STAGING}"`,
    `ssh <host> sh -c "cat > ${STAGING}/codesense-session-server" < codesense-session-server`,
    `ssh <host> sh -c "sha256sum -c && mv ${STAGING} ${VERSION} && ln -sfn ${VERSION} current"`,
  ],
};

export function machineHandlers(
  key: MachineFixtureKey,
  latency: MachineLatency = REALISTIC_LATENCY,
  nowFn: () => number = Date.now,
): HandlerRegistry {
  const canPublishTeam = key === "demo";
  const records = new Map<string, MachineRecord>(
    seedsFor(key, nowFn()).map((seed) => [seed.id, buildRecord(seed, canPublishTeam)]),
  );
  /** Hosts the terminal server was installed on this session (re-test finds it). */
  const installed = new Set<string>();
  /** Hosts whose first install attempt was interrupted already. */
  const interruptedOnce = new Set<string>();
  /** Keychain handles handed out (the passphrase itself never reaches the mock). */
  let passphraseHandles = 0;
  const replacementReservations = new Map<string, string>();
  let nextReplacementToken = 1;
  /** SW-AGR-19 PR-8: the mock's last Update all run. */
  let lastUpdateRun: TerminalServersUpdateRun | null = null;
  let nextUpdateRun = 0;
  /** SW-AGR-19 PR-7: stray terminal servers on This Mac (the demo has one). */
  const strays: StrayServer[] =
    key === "demo"
      ? [
          {
            pid: 51234,
            started: "Thu Sep 24 10:02:11 2026",
            exe: "/Users/me/codesense-wt/build-SW-RM-4/target/debug/codesense-session-server",
            root: "/private/tmp/replace-home-x7/.codesense",
            kind: "otherBuild",
            sessions: 2,
          },
        ]
      : [];

  const get = (machineId: unknown): MachineRecord => {
    const rec = records.get(String(machineId));
    if (!rec) throw new Error(`unknown machine: ${String(machineId)}`);
    return rec;
  };

  /** A remote machine only: This Mac is the registry's floor — it can't be
   *  disconnected, unshared, hidden or removed. */
  const getRemote = (machineId: unknown): MachineRecord => {
    const rec = get(machineId);
    if (rec.summary.kind === "local") throw new Error("This Mac can't be changed here");
    return rec;
  };

  const setStatus = (rec: MachineRecord, status: MachineSummary["status"]): MachineSummary => {
    rec.summary = { ...rec.summary, status };
    return rec.summary;
  };

  const connected = (rec: MachineRecord) =>
    setStatus(rec, { state: "connected", workspaces: rec.snapshot.workspaces.length });

  const testReport = (input: MachineDraftInput): MachineTestReport => {
    const host = input.host.trim().toLowerCase();
    const fp = fingerprintOf(host);
    const steps: MachineTestStep[] = [];
    const skipRest = (ids: MachineTestStep["id"][]) =>
      ids.forEach((id) => steps.push({ id, status: "skipped", detail: "", nextSteps: [] }));
    const report = (): MachineTestReport => ({
      steps,
      installPlan: steps.some((s) => s.id === "terminalServer" && s.status === "warn")
        ? INSTALL_PLAN
        : null,
      fileViewOnly: steps.some((s) => s.id === "platform" && s.status === "warn"),
    });

    if (host.includes("timeout")) {
      steps.push({
        id: "reachable",
        status: "fail",
        detail: `No response from ${host}:${input.port} after 10 s`,
        nextSteps: ["editAddress", "useJumpHost", "retry"],
      });
      skipRest(["hostKey", "auth", "platform", "dataDir", "terminalServer"]);
      return report();
    }
    steps.push({ id: "reachable", status: "ok", detail: "Reachable (38 ms)", nextSteps: [] });

    // A pinned machine whose host key changed presents the NEW key — still a
    // mismatch, still blocked: re-running the flow is never a way around it.
    const sameHost = (h: string | undefined) =>
      h?.toLowerCase().replace(/\.$/, "") === host.replace(/\.$/, "");
    const changed = [...records.values()].find(
      (r) => r.summary.status.state === "hostKeyChanged" && sameHost(r.summary.declaration?.host),
    );
    if (host.includes("mitm") || changed) {
      steps.push({
        id: "hostKey",
        status: "blocked",
        detail: "Host key doesn't match the key pinned for your team",
        nextSteps: [],
        fingerprint: fp,
      });
      skipRest(["auth", "platform", "dataDir", "terminalServer"]);
      return report();
    }
    const trusted = input.trustedHostKeyFp === fp;
    steps.push(
      trusted
        ? {
            id: "hostKey",
            status: "ok",
            detail: `Host key ${fp} trusted`,
            nextSteps: [],
            fingerprint: fp,
          }
        : {
            id: "hostKey",
            status: "needsTrust",
            detail: `First time seeing ${host}. Check this fingerprint before trusting it.`,
            nextSteps: [],
            fingerprint: fp,
          },
    );
    if (!trusted) {
      skipRest(["auth", "platform", "dataDir", "terminalServer"]);
      return report();
    }

    const user = input.username?.trim() || "dev";
    if (user === "root" || host.includes("denied")) {
      steps.push({
        id: "auth",
        status: "fail",
        detail: `Key rejected for ${user}@${host}`,
        nextSteps: ["chooseKey", "changeUser", "copyPublicKey"],
      });
      skipRest(["platform", "dataDir", "terminalServer"]);
      return report();
    }
    steps.push({ id: "auth", status: "ok", detail: `Signed in as ${user}`, nextSteps: [] });

    if (host.includes("bsd")) {
      steps.push({
        id: "platform",
        status: "warn",
        detail: "freebsd-amd64 can't run the terminal server yet — file view only (coming later)",
        nextSteps: [],
      });
      skipRest(["dataDir", "terminalServer"]);
      return report();
    }
    steps.push({ id: "platform", status: "ok", detail: "linux-x86_64", nextSteps: [] });

    if (host.includes("full")) {
      steps.push({
        id: "dataDir",
        status: "fail",
        detail: "~/.codesense isn't writable: disk full (0 B free on /home)",
        nextSteps: [],
      });
      skipRest(["terminalServer"]);
      return report();
    }
    steps.push({ id: "dataDir", status: "ok", detail: "~/.codesense is writable", nextSteps: [] });

    if (host.includes("gpu") || installed.has(host)) {
      steps.push({
        id: "terminalServer",
        status: "ok",
        detail: "Terminal server v1.0.0 found",
        nextSteps: [],
      });
    } else if (host.includes("old")) {
      steps.push({
        id: "terminalServer",
        status: "warn",
        detail: "Terminal server v0.8.2 found — this app needs v1",
        nextSteps: [],
      });
    } else {
      steps.push({
        id: "terminalServer",
        status: "warn",
        detail: "Terminal server not installed",
        nextSteps: [],
      });
    }
    return report();
  };

  return {
    list_machines: ({ orgSlug, team }): MachineSummary[] => {
      // Like the app: a connected machine reports its terminal server, any
      // other none.
      const all = [...records.values()].map((r) => {
        const { terminalServer, ...rest } = r.summary;
        return rest.status.state === "connected"
          ? {
              ...rest,
              ...mockMachineStats(),
              terminalServer: terminalServer ?? mockTerminalServer(rest.kind === "local"),
            }
          : rest;
      });
      const shared = all.filter(
        (m) => m.scope?.kind === "team" && m.scope.orgSlug === orgSlug && m.scope.team === team,
      );
      const personal = all.filter((m) => m.scope?.kind === "personal");
      const local = all.filter((m) => m.kind === "local");
      return [...local, ...shared, ...personal];
    },

    connect_machine: ({ machineId }) =>
      delay(latency.connectMs, () => {
        const rec = get(machineId);
        const s = rec.summary.status;
        if (s.state === "unreachable") {
          return setStatus(rec, {
            ...s,
            attempt: s.attempt + 1,
            retryInSecs: s.retryInSecs < 15 ? 15 : 60,
          });
        }
        if (s.state === "idle" || s.state === "reconnecting" || s.state === "connecting") {
          return connected(rec);
        }
        return rec.summary;
      }),

    disconnect_machine: ({ machineId }) => setStatus(getRemote(machineId), { state: "idle" }),

    // SW-AGR-19: a connected private-account server, or a frozen one with
    // proof, takes the safe replacement; any other gets the reinstall.
    // SW-AGR-19: no unfinished update in the mock; a reopen reports nothing back.
    // SW-AGR-19 PR-5: This Mac's update, when the mock's local server is older.
    local_terminal_server_update: () => {
      const rec = records.get(LOCAL_MACHINE_ID);
      const server = rec?.summary.terminalServer;
      if (!rec || !server || (server.relation !== "older" && server.relation !== "otherBuild"))
        return null;
      return {
        expected: { serverId: "srv-local", epoch: "1", pid: 49806, sessionIds: ["local-a"] },
        terminalServer: server,
      };
    },
    update_local_terminal_server: () =>
      delay(latency.connectMs, () => {
        const rec = records.get(LOCAL_MACHINE_ID)!;
        rec.summary = { ...rec.summary, terminalServer: mockTerminalServer(true) };
        return {
          sessions: 1,
          restored: {
            reopened: [{ name: "shell", workspace: "auth-fix" }],
            notReopened: [],
            recorded: true,
            complete: true,
            message: "This Mac: 1 tab reopened.",
          },
        };
      }),
    // SW-AGR-19 PR-7: a leaked dev-worktree server on a temp root, as the
    // story found on the user's Mac.
    list_stray_terminal_servers: () => strays.map((stray) => ({ ...stray })),
    stop_stray_terminal_server: ({ target }) => {
      const { pid, started } = target as { pid: number; started: string };
      const at = strays.findIndex((stray) => stray.pid === pid && stray.started === started);
      if (at < 0) return { status: "gone" };
      strays.splice(at, 1);
      return { status: "stopped", killed: false };
    },
    // SW-AGR-19 PR-8: the backend's Update all, settled at once in the mock:
    // each reviewed machine reaches this app's build.
    review_terminal_server_update: ({ machineId }) => {
      const rec = get(machineId);
      if (rec.summary.terminalServer?.relation !== "older")
        throw new Error("its terminal server is not out of date");
      const local = rec.summary.kind === "local";
      return local
        ? { serverId: "srv-local", epoch: "1", sessions: 1, pid: 49806, sessionIds: ["local-a"] }
        : (() => {
            const sessions =
              rec.summary.status.state === "connected"
                ? (rec.summary.status.terminalServerSessions ?? 0)
                : 0;
            return {
              serverId: `srv-${rec.summary.machineId}`,
              epoch: "1",
              sessions,
              sessionIds: Array.from({ length: sessions }, (_, i) => `remote-${i}`),
            };
          })();
    },
    start_terminal_servers_update: ({ items }) => {
      const reviewed = items as { machineId: string }[];
      const order = [
        ...reviewed.filter((i) => i.machineId !== LOCAL_MACHINE_ID),
        ...reviewed.filter((i) => i.machineId === LOCAL_MACHINE_ID),
      ];
      lastUpdateRun = {
        opId: `op-${(nextUpdateRun += 1)}`,
        running: false,
        machines: order.map((i) => i.machineId),
        outcomes: order.map(({ machineId }) => {
          const rec = get(machineId);
          const local = machineId === LOCAL_MACHINE_ID;
          rec.summary = { ...rec.summary, terminalServer: mockTerminalServer(local) };
          return {
            machineId,
            label: rec.summary.label,
            kind: "updated",
            message: `${rec.summary.label}: its tabs reopened.`,
          };
        }),
      };
      return lastUpdateRun;
    },
    // Like the backend: by its id any run, else only one still running.
    terminal_servers_update_status: ({ opId }) =>
      lastUpdateRun && (opId == null ? lastUpdateRun.running : lastUpdateRun.opId === opId)
        ? lastUpdateRun
        : null,
    machine_pending_tabs: () => null,
    reopen_machine_pending_tabs: () => ({
      reopened: [],
      notReopened: [],
      recorded: true,
      complete: true,
      message: "No tabs were waiting to reopen.",
    }),

    machine_terminal_server_update_mode: ({ machineId }) => {
      const rec = getRemote(machineId);
      const s = rec.summary.status;
      const plan = (mode: "replace" | "reinstall", sessions: number | null) => ({ mode, sessions });
      if (s.state === "terminalServerFrozen")
        return plan(s.replacementSupported ? "replace" : "reinstall", s.count);
      if (s.state === "replacementFailed") return plan("reinstall", null);
      if (s.state !== "connected") throw new Error("connect the machine first");
      // Like the backend: a private-account server that can hand over (here:
      // one not older than the app) takes the safe replacement.
      return plan(
        rec.summary.declaration?.mode === "private-account" &&
          rec.summary.terminalServer?.relation !== "older"
          ? "replace"
          : "reinstall",
        s.terminalServerSessions ?? 0,
      );
    },

    reinstall_machine_terminal_server: ({ machineId, confirmEndSessions }) =>
      delay(latency.connectMs, () => {
        if (confirmEndSessions !== true) throw new Error("confirmEndSessions must be true");
        const rec = getRemote(machineId);
        const s = rec.summary.status;
        const sessions =
          s.state === "connected"
            ? (s.terminalServerSessions ?? 0)
            : s.state === "terminalServerFrozen" || s.state === "replacementFailed"
              ? (s.count ?? 0)
              : 0;
        connected(rec);
        rec.summary = { ...rec.summary, terminalServer: mockTerminalServer(false) };
        return { status: "reinstalled", sessions };
      }),

    prepare_machine_terminal_server_replacement: ({ machineId }) => {
      const rec = getRemote(machineId);
      const s0 = rec.summary.status;
      if (s0.state === "connected" && rec.summary.declaration?.mode === "private-account") {
        const serverToken = `mock-replacement-${nextReplacementToken++}`;
        replacementReservations.set(rec.summary.machineId, serverToken);
        const count = s0.terminalServerSessions ?? null;
        rec.summary = { ...rec.summary, status: { state: "replacementReserved", count } };
        return { serverToken, count };
      }
      if (
        rec.summary.status.state !== "terminalServerFrozen" ||
        !rec.summary.status.replacementSupported
      ) {
        throw new Error("terminal server replacement is not safely available");
      }
      const count = rec.summary.status.count;
      const serverToken = `mock-replacement-${nextReplacementToken++}`;
      replacementReservations.set(rec.summary.machineId, serverToken);
      rec.summary = {
        ...rec.summary,
        status: { state: "replacementReserved", count },
      };
      return { serverToken, count };
    },

    restart_machine_terminal_server: ({ machineId, serverToken }) =>
      delay(latency.connectMs, () => {
        const rec = getRemote(machineId);
        const expected = replacementReservations.get(rec.summary.machineId);
        // Consume the reservation even when the caller presents the wrong token.
        replacementReservations.delete(rec.summary.machineId);
        if (!expected || expected !== serverToken) {
          if (rec.summary.status.state === "replacementReserved") {
            rec.summary = {
              ...rec.summary,
              status: {
                state: "terminalServerFrozen",
                count: rec.summary.status.count,
                replacementSupported: true,
              },
            };
          }
          throw new Error("invalid or stale replacement token");
        }
        const count =
          rec.summary.status.state === "replacementReserved" ? rec.summary.status.count : null;
        connected(rec);
        return { status: "replaced", count };
      }),

    get_machine_layout_snapshot: ({ machineId }) =>
      delay(latency.connectMs, () => {
        const rec = get(machineId);
        return {
          ...rec.snapshot,
          workspaces: rec.snapshot.workspaces.map((w) => ({ ...w, tabs: [...w.tabs] })),
        };
      }),

    open_machine_terminal: ({ machineId, cwd, command }) => {
      const rec = get(machineId);
      if (rec.summary.status.state !== "connected") {
        throw new Error(`${rec.summary.label} is not connected`);
      }
      // `~` is the machine's home, like a shell would read it.
      const dir = String(cwd).replace(/^~(?=\/|$)/, HOME);
      const tab = command ? String(command).split(" ")[0] : "shell";
      const ws = rec.snapshot.workspaces.find(
        (w) => dir === w.worktreePath || dir.startsWith(`${w.worktreePath}/`),
      );
      const home = rec.snapshot.workspaces.find((w) => w.worktreePath === HOME);
      const target = ws ?? home;
      if (target) {
        target.tabs.push(tab);
      } else {
        rec.snapshot.workspaces.push({
          workspaceId: `ws-home-${rec.snapshot.workspaces.length}`,
          name: "~",
          worktreePath: HOME,
          branch: null,
          tabs: [tab],
        });
        connected(rec);
      }
      return { tabId: `${machineId}:${tab}:${nowFn()}` };
    },

    // Every mock machine file reads; a test injects unreadable ones.
    list_unreadable_machine_files: (): UnreadableMachineFile[] => [],
    open_unreadable_machine_file: () => undefined,
    remove_unreadable_machine_file: () => undefined,

    list_ssh_sources: (): SshLocalSources => ({
      configHosts: [
        { alias: "devbox-02", hostName: "devbox-02.int", user: "dev", port: 22 },
        { alias: "bastion", hostName: "bastion.acme.io", user: "dev", port: 22 },
        { alias: "flaky-vm", hostName: "flaky-vm.int", user: "dev", port: 2222 },
        {
          alias: "gpu-behind",
          hostName: "192.0.2.12",
          user: "ubuntu",
          port: 22,
          proxyJump: "bastion.acme.io",
          identityFile: "~/.ssh/work_ed25519",
        },
        {
          alias: "tunnel-vm",
          hostName: "tunnel-vm.int",
          user: "dev",
          port: 22,
          proxyCommand: "cloudflared access ssh --hostname %h",
        },
      ],
      agentKeys: [
        {
          keyType: "ed25519",
          fingerprint: "SHA256:q3Zt…a91f",
          comment: "dev@mac",
          publicKey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockKeyForCodeSenseDevMock dev@mac",
          identityFile: "/Users/dev/.ssh/id_ed25519.pub",
        },
        {
          keyType: "ed25519",
          fingerprint: "SHA256:w0rk…77c2",
          comment: "dev@work",
          publicKey: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockWorkKeyForCodeSenseDevMock dev@work",
          identityFile: "/Users/dev/.codesense/agent-keys/5c1a.pub",
        },
      ],
    }),

    test_machine_connection: ({ input }) =>
      delay(latency.connectMs, () => testReport(input as MachineDraftInput)),

    enroll_private_machine_account: () =>
      delay(
        latency.connectMs,
        (): MachineTestReport => ({
          steps: [
            {
              id: "private-account",
              status: "warn",
              detail: "Private account enrolled",
              nextSteps: ["install-terminal-server"],
            },
          ],
          installPlan: null,
          fileViewOnly: false,
        }),
      ),

    install_terminal_server: ({ input }) =>
      delay(latency.connectMs, (): InstallTerminalServerResult => {
        const host = (input as MachineDraftInput).host.trim().toLowerCase();
        if (host.includes("flaky") && !interruptedOnce.has(host)) {
          interruptedOnce.add(host);
          return { ok: false, code: "interrupted", message: "Upload interrupted at 7.4 of 12 MB" };
        }
        installed.add(host);
        const logoutWarning = host.includes("logind")
          ? "This machine may end the terminal server when you log out. On the machine, run: " +
            "loginctl enable-linger dev && systemd-run --user --scope --quiet <relay>"
          : null;
        return { ok: true, version: "1.0.0", logoutWarning };
      }),

    capture_machine_passphrase: ({ keyPath }) => {
      // `cancel` in the key path plays the user dismissing the system dialog.
      if (String(keyPath).includes("cancel")) return null;
      passphraseHandles += 1;
      return { provider: "ssh", account: `mock-passphrase-${passphraseHandles}` };
    },

    save_machine: ({ machineId, input, share, connect }) => {
      const draft = input as MachineDraftInput;
      // A NEW machine never takes over an existing id (This Mac, a team machine…).
      let id = machineId ? String(machineId) : slug(draft.label || draft.host);
      if (!machineId) {
        const base = id;
        for (let n = 2; records.has(id); n++) id = `${base}-${n}`;
      }
      const existing = records.get(id);
      const host = draft.host.trim();
      const signIn = draft.signIn;
      // A member setting up a teammate's machine only writes their own binding:
      // the shared declaration (address, label, scope, pinned key) is unchanged.
      const keepDecl = existing !== undefined && !existing.summary.canPublish;
      const summary: MachineSummary = {
        machineId: id,
        kind: "ssh",
        label: keepDecl ? existing.summary.label : draft.label.trim() || host,
        scope: keepDecl ? existing.summary.scope : (share as MachineScope),
        declaration: keepDecl
          ? existing.summary.declaration
          : {
              host,
              port: draft.port,
              jumpHost: draft.jumpHost,
              defaultBaseDir: "~",
              pinnedHostKeyFp: draft.trustedHostKeyFp,
              pinnedServerId: connect ? `mock-server-${machineId}` : null,
            },
        binding: {
          username: draft.username,
          keyPath: signIn.method === "agent" ? null : signIn.keyPath,
          credentialRef: signIn.method === "keychainPassphrase" ? signIn.credentialRef : null,
          // Like the real registry: a personal machine connects at launch by
          // default once its setup completes; a team one when picked.
          autoConnect:
            existing?.summary.binding?.autoConnect ??
            (connect === true && (share as MachineScope).kind === "personal"),
          hidden: existing?.summary.binding?.hidden ?? false,
        },
        // Setting up a teammate's machine doesn't make you its publisher.
        canPublish: existing?.summary.canPublish ?? true,
        status: { state: "needsCredential" },
        attention: existing?.summary.attention ?? 0,
        capabilities: { agentCallbacks: true },
      };
      const rec: MachineRecord = existing ?? {
        summary,
        snapshot: {
          machineId: id,
          serverId: `srv-${id}`,
          epoch: 1,
          workspaces: [],
          endedByRestart: [],
        },
      };
      rec.summary = summary;
      // Like the real registry, a saved machine is known but not connected:
      // connecting is the connection manager's (`connect_machine`).
      if (connect) setStatus(rec, { state: "idle" });
      records.set(id, rec);
      return rec.summary;
    },

    set_machine_hidden: ({ machineId, hidden }) => {
      const rec = getRemote(machineId);
      const binding = rec.summary.binding ?? {
        username: null,
        keyPath: null,
        credentialRef: null,
        autoConnect: false,
        hidden: false,
      };
      rec.summary = { ...rec.summary, binding: { ...binding, hidden: Boolean(hidden) } };
    },

    set_machine_auto_connect: ({ machineId, autoConnect }) => {
      const rec = getRemote(machineId);
      const binding = rec.summary.binding;
      if (!binding || rec.summary.status.state === "needsCredential")
        throw new Error("Set up this machine before changing auto-connect");
      rec.summary = {
        ...rec.summary,
        binding: { ...binding, autoConnect: Boolean(autoConnect) },
      };
    },

    unshare_machine: ({ machineId }) => {
      const rec = getRemote(machineId);
      rec.summary = { ...rec.summary, scope: PERSONAL, canPublish: true };
      return rec.summary;
    },

    remove_machine: ({ machineId }) => {
      getRemote(machineId);
      records.delete(String(machineId));
    },

    // The mock's legacy machines leave no shared-account server behind.
    retire_legacy_terminal_server: ({ machineId }) => {
      getRemote(machineId);
      return { status: "notRunning" };
    },

    machine_fs_list: ({ machineId, dirPath }) => {
      const rec = get(machineId);
      const dir = String(dirPath);
      const name = dir.split("/").pop() ?? "";
      const ms = VERY_SLOW_DIRS.has(name)
        ? latency.verySlowListMs
        : SLOW_DIRS.has(name)
          ? latency.slowListMs
          : latency.listMs;
      return delay(ms, (): FileEntry[] => {
        if (rec.summary.status.state !== "connected") {
          throw new Error(`${rec.summary.label} is not connected`);
        }
        const node = resolveTree(rec.snapshot, dir);
        if (node === null || typeof node === "number") throw new Error(`not a directory: ${dir}`);
        return Object.entries(node)
          .sort(([a, av], [b, bv]) =>
            typeof av === typeof bv ? a.localeCompare(b) : typeof av === "number" ? 1 : -1,
          )
          .map(([entryName, v]) => ({
            name: entryName,
            path: `${dir}/${entryName}`,
            is_dir: typeof v !== "number",
            extension: typeof v === "number" ? extensionOf(entryName) : null,
            size: typeof v === "number" ? v : null,
          }));
      });
    },

    machine_fs_read: ({ machineId, path, maxBytes }) => {
      const rec = get(machineId);
      return delay(latency.listMs, (): MachineFileRead => {
        if (rec.summary.status.state !== "connected") {
          throw new Error(`${rec.summary.label} is not connected`);
        }
        const node = resolveTree(rec.snapshot, String(path));
        if (typeof node !== "number") throw new Error(`not a file: ${String(path)}`);
        const cap =
          maxBytes == null || !Number.isFinite(Number(maxBytes))
            ? Infinity
            : Math.max(0, Math.floor(Number(maxBytes)));
        const base = fileContent(rec.summary.label, String(path));
        // Only a log is big enough to fill its reported size (the size-gate demo).
        const full = String(path).endsWith(".log")
          ? base.repeat(Math.ceil(Math.min(node, cap) / base.length))
          : base;
        return { content: full.slice(0, cap), size: node, truncated: node > cap };
      });
    },
  };
}

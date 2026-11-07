// SPDX-License-Identifier: MIT
/**
 * The unit tests' terminal host: every test file's
 * `cli-workspace/transport/terminal-layout-source` is this module (see
 * `src/test-setup.ts`), serving the layout verbs from an in-memory
 * {@link MockTerminalHost} instead of Tauri. A verb applies at once and its
 * announcement goes to the sink a test installs (`installTerminalLayoutHarness`
 * routes it into the viewer store), so a store action's tab is visible as soon
 * as the action returns — as the real host's push makes it visible.
 */

import { MockTerminalHost } from "@/app/mock/terminal-layout-host";
import type {
  RuntimeLayoutReply,
  RuntimeMemberTarget,
  RuntimeSessionReference,
  RuntimeWorkspaceReference,
  TerminalLayoutChanged,
} from "@/tauri/commands";

let sink: ((change: TerminalLayoutChanged) => void) | null = null;
let lifetimes = 0;

function newHost(): MockTerminalHost {
  lifetimes += 1;
  return new MockTerminalHost((change) => sink?.(change), `test-epoch-${lifetimes}`);
}

/** The current test host. */
export let testTerminalHost = newHost();

/** Start a fresh host lifetime whose announcements go to `next`. */
export function resetTestTerminalHost(next: ((change: TerminalLayoutChanged) => void) | null) {
  sink = next;
  testTerminalHost = newHost();
  return testTerminalHost;
}

/**
 * A remote Machine's terminal server, as the app's connection to it serves it:
 * a host admitted as the Machine's `remote` member under a private authority.
 * While it is offline the app answers its last layout, frozen.
 */
export class TestRemoteHost extends MockTerminalHost {
  readonly machineId: string;
  readonly privateAuthorityEpoch: string;
  /** The app's connection to it: each reconnect is a new generation. */
  generation = 1;
  admissionRevision = 1;
  online = true;

  constructor(
    onChange: (change: TerminalLayoutChanged) => void,
    machineId = "box",
    privateAuthorityEpoch = "7",
  ) {
    super(onChange, `${machineId}-epoch`);
    this.machineId = machineId;
    this.privateAuthorityEpoch = privateAuthorityEpoch;
  }

  override target(): RuntimeMemberTarget {
    return {
      member: { machineId: this.machineId, slot: "remote" },
      admissionRevision: String(this.admissionRevision),
      namespace: {
        serverId: this.serverId,
        epoch: this.epoch,
        generation: String(this.generation),
      },
    };
  }

  override reply(): RuntimeLayoutReply {
    if (this.online) {
      return {
        mode: "live",
        ...this.target(),
        snapshot: this.snapshot(),
        privateAuthorityEpoch: this.privateAuthorityEpoch,
      };
    }
    return {
      mode: "historical",
      member: { machineId: this.machineId, slot: "remote" },
      admissionRevision: String(this.admissionRevision),
      privateAuthorityEpoch: this.privateAuthorityEpoch,
      frozen: {
        lastNamespace: null,
        serverId: this.serverId,
        epoch: this.epoch,
        snapshot: this.snapshot(),
        reason: "unreachable",
      },
    };
  }
}

/** The remote Machine the verbs reach by its admitted target, if any. */
export let testRemoteHost: TestRemoteHost | null = null;

export function setTestRemoteHost(host: TestRemoteHost | null): void {
  testRemoteHost = host;
}

/** The host a reference reaches, and the bare id there. */
function route(reference: RuntimeSessionReference): [MockTerminalHost, string] {
  if (typeof reference === "string") return [testTerminalHost, reference];
  if (reference.member.slot === "remote") {
    if (!testRemoteHost?.online) throw new Error("remote runtime unavailable");
    return [testRemoteHost, reference.session];
  }
  return [testTerminalHost, reference.session];
}

function routeWorkspace(reference: RuntimeWorkspaceReference): [MockTerminalHost, string] {
  if (typeof reference === "string") return [testTerminalHost, reference];
  if (reference.member.slot === "remote") {
    if (!testRemoteHost?.online) throw new Error("remote runtime unavailable");
    return [testRemoteHost, reference.workspace];
  }
  return [testTerminalHost, reference.workspace];
}

function answer<T>(verb: () => T): Promise<T> {
  try {
    return Promise.resolve(verb());
  } catch (e) {
    return Promise.reject(e instanceof Error ? e.message : e);
  }
}

type Rest<F> = F extends (first: never, ...rest: infer R) => unknown ? R : never;

export const terminalLayoutSourceMock = {
  terminalLayoutMemberSnapshots: () =>
    answer(() => [testTerminalHost.reply(), ...(testRemoteHost ? [testRemoteHost.reply()] : [])]),
  terminalOpenTab: (...[request]: Parameters<MockTerminalHost["openTab"]>) =>
    answer(() => testTerminalHost.openTab(request)),
  terminalRespawnTab: (...args: Parameters<MockTerminalHost["respawnTab"]>) =>
    answer(() => testTerminalHost.respawnTab(...args)),
  terminalCloseTab: (
    session: RuntimeSessionReference,
    ...rest: Rest<MockTerminalHost["closeTab"]>
  ) =>
    answer(() => {
      const [host, id] = route(session);
      return host.closeTab(id, ...rest);
    }),
  terminalPlaceTab: (
    session: RuntimeSessionReference,
    ...rest: Rest<MockTerminalHost["placeTab"]>
  ) =>
    answer(() => {
      const [host, id] = route(session);
      return host.placeTab(id, ...rest);
    }),
  terminalRenameTab: (
    session: RuntimeSessionReference,
    ...rest: Rest<MockTerminalHost["renameTab"]>
  ) =>
    answer(() => {
      const [host, id] = route(session);
      return host.renameTab(id, ...rest);
    }),
  terminalMoveTab: (session: RuntimeSessionReference, ...rest: Rest<MockTerminalHost["moveTab"]>) =>
    answer(() => {
      const [host, id] = route(session);
      return host.moveTab(id, ...rest);
    }),
  terminalSetActive: (
    workspace: RuntimeWorkspaceReference,
    session: RuntimeSessionReference | null,
  ) =>
    answer(() => {
      const [host, key] = routeWorkspace(workspace);
      return host.setActive(key, session === null ? null : route(session)[1]);
    }),
  terminalSetGrid: (
    workspace: RuntimeWorkspaceReference,
    ...rest: Rest<MockTerminalHost["setGrid"]>
  ) =>
    answer(() => {
      const [host, key] = routeWorkspace(workspace);
      return host.setGrid(key, ...rest);
    }),
  terminalSetTags: (session: RuntimeSessionReference, ...rest: Rest<MockTerminalHost["setTags"]>) =>
    answer(() => {
      const [host, id] = route(session);
      return host.setTags(id, ...rest);
    }),
  onTerminalLayoutChanged: () => Promise.resolve(() => {}),
  onTerminalAdmissionChanged: () => Promise.resolve(() => {}),
  onTerminalLayoutResync: () => Promise.resolve(() => {}),
};

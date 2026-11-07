// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

// Integration smoke test for the editor buffer: the real hook and IO runner
// over the buffer state machine (buffer-machine.ts).
// A seeded random schedule interleaves typing, saves, agent writes, watcher
// events, size relists, root moves, unmount/remount through a kept draft,
// reloads and merges, and resolves every read and write out of order. After
// each step the ownership rules must hold:
//
//  1. Unsaved text is never lost: until the user's latest text has been
//     written, or they explicitly discarded it, it is the buffer.
//  2. No write lands over a disk version the editor never showed the user.
//  3. At rest, a clean editor shows disk, and a dirty one that diverged from
//     disk is under the conflict guard.

const readFileMock = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => readFileMock(...args),
  WORKTREE_WRITE_CONFLICT_PREFIX: "conflict:",
}));

import { sha256Hex, useFileEditor, type FileDraft } from "./use-file-editor";

afterEach(() => {
  readFileMock.mockReset();
});

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

type Mode = "local" | "remote";

interface Props {
  path: string;
  size: number;
  nonce: number;
}

const ROOTS = ["/wtA", "/wtB"];
const hashOf = (s: string) => `h:${s}`;

async function settle() {
  await act(async () => {
    for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0));
  });
}

const ACTIONS = [
  "type",
  "type",
  "save",
  "agent",
  "notify",
  "resolve",
  "resolve",
  "resolve",
  "relist",
  "huge",
  "retarget",
  "remount",
  "reload",
  "merge",
] as const;
type Action = (typeof ACTIONS)[number] | "resolve:read" | "resolve:read-last" | "resolve:write";

/** Run a random schedule (from `seed`), or a scripted one. Returns the
 *  invariant failures, with the schedule that produced them. */
async function runSchedule(
  seed: number,
  mode: Mode,
  steps: number,
  script?: readonly Action[],
): Promise<string[]> {
  const r = rng(seed);
  const pick = <T>(xs: readonly T[]) => xs[Math.floor(r() * xs.length)] as T;
  const log: string[] = [];

  // The one file on disk (reached through either root), and every version it
  // has held since our last write landed.
  let disk = "v0\n";
  let sinceOurWrite = new Set<string>([disk]);
  // Disk versions in the order they appeared (every text is unique).
  const order = new Map<string, number>([[disk, 0]]);
  let newestCleanShown = 0;
  // Every disk version the editor showed (adopted, merged, or wrote).
  const shown = new Set<string>([]);
  const pending: { kind: "read" | "write"; label: string; run: () => Promise<void> | void }[] = [];
  let counter = 0;

  // The user's latest text and whether it is safe (written, or discarded).
  let userText: string | null = null;
  let userSafe = true;
  const failures: string[] = [];

  const read = (path: string) =>
    new Promise<string>((resolve) => {
      const snap = disk;
      pending.push({
        kind: "read",
        label: `read ${path} -> ${JSON.stringify(snap)}`,
        run: () => resolve(snap),
      });
    });

  const applyWrite = async (content: string, expected: string | undefined) => {
    const current = disk;
    const ok =
      mode === "remote"
        ? expected === hashOf(current)
        : expected === undefined || expected === (await sha256Hex(current));
    if (!ok) return false;
    if (!shown.has(current)) {
      failures.push(
        `write of ${JSON.stringify(content)} clobbered unseen ${JSON.stringify(current)}`,
      );
    }
    disk = content;
    order.set(content, order.size);
    sinceOurWrite = new Set([content]);
    shown.add(content);
    if (content === userText) userSafe = true;
    return true;
  };

  const write = vi.fn(
    (_path: string, content: string, expected?: string) =>
      new Promise<void | { hash: string }>((resolve, reject) => {
        pending.push({
          kind: "write",
          label: `write ${JSON.stringify(content)} expect ${expected ?? "-"}`,
          run: async () => {
            const ok = await applyWrite(content, expected);
            if (ok) resolve(mode === "remote" ? { hash: hashOf(content) } : undefined);
            else if (mode === "remote")
              reject({ kind: "conflict", observation: { hash: hashOf(disk) } });
            else reject(new Error("conflict: changed on disk"));
          },
        });
      }),
  );

  readFileMock.mockImplementation((path: string) => read(path));
  const remote =
    mode === "remote"
      ? {
          readWithHash: (path: string) =>
            read(path).then((content) => ({ content, hash: hashOf(content) })),
        }
      : undefined;

  let props: Props = { path: `${ROOTS[0]}/a.ts`, size: 3, nonce: 0 };
  let restore: FileDraft | null = null;
  const mount = () =>
    renderHook(
      (p: Props) =>
        useFileEditor({ path: p.path, size: p.size }, p.nonce, {
          write,
          remote,
          restore,
          pollMs: 0,
        }),
      { initialProps: props },
    );
  let hook = mount();

  const record = () => {
    const c = hook.result.current;
    if (c.state.kind === "editable") shown.add(c.diskText);
    if (c.reconciling) shown.add(c.reconcileDisk);
  };
  const check = (where: string) => {
    const c = hook.result.current;
    if (!userSafe && userText != null && c.buffer !== userText) {
      failures.push(
        `${where}: buffer ${JSON.stringify(c.buffer)} lost ${JSON.stringify(userText)}`,
      );
    }
    // A clean editor never rolls back to a version from before our own write,
    // nor to one older than a version it already showed clean.
    if (c.state.kind === "editable" && !c.dirty && !c.stale) {
      if (!sinceOurWrite.has(c.buffer)) {
        failures.push(`${where}: clean buffer ${JSON.stringify(c.buffer)} predates our last write`);
      }
      const at = order.get(c.buffer) ?? -1;
      if (at < newestCleanShown) {
        failures.push(`${where}: clean buffer rolled back to ${JSON.stringify(c.buffer)}`);
      }
      newestCleanShown = Math.max(newestCleanShown, at);
    }
  };

  const runPending = async (index: number) => {
    const [op] = pending.splice(index, 1);
    if (!op) return;
    log.push(`  resolve ${op.label}`);
    await act(async () => {
      await op.run();
    });
  };

  await settle();
  const total = script ? script.length : steps;
  for (let step = 0; step < total && failures.length === 0; step++) {
    const c = hook.result.current;
    const action: Action = script ? (script[step] as Action) : pick(ACTIONS);
    log.push(`${step}: ${action}`);
    switch (action) {
      case "type":
        if (c.state.kind === "editable" && !c.reconciling) {
          userText = `u${++counter}\n`;
          userSafe = false;
          act(() => c.setBuffer(userText as string));
        }
        break;
      case "save":
        if (c.state.kind === "editable" && !c.reconciling) void act(() => void c.save(c.buffer));
        break;
      case "agent":
        disk = `agent${++counter}\n`;
        order.set(disk, order.size);
        sinceOurWrite.add(disk);
        break;
      case "notify":
        props = { ...props, nonce: props.nonce + 1 };
        hook.rerender(props);
        break;
      case "resolve":
        if (pending.length > 0) await runPending(Math.floor(r() * pending.length));
        break;
      case "resolve:read":
      case "resolve:write": {
        const kind = action === "resolve:read" ? "read" : "write";
        const i = pending.findIndex((op) => op.kind === kind);
        if (i >= 0) await runPending(i);
        break;
      }
      case "resolve:read-last": {
        const i = pending.map((op) => op.kind).lastIndexOf("read");
        if (i >= 0) await runPending(i);
        break;
      }
      case "relist":
        props = { ...props, size: (props.size % 1000) + 1 };
        hook.rerender(props);
        break;
      case "huge":
        // The tree lists the file past the view limit (it grew).
        props = { ...props, size: 50_000_000 };
        hook.rerender(props);
        break;
      case "retarget": {
        const other = props.path.startsWith(ROOTS[0] as string) ? ROOTS[1] : ROOTS[0];
        props = { ...props, path: `${other}/a.ts` };
        hook.rerender(props);
        break;
      }
      case "remount": {
        // The pane's handoff: a dirty draft goes to the store, then comes back.
        const d = c.getDraft();
        restore = d.dirty ? { buffer: d.buffer, lastSaved: d.lastSaved } : null;
        hook.unmount();
        hook = mount();
        break;
      }
      case "reload":
        if (c.showStaleBanner && !c.reconciling) {
          userSafe = true; // the user chose to discard their edits
          act(() => c.reloadFromDisk());
        }
        break;
      case "merge":
        if (c.reconciling) {
          if (r() < 0.5) {
            userSafe = true; // take theirs: an explicit discard
            act(() => c.takeTheirs());
          } else {
            void act(() => void c.resolveSave(c.buffer));
          }
        } else if (c.stale) {
          act(() => c.startReconcile());
        }
        break;
    }
    await settle();
    record();
    check(`step ${step} (${action})`);
  }

  // Quiesce: answer everything, tell the editor disk may have moved, answer again.
  for (let round = 0; round < 4 && failures.length === 0; round++) {
    while (pending.length > 0) await runPending(0);
    await settle();
    record();
    check(`quiesce ${round}`);
    props = { ...props, nonce: props.nonce + 1 };
    hook.rerender(props);
    await settle();
  }
  while (pending.length > 0) await runPending(0);
  await settle();
  check("rest");

  const c = hook.result.current;
  if (failures.length === 0 && c.state.kind === "editable" && !c.reconciling) {
    if (!c.dirty && !c.stale && c.buffer !== disk) {
      failures.push(
        `at rest a clean buffer ${JSON.stringify(c.buffer)} != disk ${JSON.stringify(disk)}`,
      );
    }
    if (c.stale && c.buffer === disk) {
      failures.push(`at rest a false conflict: the guard is up but disk equals the buffer`);
    }
    if (c.dirty && c.diskText !== disk && !c.stale) {
      failures.push(`at rest a dirty buffer diverged from disk ${JSON.stringify(disk)} unguarded`);
    }
  }
  hook.unmount();
  return failures.length > 0 ? [...failures, "schedule:", ...log] : [];
}

// The scripted per-bug orderings from earlier review rounds are covered by the
// buffer machine's exhaustive model check (buffer-machine.test.ts). This file
// stays as an integration smoke test of the real hook and IO runner.

describe("useFileEditor — race orderings (seeded property test)", () => {
  for (const mode of ["remote", "local"] as const) {
    it(`${mode}: no ordering loses unsaved text, clobbers unseen disk, or rests inconsistent`, async () => {
      const seeds = Number(process.env.RACE_SEEDS ?? 30);
      for (let seed = 1; seed <= seeds; seed++) {
        const failures = await runSchedule(seed * 7919 + (mode === "local" ? 1 : 0), mode, 40);
        expect(failures, `seed ${seed}\n${failures.join("\n")}`).toEqual([]);
      }
    }, 120_000);
  }
});

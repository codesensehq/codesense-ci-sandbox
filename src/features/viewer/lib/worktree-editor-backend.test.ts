// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The remote side of the worktree editor: path translation to the machine's
// root-relative wire, base64 round-trips, strict UTF-8, the optimistic-lock
// hash threading, and the graceful "older host" answers.

const cmd = {
  sourceRemoteListDirectory: vi.fn(),
  sourceRemoteListFiles: vi.fn(),
  sourceRemoteFileHead: vi.fn(),
  sourceRemoteFileRead: vi.fn(),
  sourceRemoteFileWrite: vi.fn(),
  sourceRemoteFileInspect: vi.fn(),
  worktreeListFiles: vi.fn(),
  worktreeFileHead: vi.fn(),
  writeWorktreeFile: vi.fn(),
};
vi.mock("@/tauri/commands", () => ({
  sourceRemoteListDirectory: (...a: unknown[]) => cmd.sourceRemoteListDirectory(...a),
  sourceRemoteListFiles: (...a: unknown[]) => cmd.sourceRemoteListFiles(...a),
  sourceRemoteFileHead: (...a: unknown[]) => cmd.sourceRemoteFileHead(...a),
  sourceRemoteFileRead: (...a: unknown[]) => cmd.sourceRemoteFileRead(...a),
  sourceRemoteFileWrite: (...a: unknown[]) => cmd.sourceRemoteFileWrite(...a),
  sourceRemoteFileInspect: (...a: unknown[]) => cmd.sourceRemoteFileInspect(...a),
  worktreeListFiles: (...a: unknown[]) => cmd.worktreeListFiles(...a),
  worktreeFileHead: (...a: unknown[]) => cmd.worktreeFileHead(...a),
  writeWorktreeFile: (...a: unknown[]) => cmd.writeWorktreeFile(...a),
}));

import {
  backendFor,
  base64DecodeToBytes,
  base64EncodeBytes,
  decodeUtf8Strict,
  makeLocalBackend,
  makeRemoteBackend,
} from "./worktree-editor-backend";

const b64 = (text: string) => base64EncodeBytes(new TextEncoder().encode(text));

beforeEach(() => {
  for (const fn of Object.values(cmd)) fn.mockReset();
});

describe("remote backend", () => {
  const backend = makeRemoteBackend("m1", "h1", "/home/dev/work/", "feature/x");

  it("lists a directory through root-relative paths and re-anchors the entries", async () => {
    cmd.sourceRemoteListDirectory.mockResolvedValue([
      { name: "a.ts", path: "src/a.ts", is_dir: false, extension: "ts", size: 3 },
    ]);
    const root = await backend.listDirectory!("/home/dev/work");
    expect(cmd.sourceRemoteListDirectory).toHaveBeenLastCalledWith("m1", "h1", "");
    await backend.listDirectory!("/home/dev/work/src");
    expect(cmd.sourceRemoteListDirectory).toHaveBeenLastCalledWith("m1", "h1", "src");
    expect(root[0]?.path).toBe("/home/dev/work/src/a.ts");
  });

  it("reads text with its hash and writes base64 with the expected hash", async () => {
    cmd.sourceRemoteFileRead.mockResolvedValue({
      base64: b64("héllo\n"),
      observation: { hash: "h0", mtimeNs: 0, size: 7, mode: 420 },
      truncated: false,
    });
    const read = await backend.remote!.readWithHash("/home/dev/work/src/a.ts");
    expect(cmd.sourceRemoteFileRead).toHaveBeenCalledWith("m1", "h1", "src/a.ts");
    expect(read).toEqual({ content: "héllo\n", hash: "h0" });

    cmd.sourceRemoteFileWrite.mockResolvedValue({ observation: { hash: "h1" } });
    await expect(backend.write("/home/dev/work/src/a.ts", "ünïcode", "h0")).resolves.toEqual({
      hash: "h1",
    });
    const [, , rel, payload, expected] = cmd.sourceRemoteFileWrite.mock.calls[0]!;
    expect(rel).toBe("src/a.ts");
    expect(new TextDecoder().decode(base64DecodeToBytes(payload as string))).toBe("ünïcode");
    expect(expected).toBe("h0");
  });

  it("refuses a path outside the worktree instead of sending it to the machine as-is", async () => {
    await expect(backend.remote!.readWithHash("/etc/passwd")).rejects.toMatchObject({
      kind: "transportFailure",
    });
    await expect(backend.write("/home/dev/workother/a.ts", "x")).rejects.toMatchObject({
      kind: "transportFailure",
    });
    expect(cmd.sourceRemoteFileRead).not.toHaveBeenCalled();
    expect(cmd.sourceRemoteFileWrite).not.toHaveBeenCalled();
  });

  it("refuses to decode non-UTF-8 bytes (no lossy round trip)", async () => {
    cmd.sourceRemoteFileRead.mockResolvedValue({
      base64: base64EncodeBytes(new Uint8Array([0x66, 0x6f, 0xff, 0xfe])),
      observation: { hash: "h0", mtimeNs: 0, size: 4, mode: 420 },
      truncated: false,
    });
    await expect(backend.remote!.readWithHash("/home/dev/work/latin1.txt")).rejects.toEqual({
      kind: "notUtf8",
      hash: "h0",
    });
    expect(() => decodeUtf8Strict(new Uint8Array([0xc3]))).toThrow();
  });

  it("refuses a truncated read as too large rather than opening part of the file", async () => {
    cmd.sourceRemoteFileRead.mockResolvedValue({
      base64: b64("partial"),
      observation: { hash: "hx", size: 9_000_000 },
      truncated: true,
    });
    await expect(backend.remote!.readWithHash("/home/dev/work/big.log")).rejects.toEqual({
      kind: "boundsExceeded",
      hash: "hx",
    });
  });

  it("polls a file's hash through inspect", async () => {
    cmd.sourceRemoteFileInspect.mockResolvedValue({ hash: "abc", mtimeNs: 1, size: 1, mode: 420 });
    await expect(backend.remote!.inspectHash!("/home/dev/work/a")).resolves.toBe("abc");
    expect(cmd.sourceRemoteFileInspect).toHaveBeenCalledWith("m1", "h1", "a");
  });

  it("answers the committed baseline: text, new file, or unsupported host", async () => {
    cmd.sourceRemoteFileHead.mockResolvedValueOnce({ base64: b64("base\n"), supported: true });
    await expect(backend.readHead("/home/dev/work/a")).resolves.toBe("base\n");
    cmd.sourceRemoteFileHead.mockResolvedValueOnce({ base64: null, supported: true });
    await expect(backend.readHead("/home/dev/work/new")).resolves.toBeNull();
    cmd.sourceRemoteFileHead.mockResolvedValueOnce({ base64: null, supported: false });
    await expect(backend.readHead("/home/dev/work/a")).resolves.toBeUndefined();
  });

  it("carries the workspace's branch into the file list", async () => {
    cmd.sourceRemoteListFiles.mockResolvedValue({
      paths: ["a"],
      truncated: false,
      includesUntracked: false,
    });
    await expect(backend.listFiles()).resolves.toEqual({
      paths: ["a"],
      truncated: false,
      includesUntracked: false,
      branch: "feature/x",
    });
  });
});

describe("local backend", () => {
  it("routes to the local commands and hashes save baselines", async () => {
    const backend = makeLocalBackend("/wt/");
    expect(backend.location).toBe("local");
    expect(backend.listDirectory).toBeUndefined();
    cmd.writeWorktreeFile.mockResolvedValue(undefined);
    await backend.write("/wt/a.ts", "x", "deadbeef");
    expect(cmd.writeWorktreeFile).toHaveBeenCalledWith("/wt/a.ts", "x", "deadbeef");
    cmd.worktreeListFiles.mockResolvedValue({ paths: ["a.ts"], truncated: false, branch: "main" });
    await expect(backend.listFiles()).resolves.toMatchObject({
      includesUntracked: true,
      branch: "main",
    });
  });
});

describe("BOM fidelity", () => {
  it("keeps a UTF-8 BOM through a remote read and the next save", async () => {
    const backend = makeRemoteBackend("m1", "h1", "/w", null);
    const withBom = new Uint8Array([0xef, 0xbb, 0xbf, 0x61, 0x0a]);
    cmd.sourceRemoteFileRead.mockResolvedValue({
      base64: base64EncodeBytes(withBom),
      observation: { hash: "h0", mtimeNs: 0, size: 5, mode: 420 },
      truncated: false,
    });
    const read = await backend.remote!.readWithHash("/w/a.txt");
    expect(read.content).toBe("\ufeffa\n");
    cmd.sourceRemoteFileWrite.mockResolvedValue({ observation: { hash: "h1" } });
    await backend.write("/w/a.txt", read.content, "h0");
    const payload = cmd.sourceRemoteFileWrite.mock.calls[0]![3] as string;
    expect(Array.from(base64DecodeToBytes(payload))).toEqual(Array.from(withBom));
  });
});

describe("backendFor", () => {
  const base = { machineId: null, handle: null, remoteRoot: "", branch: null, cwd: "/wt" };
  it("never serves a remote workspace from this Mac", () => {
    expect(backendFor({ ...base, kind: "remote", machineId: "m1", handle: "" })).toBeNull();
    expect(backendFor({ ...base, kind: "remote", machineId: "", handle: "h1" })).toBeNull();
    expect(
      backendFor({ ...base, kind: "remote", machineId: "m1", handle: "h1", remoteRoot: "/r" })
        ?.location,
    ).toBe("remote");
  });
  it("has no remote backend before the machine reports the worktree's directory", () => {
    expect(
      backendFor({ ...base, kind: "remote", machineId: "m1", handle: "h1", remoteRoot: "" }),
    ).toBeNull();
  });
  it("serves a remote worktree whose directory is the filesystem root", async () => {
    const backend = backendFor({
      ...base,
      kind: "remote",
      machineId: "m1",
      handle: "h1",
      remoteRoot: "/",
    });
    expect(backend?.location).toBe("remote");
    expect(backend?.rootPath).toBe("");
    cmd.sourceRemoteFileWrite.mockResolvedValue({ observation: { hash: "h1" } });
    await backend!.write("/src/a.ts", "x", "h0");
    expect(cmd.sourceRemoteFileWrite.mock.calls.at(-1)![2]).toBe("src/a.ts");
  });
  it("needs a cwd for a local workspace, and nothing for an unminted one", () => {
    expect(backendFor({ ...base, kind: null })).toBeNull();
    expect(backendFor({ ...base, kind: "local", cwd: "" })).toBeNull();
    expect(backendFor({ ...base, kind: "local" })?.location).toBe("local");
  });
});

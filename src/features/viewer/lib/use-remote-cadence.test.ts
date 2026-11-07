// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";
import { renderHook, act } from "@testing-library/react";

import type { SourceRepo } from "@/features/git-sync/lib/source-redline-source";

import {
  CADENCE_PRESETS,
  LARGE_REPO_THRESHOLD,
  __resetRemoteCadence,
  pickCadencePreset,
  setRemoteCadenceOverride,
  setRemoteTrackedCount,
  useRemoteCadence,
  __inspectRemoteCadence,
} from "./use-remote-cadence";

const localRepo: SourceRepo = { kind: "local", cwd: "/wt" };
const smallRemote: SourceRepo = { kind: "remote", machineId: "m-1", handle: "h-1" };

describe("useRemoteCadence — preset selection", () => {
  beforeEach(() => {
    __resetRemoteCadence();
  });

  it("local always resolves to the localNative preset (no override, no tracked)", () => {
    expect(pickCadencePreset(localRepo, null)).toEqual(CADENCE_PRESETS.localNative);
    expect(pickCadencePreset(localRepo, 50_000)).toEqual(CADENCE_PRESETS.localNative);
  });

  it("remote with no tracked count defaults to remoteSmall", () => {
    expect(pickCadencePreset(smallRemote, null)).toEqual(CADENCE_PRESETS.remoteSmall);
  });

  it("remote under threshold stays remoteSmall", () => {
    expect(pickCadencePreset(smallRemote, LARGE_REPO_THRESHOLD - 1)).toEqual(
      CADENCE_PRESETS.remoteSmall,
    );
  });

  it("remote at threshold stays small (strict > comparison)", () => {
    expect(pickCadencePreset(smallRemote, LARGE_REPO_THRESHOLD)).toEqual(
      CADENCE_PRESETS.remoteSmall,
    );
  });

  it("remote OVER threshold loosens to remoteLarge", () => {
    expect(pickCadencePreset(smallRemote, LARGE_REPO_THRESHOLD + 1)).toEqual(
      CADENCE_PRESETS.remoteLarge,
    );
  });
});

describe("useRemoteCadence — override + hook reactivity", () => {
  beforeEach(() => {
    __resetRemoteCadence();
  });

  it("useRemoteCadence returns the preset for a remote repo without override", () => {
    const { result } = renderHook(() => useRemoteCadence(smallRemote));
    expect(result.current).toEqual(CADENCE_PRESETS.remoteSmall);
  });

  it("picks remoteLarge after a tracked count probe exceeds the threshold", () => {
    const { result } = renderHook(() => useRemoteCadence(smallRemote));
    expect(result.current).toEqual(CADENCE_PRESETS.remoteSmall);
    act(() => {
      setRemoteTrackedCount(smallRemote.machineId, smallRemote.handle, LARGE_REPO_THRESHOLD + 10);
    });
    expect(result.current).toEqual(CADENCE_PRESETS.remoteLarge);
  });

  it("per-machine override wins even for a large repo", () => {
    setRemoteTrackedCount(smallRemote.machineId, smallRemote.handle, 100_000);
    const custom = { watcherMs: 100, sourceReadMs: 200, diffMs: 300 };
    const { result } = renderHook(() => useRemoteCadence(smallRemote));
    expect(result.current).toEqual(CADENCE_PRESETS.remoteLarge);
    act(() => {
      setRemoteCadenceOverride(smallRemote.machineId, custom);
    });
    expect(result.current).toEqual(custom);
  });

  it("clearing an override returns to the auto-selected preset", () => {
    const custom = { watcherMs: 100, sourceReadMs: 200, diffMs: 300 };
    setRemoteCadenceOverride(smallRemote.machineId, custom);
    const { result } = renderHook(() => useRemoteCadence(smallRemote));
    expect(result.current).toEqual(custom);
    act(() => {
      setRemoteCadenceOverride(smallRemote.machineId, null);
    });
    expect(result.current).toEqual(CADENCE_PRESETS.remoteSmall);
  });

  it("inspect helper reads back the current override + tracked count", () => {
    const custom = { watcherMs: 100, sourceReadMs: 200, diffMs: 300 };
    setRemoteCadenceOverride(smallRemote.machineId, custom);
    setRemoteTrackedCount(smallRemote.machineId, smallRemote.handle, 42);
    expect(__inspectRemoteCadence(smallRemote)).toEqual({
      override: custom,
      trackedCount: 42,
    });
  });
});

// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  mintFrame,
  rebindFrameTeam,
  releaseFrame,
  type FrameRecord,
  type MintFrameArgs,
} from "@/tauri/commands";

import { flushFrame, retireAfterFlush, hasLiveFrames } from "../lib/frame-lifecycle";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { BridgedFrameBudget } from "../lib/bridged-frame-budget";
import { bridgeTiming, nowMs } from "../lib/bridge-timing";

const budget = new BridgedFrameBudget(VIEWER_PERF.maxBridgedFrames, (pinned) => {
  if (import.meta.env.DEV)
    console.warn(`Bridged frames: ${pinned} pinned frames exceed the budget`);
});
interface FrameActivity {
  visible: boolean;
  unsaved: boolean;
  retiring?: boolean;
}

/** One host mount owns one revocable frame, including a mint that finishes
 * after unmount. A changed document never renders with the previous grant. */
export function useFrameRecord(
  port: number | null | undefined,
  args: MintFrameArgs | null,
  activity?: FrameActivity,
) {
  const mount = useRef(Symbol("bridged-frame"));
  const latestActivity = useRef(activity);
  const visible = activity?.visible ?? true;
  const unsaved = activity?.unsaved ?? false;
  const retiring = activity?.retiring ?? false;
  const [generation, retry] = useState(0);
  const remintFrame = useCallback(() => {
    bridgeTiming(() => `frame-remint reason=requested t=${nowMs()}`);
    retry((n) => n + 1);
  }, []);
  useEffect(() => {
    latestActivity.current = activity;
  });
  const documentPath = args?.documentPath;
  const contentRoot = args?.contentRoot;
  const assetFolder = args?.assetFolder;
  const slot = args?.slot;
  const org = args?.message?.org;
  const team = args?.message?.team;
  const messageId = args?.message?.messageId;
  const builtinOrg = args?.builtinTeam?.org;
  const builtinTeam = args?.builtinTeam?.team;
  const policy = JSON.stringify(args?.policy ?? {});
  const request = useMemo<MintFrameArgs | null>(() => {
    if (!documentPath || !contentRoot || !slot) return null;
    return {
      documentPath,
      contentRoot,
      assetFolder,
      slot,
      policy: JSON.parse(policy),
      ...(builtinOrg && builtinTeam ? { builtinTeam: { org: builtinOrg, team: builtinTeam } } : {}),
      ...(org !== undefined && team !== undefined && messageId !== undefined
        ? { message: { org, team, messageId } }
        : {}),
    };
  }, [
    documentPath,
    contentRoot,
    assetFolder,
    slot,
    org,
    team,
    messageId,
    policy,
    builtinOrg,
    builtinTeam,
  ]);
  const key = JSON.stringify([port, request]);
  const [state, setState] = useState<{
    key: string;
    frame: FrameRecord | null;
    error: string | null;
    evicted?: boolean;
  } | null>(null);
  useEffect(() => {
    if (!request || typeof port !== "number") {
      // A disabled viewer must not revive its released frame if enabled again.
      setState(null);
      return;
    }
    let active = true;
    let minted: FrameRecord | null = null;
    let unregister: (() => void) | undefined;
    const release = (record: FrameRecord) => {
      void releaseFrame(record.frameId).catch(console.error);
    };
    mintFrame(request)
      .then((frame) => {
        minted = frame;
        if (!active) {
          release(frame);
          return;
        }
        setState({ key, frame, error: null });
        if (request.slot !== "baseline")
          unregister = budget.register(mount.current, {
            visible: latestActivity.current?.visible ?? true,
            unsaved: latestActivity.current?.unsaved ?? false,
            privateSpace: frame.kind === "private",
            flush: () =>
              retireAfterFlush(
                (f) => f.frameId === frame.frameId,
                () => {},
              ),
            evict: () => {
              bridgeTiming(
                () =>
                  `frame-evict frame=${frame.frameId} path=${documentPath} t=${nowMs()} maxBridgedFrames=${VIEWER_PERF.maxBridgedFrames}`,
              );
              minted = null;
              release(frame);
              setState({ key, frame: null, error: null, evicted: true });
            },
          });
      })
      .catch((error: unknown) => {
        if (active)
          setState({
            key,
            frame: null,
            error: `Failed to create document frame: ${String(error)}`,
          });
      });
    return () => {
      active = false;
      unregister?.();
      if (minted) {
        const record = minted;
        if (hasLiveFrames((f) => f.frameId === record.frameId))
          void flushFrame(record).then(() => release(record));
        else release(record);
      }
    };
  }, [port, request, key, generation]);
  useEffect(() => {
    budget.update(mount.current, { visible: visible && !retiring, unsaved });
  }, [visible, unsaved, retiring]);
  useEffect(() => {
    if (visible && state?.key === key && state.evicted) {
      bridgeTiming(
        () => `frame-remint reason=evicted-now-visible path=${documentPath} t=${nowMs()}`,
      );
      retry((n) => n + 1);
    }
  }, [visible, state, key, documentPath]);
  const frame = state?.key === key ? state.frame : null;
  const [, rebound] = useState(0);
  /** A follows-context frame moves to `team` without a new mint: the record keeps its identity
   *  and is updated in place, so the page, its listeners and its lifecycle never restart. */
  const rebindTeam = useCallback(
    async (team: { org: string; team: string }) => {
      if (!frame) throw new Error("unavailable: no frame");
      const next = await rebindFrameTeam(frame.frameId, frame.token, team.org, team.team);
      Object.assign(frame, next);
      rebound((n) => n + 1);
      return frame;
    },
    [frame],
  );
  return {
    remintFrame,
    rebindTeam,
    frameRecord: frame,
    frameError: state?.key === key ? state.error : null,
  };
}

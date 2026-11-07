// SPDX-License-Identifier: MIT
import { isFrameFlushing } from "./frame-lifecycle";
import type { FrameCallerKind } from "@/tauri/commands";
import { invoke } from "@tauri-apps/api/core";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
export type VerbCaller = {
  kind: FrameCallerKind;
  frameId: string;
  principal: string;
  dashboard: string;
  slot: string;
  team: { org: string; team: string } | null;
  scope: string;
};
export type Admission =
  | { status: "admit"; callId: string }
  | { status: "deny"; reason: string }
  | { status: "pending"; confirmId: string; allowAlways: boolean };
export type PendingCall = {
  caller: VerbCaller;
  verb: string;
  args: unknown;
  callId: string;
  allowAlways: boolean;
};
export const verbDispatch = async (caller: VerbCaller, verb: string, args: unknown) =>
  invoke<Admission>("verb_dispatch", {
    caller,
    expectedPrincipal: (await rendererPrincipal()) ?? "",
    verb,
    args: args ?? {},
    ...(isFrameFlushing(caller.frameId) ? { flushing: true } : {}),
  });
export const verbAnswer = (confirmId: string, allow: boolean) =>
  invoke<PendingCall>("verb_answer", { confirmId, allow });
export const verbSettle = (callId: string, landed: boolean, reason: string) =>
  invoke<void>("verb_settle", { callId, landed, reason });

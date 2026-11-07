// SPDX-License-Identifier: MIT
import { useCallback, useContext, useMemo } from "react";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
import { bridgeDocContext } from "@/tauri/commands";
import {
  runBridgeCommand,
  resolveBridgeCommandArgs,
  resolveBridgeReplyRecipient,
  resolveFrameCall,
  checkFrameGrant,
  type FrameCallIdentity,
  type BridgeContext,
} from "./html-bridge";
import { verbDispatch, verbAnswer, verbSettle, type VerbCaller } from "../lib/verb-dispatch";
import { updateDashboardPolicy } from "../lib/verb-policy";
import { VerbConfirmationContext } from "./verb-confirmation-context";

/** The native dispatcher owns admission; admitted handlers remain in the host bridge. */
export function useActionGate(
  filePath: string | null,
  ctx?: BridgeContext,
  onAdmitted?: (verb: string) => void,
) {
  const frameId = useMemo(() => crypto.randomUUID(), [filePath]);
  const requestConfirmation = useContext(VerbConfirmationContext);
  const runGatedAction = useCallback(
    async (
      command: unknown,
      args: unknown,
      identity?: FrameCallIdentity,
      options?: { confirmInHost?: boolean; isCurrent?: () => boolean },
    ): Promise<unknown> => {
      const grant = ctx?.frame ? await resolveFrameCall(ctx.frame, identity) : undefined;
      const docPath = ctx?.docPath ?? filePath ?? "";
      const callContext: BridgeContext = {
        ...ctx,
        docPath,
        ...(grant ? { frameGrant: grant } : {}),
      };
      const verb = String(command);
      const resolvedArgs = grant ? await resolveBridgeCommandArgs(verb, args, callContext) : args;
      if (grant && ctx?.frame) await checkFrameGrant(grant, verb, resolvedArgs, ctx.frame);
      const location = grant || ctx?.message ? null : await bridgeDocContext(docPath);
      const kind: VerbCaller["kind"] =
        grant && ctx?.frame
          ? ctx.frame.kind
          : ctx?.message
            ? "message"
            : docPath.startsWith("builtin:") || docPath.startsWith("virtual:")
              ? "builtin"
              : location?.layer === "private" || !location?.team
                ? "private"
                : "team";
      const caller: VerbCaller = grant
        ? { ...grant, kind }
        : {
            kind,
            frameId,
            principal: (await rendererPrincipal()) ?? "",
            dashboard: ctx?.prefsKey ?? docPath,
            slot: ctx?.message ? "message" : ctx?.prefsKey?.startsWith("pane:") ? "pane" : "center",
            team: ctx?.message
              ? { org: ctx.message.org, team: ctx.message.team }
              : location?.team
                ? { org: location.org ?? "", team: location.team }
                : null,
            scope: kind,
          };
      const admission = await verbDispatch(caller, verb, resolvedArgs);
      if (admission.status === "deny")
        throw new Error(
          admission.reason === "not-allowed" ? "denied: not-allowed" : admission.reason,
        );
      let callId: string;
      let admittedArgs = resolvedArgs;
      let confirmed = false;
      if (admission.status === "pending") {
        let answer;
        try {
          answer = await requestConfirmation({
            id: admission.confirmId,
            caller,
            verb,
            args: resolvedArgs,
            allowAlways: admission.allowAlways,
            ...(verb === "replyAsHuman"
              ? {
                  replyRecipient: await resolveBridgeReplyRecipient(resolvedArgs, {
                    docPath,
                    ...callContext,
                  }).catch(() => undefined),
                }
              : {}),
          });
        } catch (error) {
          await verbAnswer(admission.confirmId, false);
          throw error;
        }
        const call = await verbAnswer(admission.confirmId, answer !== "cancel");
        if (answer === "cancel") return { status: "cancelled" };
        callId = call.callId;
        admittedArgs = call.args;
        confirmed = true;
        if (answer === "always") {
          await updateDashboardPolicy((policy) => ({
            ...policy,
            perDashboard: {
              ...policy.perDashboard,
              [caller.dashboard]: { ...policy.perDashboard[caller.dashboard], [verb]: "never" },
            },
          }));
        }
      } else {
        callId = admission.callId;
        if (options?.confirmInHost) {
          const answer = await requestConfirmation({
            id: `host:${crypto.randomUUID()}`,
            caller,
            verb,
            args: resolvedArgs,
            allowAlways: false,
          });
          if (answer === "cancel") {
            await verbSettle(callId, false, "cancelled");
            return { status: "cancelled" };
          }
          confirmed = true;
        }
      }
      if (options?.isCurrent && !options.isCurrent()) {
        await verbSettle(callId, false, "stale document");
        throw new Error("unavailable: document was replaced; reload and retry");
      }
      onAdmitted?.(verb);
      let result: unknown;
      try {
        result = await runBridgeCommand(verb, admittedArgs, { docPath, ...callContext });
      } catch (error) {
        await verbSettle(callId, false, error instanceof Error ? error.message : String(error));
        throw error;
      }
      const landed = !Array.isArray(result) || result.some((row) => row?.ok === true);
      await verbSettle(callId, landed, landed ? "admitted" : "write not applied");
      return confirmed ? { status: "confirmed", result } : result;
    },
    [filePath, ctx, frameId, requestConfirmation, onAdmitted],
  );
  return { runGatedAction };
}

// SPDX-License-Identifier: MIT
import { createContext } from "react";
import type { VerbCaller } from "../lib/verb-dispatch";
export type Approval = "once" | "always" | "cancel";
export type ConfirmationRequest = {
  id: string;
  caller: VerbCaller;
  verb: string;
  args: unknown;
  allowAlways: boolean;
  replyRecipient?: string;
};
export const VerbConfirmationContext = createContext<
  (request: ConfirmationRequest) => Promise<Approval>
>(async () => {
  throw new Error("unavailable: host confirmation layer");
});

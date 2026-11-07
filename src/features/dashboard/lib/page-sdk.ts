// SPDX-License-Identifier: MIT
import type { FrameRecord } from "@/tauri/commands";

import { PAGE_SDK_FEATURES, VERB_REGISTRY } from "./verb-registry.generated";

export { PAGE_SDK_FEATURES };

/** What the shim's `has()` answers from: the features and the verbs this frame's effective
 *  grant admits. Architecture: informational only; Rust checks every call. Policy: under the
 *  default `open` preset the grant is the full vocabulary, so every verb is listed. */
export function pageSdk(frame: Pick<FrameRecord, "effectiveGrant"> | null | undefined) {
  const grant = new Set(frame?.effectiveGrant ?? []);
  return {
    features: [...PAGE_SDK_FEATURES],
    verbs: VERB_REGISTRY.filter((verb) => grant.has(verb.grant)).map((verb) => verb.name),
  };
}

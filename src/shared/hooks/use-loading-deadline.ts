// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { CONTENT_SCAN_TIMEOUT_MS } from "@/shared/lib/pipeline-graph/content-scan-queue";

/** Bound bootstrap waiting without changing or cancelling the underlying operation. */
export function useLoadingDeadline(loading: boolean, identity: string): boolean {
  const token = useMemo(() => Symbol(identity), [identity]);
  const [expired, setExpired] = useState<symbol>();
  useEffect(() => {
    if (!loading) return;
    const timer = setTimeout(() => setExpired(token), CONTENT_SCAN_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [loading, token]);
  return expired === token;
}

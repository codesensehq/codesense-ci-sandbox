// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState } from "react";
import { CodeDiffReview } from "@/shared/components/code-editor/CodeDiffReview";
import { createIntentReader } from "../../../../src-tauri/assets/dashboards/story-trail/intent-reader.mjs";
import { readOpenDiff } from "../lib/source-redline-source";
import { diffTabAddress, openDiffTab, type DiffTabAddress } from "../lib/open-diff-route";
import type { OpenDiffRead } from "../lib/open-diff-types";

export function OpenDiffTab({ filePath }: { filePath: string }) {
  const address = useMemo(() => diffTabAddress(filePath), [filePath]);
  if (!address)
    return (
      <p role="status" className="p-3">
        This diff address is not valid.
      </p>
    );
  return <DiffEntry key={filePath} address={address} />;
}

function DiffEntry({ address: { params, team } }: { address: DiffTabAddress }) {
  const [result, setResult] = useState<OpenDiffRead | null>(null);
  const [error, setError] = useState("");
  const readerRef = useRef<ReturnType<typeof createIntentReader> | null>(null);
  useEffect(() => {
    setResult(null);
    setError("");
    const reader = createIntentReader({
      capture: () => params,
      read: readOpenDiff,
      commit: (data: OpenDiffRead) => {
        setResult(data);
        setError("");
      },
      fail: (failure: unknown) => setError(String(failure)),
    });
    readerRef.current = reader;
    reader.intent({ read: true });
    return () => {
      reader.stop();
      readerRef.current = null;
    };
  }, [params]);
  const retry = () => readerRef.current?.intent({ read: true });
  const openWorkspace = async () => {
    try {
      const { HTML_BRIDGE_ALLOWLIST } = await import("@/features/dashboard/components/html-bridge");
      await HTML_BRIDGE_ALLOWLIST.openWorkspace({ storyId: params.storyId });
    } catch (failure) {
      setError(String(failure));
    }
  };
  return (
    <section className="flex h-full min-h-0 flex-col" aria-label="Source diff">
      <header className="flex flex-wrap items-center gap-3 border-b p-3 text-sm">
        <span>
          {params.repo} · {params.rev} · {params.path}
        </span>
        <button type="button" onClick={retry}>
          Refresh
        </button>
        {params.storyId && (
          <button
            type="button"
            onClick={() => {
              void openWorkspace();
            }}
          >
            Open in workspace
          </button>
        )}
        {result?.status === "opened" && (
          <label>
            File{" "}
            <select
              value={params.path}
              onChange={(event) =>
                openDiffTab({ ...params, path: event.target.value, range: undefined }, team)
              }
            >
              {!result.files.some((file) => file.path === params.path) && (
                <option value={params.path}>{params.path}</option>
              )}
              {result.files.map((file) => (
                <option key={file.path} value={file.path}>
                  {file.path}
                </option>
              ))}
            </select>
          </label>
        )}
      </header>
      {error ? (
        <p role="alert" className="p-3">
          {error}
        </p>
      ) : !result ? (
        <p role="status" className="p-3">
          Loading diff…
        </p>
      ) : result.status !== "opened" ? (
        <div role="status" className="p-3">
          <p>{result.reason}</p>
          {result.status === "commitNotLocal" && (
            <label>
              Fetch command{" "}
              <input
                readOnly
                value={result.fetchCommand}
                onFocus={(event) => event.target.select()}
              />
            </label>
          )}
        </div>
      ) : result.redline.binary ? (
        <p className="p-3">Binary file — a text diff is unavailable.</p>
      ) : (
        <div className="min-h-0 flex-1">
          <CodeDiffReview
            before={result.redline.before}
            after={result.redline.after}
            path={params.path}
            afterRange={params.range}
            collapseUnchanged={!params.range}
          />
        </div>
      )}
    </section>
  );
}

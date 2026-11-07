# git-ai checkpoint evidence

These are **scripted-real-engine** records captured in a scripted run of git-ai
1.7.5. They are not logged-in Claude/Codex session captures. Session/tool ids were
supplied in scripted hook payloads; git-ai generated the trace ids and checkpoint
records. No fabricated exec record is presented as a logged-in real record.

Imported without field changes from that run's evidence file:
- line13 → `archived-codex.jsonl`, originally under
  `.git/ai/working_logs/old-74cbadc8b4d89499d88de8b97477c0f54b365707/checkpoints.jsonl`;
- lines15–16 → `live-claude.jsonl`, originally under
  `.git/ai/working_logs/16b53d541132682ebc81b6105085b5668e96308f/checkpoints.jsonl`.

The two Claude traces map to one tool-use id. Files are compacted JSON, preserving
all original fields. The source evidence SHA256 is `9b1bfa5cb1fdcdbce49f8dd8b5a8893f64ec7a2e723efbb10121a82a8de02ea1`.

## Offline evidence limitation

When this evidence was gathered (2026-10-28), git-ai was absent from PATH, the managed
CodeSense installation locations, common local bin directories, and the earlier
sandbox installation, so the absence fallback applies. No binary
or documentation was downloaded, and no new linked-worktree engine run could be
performed. Archive retention is **unverified**: no pruning interval is asserted.
The old evidence proves a live and an `old-<sha>` placement existed in that run,
not how long an archive survives. Discovery tests place these captured records
in both common Git directories and actual linked-worktree Git directories.

On-demand harvest cannot recover a log pruned, or a worktree deleted, before its
first query. Capture-time harvesting remains a possible separate follow-up if
measured retention later warrants it.

Directory label = base, mapping inferred, unverified without a binary. A log is
positive coverage for a noted commit only if its directory labels that commit or
one of its parents; unrelated live logs and uncertain labels are not coverage.
Exact durable trace rows take priority over this inferred coverage.

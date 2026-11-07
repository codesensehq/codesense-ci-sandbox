// SPDX-License-Identifier: MIT
// Wire shapes from session_usage/{story_changes,session_read,task,record,timing}.rs.
export const task = (taskId, parentId = null, extra = {}) => ({
  taskId, org: 'example/project', team: 'dev', storyId: 'SV-X', goal: taskId,
  taskType: 'build', subject: null, parentId, parentSource: parentId ? 'launcher' : 'none',
  typeSource: 'brief', fieldSources: {}, mainSkill: 'bmad-build', targets: null,
  layers: {skills: [], source: null, context: null}, unavailable: {}, status: 'open', rawStatus: null, closedAtMs: null,
  acceptanceMet: null, label: null, tamper: null, tamperGlobs: [], worktree: null,
  createdBy: null, createdAtMs: 1000, reviews: [], ...extra,
});
export const session = (id, extra = {}) => ({
  session: {sessionId: id, nativeSessionId: null, org: 'example/project', team: 'dev',
    handle: id, launchedBy: null, agent: 'builder@abc', manifestHash: null, cli: 'codex',
    cliVersion: null, model: 'gpt-model', mcpAllow: null, bridgeTools: null,
    account: {provider: 'openai', accountId: 'work', label: 'Work', source: 'active'},
    storyId: 'SV-X', workspace: null, worktree: null, branch: null, runId: null,
    configHome: null, providedSkills: [], unavailable: {}, recordedAtMs: 1000,
    timing: {startedAtMs: 1000, endedAtMs: 61000, wallMs: 60000, activeMs: 40000,
      idleMs: 20000, turnCount: 1, toolCallCount: 2, timingSource: 'headless-capture', idleGaps: []}},
  nativeSessionIds: [], story: {id: 'SV-X', title: 'A story', status: null, epic: null},
  run: {state: 'unlinked', reason: 'the session has no linked run'},
  models: [{model: 'gpt-model', calls: 1, tokens: {input: 100, output: 20, cacheRead: 0, cacheWrite: 0, cacheWrite1h: 0}, costUsd: 2, costSource: 'reported', reason: null}],
  cost: {state: 'present', totalUsd: 2, source: 'reported', reason: null},
  tools: {allowed: {mcpAllow: null, bridgeTools: null, unavailable: {}}, used: null,
    usedState: 'absent', usedReason: 'no conversation has been stored for this session yet'},
  tasks: [], retries: null, unavailable: {}, basis: 'exact', ...extra,
});
export const story = (extra = {}) => ({
  storyId: 'SV-X', sessions: [session('lead', {tasks: [task('lead', 'story:SV-X')]})],
  tasks: [task('story:SV-X', null, {goal: 'A story', taskType: 'story'}), task('lead', 'story:SV-X')],
  totals: {sessions: 1, costUsd: 2, wallMs: 60000, activeMs: 40000, idleMs: 20000, reviewRounds: 0},
  unavailable: {}, ...extra,
});

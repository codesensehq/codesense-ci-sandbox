// SPDX-License-Identifier: MIT
// Pure projections of the shared session/task reads. No DOM, reads or clocks.
export const money = value => value == null ? 'unknown' : `$${value.toFixed(2)}`;
export const duration = value => {
  if (value == null) return '';
  const seconds = Math.round(value / 1000);
  if (value > 0 && seconds < 60) return `${seconds}s`;
  const minutes = Math.round(value / 60000);
  return minutes >= 60 ? `${Math.floor(minutes / 60)}h${minutes % 60 ? ` ${minutes % 60}m` : ''}` : `${minutes}m`;
};
export const sessionId = row => row.session?.sessionId ?? row.nativeSessionIds?.[0];
export const timing = row => row.session?.timing;
const completeSum = (rows, value) => rows.length && rows.every(r => value(r) != null)
  ? rows.reduce((sum, r) => sum + value(r), 0) : null;
export function spanRows(rows) {
  return rows.filter(row => timing(row)?.startedAtMs != null && timing(row)?.wallMs != null)
    .map(row => ({id: sessionId(row), label: sessionFacts(row).agent, basis: row.basis,
      start: timing(row).startedAtMs, end: timing(row).startedAtMs + timing(row).wallMs}));
}
export function basisTotals(data) {
  const result = {};
  for (const basis of ['exact', 'associated', 'unknown']) {
    const rows = data.sessions.filter(row => (row.basis ?? 'unknown') === basis);
    if (!rows.length) continue;
    const spans = spanRows(rows);
    const costUsd = completeSum(rows, r => r.cost?.totalUsd);
    const allTimed = spans.length === rows.length;
    result[basis] = {sessions: rows.length, costUsd,
      reason: costUsd == null ? data.unavailable?.costUsd ?? rows.find(r => r.cost?.totalUsd == null)?.cost?.reason ?? 'not priced' : null,
      timeMs: completeSum(rows, r => timing(r)?.wallMs),
      activeMs: completeSum(rows, r => timing(r)?.activeMs),
      wallMs: allTimed ? Math.max(...spans.map(s => s.end)) - Math.min(...spans.map(s => s.start)) : null,
      timeReason: allTimed ? null : data.unavailable?.time ?? 'no observed timing for one or more sessions'};
  }
  return result;
}
export function buildTree(tasks) {
  const nodes = new Map(tasks.map(task => [task.taskId, {task, children: []}]));
  const roots = [];
  for (const node of nodes.values()) {
    const parent = nodes.get(node.task.parentId);
    if (parent) parent.children.push(node); else roots.push(node);
  }
  return roots;
}
export const costSourceLabel = source => ({reported:'reported', rate_table:'from price list', mixed:'reported + price list'})[source] ?? '';
export function sessionFacts(row) {
  const record = row.session;
  const missing = row.cost?.totalUsd == null;
  const modelNames = row.models?.map(m => m.model) ?? [];
  const tools = row.tools?.used;
  const outcome = row.run?.state === 'present' ? row.run.outcome?.outcome : null;
  return {
    id: sessionId(row), agent: record?.agent ?? record?.handle ?? 'hand-started',
    cli: record?.cli, account: record?.account?.label,
    model: [...new Set([...modelNames, ...(record?.model && !modelNames.length ? [record.model] : [])])].join(' · '),
    link: record ? '' : `not linked · ${row.unavailable?.session ?? 'no session record'}`,
    basis: row.basis ?? 'unknown', cost: money(row.cost?.totalUsd),
    costSource: missing ? 'not priced' : costSourceLabel(row.cost?.source),
    priceModels: missing ? (row.models ?? []).filter(m => m.costUsd == null || m.costSource == null).map(m => m.model) : [],
    costReason: missing ? row.cost?.reason : null,
    tools: tools == null ? row.tools?.usedReason ?? '' : tools.map(t => `${t.name} ${t.calls}`).join(' · '),
    toolsReason: tools != null ? row.tools?.usedReason : null,
    time: duration(timing(row)?.wallMs), active: duration(timing(row)?.activeMs),
    outcome: outcome ? [outcome.status, outcome.summary].filter(v => typeof v === 'string').join(' · ') : row.run?.reason ?? '',
  };
}
export function compareTarget(actual, target, format = String) {
  if (actual == null) return typeof target === 'number' ? `target ${format(target)}` : '';
  const value = format(actual);
  if (typeof target !== 'number') return value;
  const diff = actual - target;
  return `${value} · target ${format(target)} · ${diff === 0 ? 'same as target' : `${format(Math.abs(diff))} ${diff > 0 ? 'more' : 'less'} than target`}`;
}
export function taskFacts(task, sessions) {
  const rows = sessions.filter(s => s.tasks?.some(t => t.taskId === task.taskId));
  const targets = task.targets?.values ?? {};
  // Only task-scoped outcomes measure a task. Linked sessions identify its agent.
  const unknown = (reason,target,format) => `unknown · ${reason}${typeof target==='number'?` · target ${format(target)}`:''}`;
  const outcome=task.outcome;
  const timeReason=outcome?.unavailable?.time ?? task.unavailable?.time ?? task.unavailable?.outcome ?? 'no task timing recorded';
  const costReason=outcome?.unavailable?.cost ?? task.unavailable?.cost ?? task.unavailable?.costUsd ?? task.unavailable?.outcome ?? 'no task cost recorded';
  const reviews = outcome?.reviewRounds ?? task.reviews?.length;
  const unique = field => [...new Set(rows.map(r => sessionFacts(r)[field]).filter(Boolean))].join(' · ');
  return {type: task.taskType === 'unclassified' || task.typeSource === 'unclassified'
      ? `unclassified${task.unavailable?.type ? ` · ${task.unavailable.type}` : ''}` : task.taskType,
    skill: task.mainSkill ?? '', agent: unique('agent'), model: outcome?.cost?.models?.length?outcome.cost.models.join(' · '):unique('model'),
    time: outcome?.time?.wallMs!=null?compareTarget(outcome.time.wallMs,typeof targets.wall_minutes==='number'?targets.wall_minutes*60000:null,duration):unknown(timeReason,targets.wall_minutes,minutes=>duration(minutes*60000)),
    active: outcome?.time?.activeMs!=null?compareTarget(outcome.time.activeMs,typeof targets.active_minutes==='number'?targets.active_minutes*60000:null,duration):typeof targets.active_minutes==='number'?unknown(timeReason,targets.active_minutes,minutes=>duration(minutes*60000)):'',
    cost: outcome?.cost?.costUsd!=null?compareTarget(outcome.cost.costUsd,targets.cost_usd,money):unknown(costReason,targets.cost_usd,money),
    rounds: reviews == null ? 'not tracked' : compareTarget(reviews, targets.review_rounds),
  };
}
export function pageState(data, error) {
  if (!data) return error ? 'failed' : 'loading';
  if (error) return 'stale';
  return data.sessions.length ? 'ready' : 'empty';
}
export function sessionsByTask(tasks, sessions) {
  const seen = new Map(), groups = [];
  const ordered=[...sessions].sort((a,b)=>{const x=timing(a)?.startedAtMs,y=timing(b)?.startedAtMs;return x==null?y==null?0:1:y==null?-1:x-y;});
  const visit = nodes => {for (const node of nodes) {
    const linked = ordered.filter(s => s.tasks?.some(t => t.taskId === node.task.taskId));
    if (linked.length) groups.push({task:node.task, entries:linked.map(session=>{
      const id=sessionId(session), first=seen.get(id);
      if(!first) seen.set(id,node.task.goal);
      return {session, sameAs:first ?? null};
    })});
    visit(node.children);
  }};
  visit(buildTree(tasks));
  const unlinked=ordered.filter(s=>!seen.has(sessionId(s)));
  if(unlinked.length)groups.push({task:null,entries:unlinked.map(session=>({session,sameAs:null}))});
  return groups;
}

export const localDate = ms => {const d=new Date(ms);return `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,'0')}-${String(d.getDate()).padStart(2,'0')}`;};

export function rateUsagePriced(request,sessions) {
  const isSession=request.rowId.startsWith('session:');
  const id=request.rowId.slice(isSession?'session:'.length:'task:'.length);
  const rows=sessions.filter(row=>isSession?sessionId(row)===id:row.tasks?.some(task=>task.taskId===id));
  const models=rows.flatMap(row=>row.models??[]).filter(model=>model.model===request.model);
  return models.length>0 && models.every(model=>model.costUsd!=null);
}

// Rust session_usage::model_key: model.rsplit('/').next().unwrap_or(model).replace('.', "-")
export const modelRatePrefix = model => String(model).split('/').at(-1).replaceAll('.', '-');

// Line ownership: why_line's span rule, the story's own ranges and the revision it reads.
export const LINE_LIMIT_REASON = 'span must be 1-based, inclusive, ordered, and end - start <= 2000';
export const lineRefusal = (start, end) => Number.isInteger(start) && Number.isInteger(end) && start >= 1 && start <= end && end - start <= 2000 ? null : LINE_LIMIT_REASON;
export function prefillLines(changes, path) {
  const ranges = (changes?.commits ?? []).flatMap(commit => commit.ranges ?? []).filter(range => range.path === path);
  if (!ranges.length) return null;
  const start = Math.min(...ranges.map(r => r.start)), end = Math.max(...ranges.map(r => r.end));
  return end - start <= 2000 ? {start, end} : {start: ranges[0].start, end: Math.min(ranges[0].end, ranges[0].start + 2000)};
}
// The line panel only reads and diffs files this story changed.
export const storyFile = (data, path) => !!data?.actual?.some(file => file.path === path);
// The Files row's diff choices: the live story worktree, then commits (newest first), then merged PRs.
export function diffChoices(data, path) {
  const choices = [...((data?.worktrees ?? []).some(row => row.state === 'live' && row.basis === 'exact') ? [{rev: 'WORKTREE', label: 'Worktree diff'}] : []),
    ...(data?.commits ?? []).filter(commit => commit.files?.includes(path)).map(commit => ({rev: commit.sha, label: 'Diff ' + commit.sha.slice(0, 7)})),
    ...(data?.prs ?? []).filter(pr => pr.mergedSha && pr.files?.includes(path)).map(pr => ({rev: pr.mergedSha, label: 'Diff ' + pr.mergedSha.slice(0, 7)}))];
  return choices.filter((choice, index) => choices.findIndex(other => other.rev === choice.rev) === index);
}
// Merged work reads and diffs its merged sha; unmerged work reads the story's own local branch and diffs the Files row's first choice.
export function lineRevision(data, path) {
  if (!storyFile(data, path)) return null;
  const pr = (data?.prs ?? []).filter(pr => pr.mergedSha && pr.files?.includes(path)).sort((a, b) => (b.mergedAt ?? 0) - (a.mergedAt ?? 0))[0];
  if (pr) return {rev: pr.mergedSha, diffRev: pr.mergedSha, label: `Reads the merged change (${pr.mergedSha.slice(0, 7)})`};
  const branch = (data?.branches ?? []).find(row => row.basis === 'exact' && row.availability === 'local')?.name;
  return branch ? {rev: branch, diffRev: diffChoices(data, path)[0]?.rev ?? null, label: `Reads branch ${branch}`} : null;
}
export const lineSpan = ({start, end}) => start === end ? `Line ${start}` : `Lines ${start}–${end}`;
// Only data-named sessions are claimed: exact linked ranges by session, a named runtime without a record as
// "session unknown", associated ranges as their candidates, and every other range as text with its reason.
export function lineOwnership(result, storyId, sessions = []) {
  const groups = new Map(), associated = [], other = [], candidates = new Set();
  const agent = id => {const row = result.sessions?.[id] ?? sessions.find(s => sessionId(s) === id || s.nativeSessionIds?.includes(id));return row ? sessionFacts(row) : null;};
  let changed = 0;
  for (const range of result.ranges ?? []) {
    const sha = range.commit?.slice(0, 7);
    if (range.basis === 'associated') {associated.push(range);for (const id of range.candidateSessions ?? []) candidates.add(id);continue;}
    if (range.basis !== 'exact' || range.tool === 'human' || !range.tool) {other.push({range, text: [lineSpan(range), sha, range.reason].filter(Boolean).join(' · ')});continue;}
    const linked = range.link === 'linked' && range.sessionId;
    if (linked && range.story === storyId) changed += range.end - range.start + 1;
    const key = linked ? `session:${range.sessionId}` : `runtime:${range.tool}`;
    if (!groups.has(key)) {const facts = linked ? agent(range.sessionId) : null;groups.set(key, {key, sessionId: linked ? range.sessionId : null, agent: linked ? facts?.agent ?? range.sessionId : range.tool, detail: [range.model ?? facts?.model, linked ? null : 'session unknown'].filter(Boolean).join(' · '), ranges: []});}
    const turn = linked ? range.turn?.turn ?? null : null, turnReason = linked && turn == null ? range.unavailable?.turn ?? range.turn?.reason : null;
    const elsewhere = linked && range.story !== storyId ? (range.story ? `another story (${range.story})` : 'story unknown') : null;
    groups.get(key).ranges.push({key: `${range.start}:${range.end}:${range.commit}`, turn, text: [lineSpan(range), turn == null ? null : `${range.turn.basis === 'commit' ? 'Committed' : 'Written'} in turn ${turn}`, sha, 'exact', elsewhere, linked ? turnReason : range.reason].filter(Boolean).join(' · ')});
  }
  return {changed, groups: [...groups.values()], associated, candidates: [...candidates].map(id => ({id, agent: agent(id)?.agent ?? id})), other};
}

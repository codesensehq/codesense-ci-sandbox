// SPDX-License-Identifier: MIT
import {describe, expect, it} from 'vitest';
import {basisTotals, buildTree, taskFacts, sessionFacts, pageState, spanRows} from './model.mjs';
import {session, story, task} from './fixtures.mjs';

describe('story trail facts', () => {
  it('keeps exact and associated totals separate and rejects partial costs', () => {
    const data = story({sessions: [session('a'), session('b', {cost: {state:'absent', totalUsd:null, source:null, reason:'not priced'}}), session('c', {basis:'associated'})], totals:{costUsd:null}, unavailable:{costUsd:'incomplete priced usage'}});
    const totals = basisTotals(data);
    expect(totals.exact.costUsd).toBeNull();
    expect(totals.exact.reason).toContain('incomplete priced usage');
    expect(totals.exact.sessions).toBe(2);
    expect(totals.associated.costUsd).toBe(2);
    expect(totals.exact.wallMs).toBe(60000); // span, not summed time
  });
  it('never treats missing timing as zero or a partial time total', () => {
    const data = story({sessions:[session('a'), session('b', {session:null, nativeSessionIds:['native'], unavailable:{session:'hand-started'}})]});
    expect(basisTotals(data).exact.activeMs).toBeNull();
    expect(spanRows(data.sessions)).toHaveLength(1);
  });
  it('builds nested tasks, keeps an orphan and shows an unclassified reason', () => {
    const tasks = [task('root'), task('child','root'), task('orphan','missing'), task('plain', null, {taskType:'unclassified',typeSource:'unclassified',unavailable:{type:'no rule matched'}})];
    expect(buildTree(tasks).map(n => [n.task.taskId,n.children.map(c => c.task.taskId)])).toEqual([['root',['child']],['orphan',[]],['plain',[]]]);
    expect(taskFacts(tasks[3],[]).type).toBe('unclassified · no rule matched');
    expect(taskFacts(task('a'),[session('a',{tasks:[task('a')]})]).cost).toContain('unknown');
  });
  it('joins tasks to sessions and compares real target names neutrally', () => {
    const t=task('a',null,{targets:{source:'brief',values:{wall_minutes:2,cost_usd:3,review_rounds:1}},reviews:['review-a','review-b']});
    const facts=taskFacts(t,[session('s',{tasks:[t]})]);
    expect(facts.agent).toBe('builder@abc');
    expect(facts.model).toBe('gpt-model');
    expect(facts.time).toContain('target 2m');
    expect(facts.cost).toContain('unknown');expect(facts.cost).toContain('target $3.00');
    expect(facts.rounds).toContain('2 · target 1');
  });
  it.each(['reported','rate_table','mixed'])('maps cost source %s', source => {
    expect(sessionFacts(session('a',{cost:{state:'present',totalUsd:2,source,reason:null}})).costSource).toBe({reported:'reported',rate_table:'from price list',mixed:'reported + price list'}[source]);
  });
  it.each(['truncated','expired','absent','unsupported','unavailable'])('shows tools reason for %s', usedState => {
    expect(sessionFacts(session('a',{tools:{used:null,usedState,usedReason:'capture reason'}})).tools).toBe('capture reason');
  });
  it('maps hand-started, associated, absent outcome, unknown pricing, and untracked reviews', () => {
    const facts=sessionFacts(session('native',{session:null,nativeSessionIds:['native'],basis:'associated',unavailable:{session:'hand-started'},cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced model'},models:[{model:'new-model',costSource:null}]}));
    expect(facts.agent).toBe('hand-started');
    expect(facts.link).toBe('not linked · hand-started');
    expect(facts.basis).toBe('associated');
    expect(facts.costSource).toBe('not priced');
    expect(facts.outcome).toBe('the session has no linked run');
    expect(facts).not.toHaveProperty('retries');
    expect(taskFacts(task('a',null,{reviews:undefined}),[]).rounds).toBe('not tracked');
    expect(sessionFacts(session('a',{run:{state:'absent',reason:'no outcome recorded'}})).outcome).toBe('no outcome recorded');
  });
  it('maps loading, unknown, no linked sessions, later failed and ready', () => {
    expect(pageState(null,null)).toBe('loading');
    expect(pageState(null,'failed')).toBe('failed');
    expect(pageState(story({sessions:[],tasks:[]}),null)).toBe('empty');
    expect(pageState(story({sessions:[]}),null)).toBe('empty');
    expect(pageState(story(),'failed')).toBe('stale');
    expect(pageState(story(),null)).toBe('ready');
  });
});

it('lists shared sessions only once, with a reference under later tasks',async()=>{
  const {sessionsByTask}=await import('./model.mjs');
  const first=task('a',null,{goal:'Build'}),second=task('b',null,{goal:'Prove'});
  const groups=sessionsByTask([first,second],[session('s',{tasks:[first,second]})]);
  expect(groups[0].entries[0].sameAs).toBeNull();
  expect(groups[1].entries[0].sameAs).toBe('Build');
});
it('shows configured targets before a task has any actual measurements',()=>{
  const f=taskFacts(task('pending',null,{targets:{source:'brief',values:{wall_minutes:10,cost_usd:2}}}),[]);
  expect(f.time).toContain('target 10m');expect(f.cost).toContain('target $2.00');
});
it('reads a present outcome through the durable RunOutcomeRead envelope',()=>{
  expect(sessionFacts(session('a',{run:{state:'present',outcome:{outcome:{story_id:'SV-X',run_id:'run-1',status:'success',summary:'Built the change',ended_at:'2026-10-28T10:00:00Z',source:'signal'},validated:'completed',link:'linked',has_structured_transcript:false}}})).outcome).toBe('success · Built the change');
});
it('orders sessions chronologically inside each task, with untimed sessions last',async()=>{
  const {sessionsByTask}=await import('./model.mjs');const t=task('a');
  const early=session('early',{tasks:[t]}),late=session('late',{tasks:[t]});late.session.timing.startedAtMs=5000;
  const untimed=session('untimed',{session:null,nativeSessionIds:['untimed'],tasks:[t]});
  expect(sessionsByTask([t],[late,untimed,early])[0].entries.map(e=>e.session.session?.sessionId??e.session.nativeSessionIds[0])).toEqual(['early','late','untimed']);
});
it('does not allocate whole-session measurements to two subtasks',()=>{
  const one=task('one',null,{targets:{source:'brief',values:{wall_minutes:1,cost_usd:1}},unavailable:{time:'task interval not measured',cost:'task cost not measured'}}),two=task('two');
  const shared=session('shared',{tasks:[one,two]});shared.session.timing.wallMs=600000;shared.cost.totalUsd=10;
  const a=taskFacts(one,[shared]),b=taskFacts(two,[shared]);expect(a.time).toContain('unknown');expect(a.time).toContain('task interval not measured');expect(a.cost).toContain('task cost not measured');expect(a.time).not.toContain('more than');expect(b.time).not.toContain('10m');expect(b.cost).not.toContain('$10');
});
it('compares task-scoped outcome measurements with targets without borrowing session totals',()=>{
 const actual=task('task-a',null,{targets:{values:{wall_minutes:2,active_minutes:1,cost_usd:3}},outcome:{scope:'interval',time:{wallMs:60000,activeMs:30000,idleMs:30000,sessions:1},cost:{costUsd:1,models:['gpt-model'],inputTokens:100,outputTokens:20},reviewRounds:2,unavailable:{}}});
 const facts=taskFacts(actual,[session('whole-session',{tasks:[actual]})]);
 expect(facts.time).toBe('1m · target 2m · 1m less than target');expect(facts.active).toBe('30s · target 1m · 30s less than target');expect(facts.cost).toBe('$1.00 · target $3.00 · $2.00 less than target');expect(facts.rounds).toBe('2');expect(facts.time).not.toContain('not in story_changes');
});
it('retains outcome absence reasons and unpriced cost while showing recorded time',()=>{
 const t=task('task-a',null,{outcome:{time:{wallMs:30000,activeMs:10000},cost:{costUsd:null},unavailable:{cost:'a recorded model has no price'},reviewRounds:0}});const facts=taskFacts(t,[]);
 expect(facts.time).toBe('30s');expect(facts.cost).toContain('a recorded model has no price');expect(facts.time).not.toContain('unknown');
});

it('durations just under a minute round before choosing the unit', async () => {
  const {duration} = await import('./model.mjs');
  expect(duration(59_400)).toBe('59s');
  expect(duration(59_600)).toBe('1m');
});

it('labels a commit turn "Committed in turn" and an edit turn "Written in turn"', async () => {
  const {lineOwnership} = await import('./model.mjs');
  const range = (start: number, basis: string, turn: number) => ({start, end: start, commit: 'a'.repeat(40), basis: 'exact', tool: 'claude', sessionId: 's', link: 'linked', story: 'SV-X', turn: {sessionId: 's', toolUseId: 't', turn, basis}});
  const rows = lineOwnership({ranges: [range(1, 'edit', 4), range(2, 'commit', 7)], sessions: {}}, 'SV-X').groups[0].ranges.map((r: {text: string}) => r.text);
  expect(rows[0]).toContain('Written in turn 4');
  expect(rows[1]).toContain('Committed in turn 7');
  expect(rows[1]).not.toContain('Written');
});

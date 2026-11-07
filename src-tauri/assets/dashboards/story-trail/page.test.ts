// SPDX-License-Identifier: MIT
import {afterEach, describe, expect, it, vi} from 'vitest';
import {trailTestKit} from './test-kit';
import {mountTrail} from './page.mjs';
import {story, session} from './fixtures.mjs';

const disposers: (()=>void)[]=[];
afterEach(()=>{vi.useRealTimers();vi.unstubAllEnvs();disposers.splice(0).forEach(fn=>fn()); document.body.replaceChildren();});
const settle = async () => {for(let i=0;i<12;i++) await Promise.resolve();};
function boot(read=vi.fn().mockResolvedValue(story()), hash='#story/SV-X', native=false) {
  document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
  let visible=true, visibility=(value:boolean)=>{};
  const cs={kit:trailTestKit(),storyChanges:read,sessionRead:vi.fn(),setModelRate:vi.fn().mockResolvedValue(1),
    subscribe:vi.fn().mockResolvedValue({}),unsubscribe:vi.fn(),
    onVisibility: (fn:(v:boolean)=>void)=>{visibility=fn;fn(visible);return ()=>{};},
    onTheme:vi.fn(()=>()=>{})};
  const app=mountTrail({cs, doc:document, win:window, ...(native?{}:{getHash:()=>hash, navigate:(next:string)=>{hash=next;app.route();}})});
  disposers.push(app.stop);
  return {cs,app,show:(v:boolean)=>{visible=v;visibility(v);}};
}
describe('trail entry paths',()=>{
  it('opens #story/X, sorts accessibly and opens/closes a row without reordering',async()=>{
    const h=boot(vi.fn().mockResolvedValue(story({sessions:[session('lead'),session('second')]})));
    await settle();
    expect(h.cs.storyChanges).toHaveBeenCalledWith({storyId:'SV-X'});
    expect(document.querySelector('h1')?.textContent).toBe('A story');
    const rows=()=>[...document.querySelectorAll('[data-session]')].map(e=>e.getAttribute('data-session'));
    const before=rows();
    const button=document.querySelector<HTMLButtonElement>('[data-open="lead"]')!;
    button.focus();button.click();
    expect(rows()).toEqual(before);
    expect(document.querySelector('[aria-selected="true"]')).not.toBeNull();
    expect(document.querySelector('#drill-panel')?.getAttribute('role')).toBe('complementary');
    document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
    expect(document.activeElement?.getAttribute('data-open')).toBe('lead');
    document.querySelector<HTMLButtonElement>('[data-sort="agent"]')!.click();
    expect(document.querySelector('th[aria-sort="ascending"] button')?.getAttribute('data-sort')).toBe('agent');
  });
  it('shows an unknown story and an existing story with no sessions distinctly',async()=>{
    boot(vi.fn().mockResolvedValue(story({tasks:[],sessions:[]})));
    await settle();expect(document.body.textContent).toContain('No recorded work for SV-X yet');
  });
  it.each([true,false])('catches up hidden nudges, before first read resolves=%s',async early=>{
    let resolve:(v:unknown)=>void=()=>{};
    const read=vi.fn().mockImplementationOnce(()=>new Promise(r=>{resolve=r;})).mockResolvedValue(story());
    const h=boot(read);await settle();
    if(!early){resolve(story());await settle();}
    h.show(false);
    window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));
    window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:worktrees-changed'}}));
    if(early){resolve(story());await settle();}
    expect(read).toHaveBeenCalledTimes(1);
    h.show(true);await settle();expect(read).toHaveBeenCalledTimes(2);
    h.cs.subscribe.mock.calls[0][1].onChange();await settle();expect(read).toHaveBeenCalledTimes(3);
  });
  it('keeps prior rows on later failure and Retry recovers',async()=>{
    const read=vi.fn().mockResolvedValueOnce(story()).mockRejectedValueOnce(new Error('read failed')).mockResolvedValue(story());
    boot(read);await settle();window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();
    expect(document.querySelector('[data-session="lead"]')).not.toBeNull();
    expect(document.body.textContent).toContain('read failed');
    document.querySelector<HTMLButtonElement>('[data-retry]')!.click();await settle();
    expect(document.querySelector('[data-retry]')).toBeNull();
  });
  it('supports tree arrow/Home/End keys and the two-button session grouping',async()=>{
    boot();await settle();
    const tree=document.querySelector('[role=tree]')!;
    const first=tree.querySelector<HTMLElement>('[role=treeitem]')!;first.focus();
    first.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true}));
    expect(document.activeElement?.getAttribute('data-task')).toBe('lead');
    document.activeElement!.dispatchEvent(new KeyboardEvent('keydown',{key:'Home',bubbles:true}));
    expect(document.activeElement?.getAttribute('aria-expanded')).toBe('true');
    document.activeElement!.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowLeft',bubbles:true}));
    expect(document.activeElement?.getAttribute('aria-expanded')).toBe('false');
    document.activeElement!.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
    document.activeElement!.dispatchEvent(new KeyboardEvent('keydown',{key:'End',bubbles:true}));
    expect(document.activeElement?.getAttribute('data-task')).toBe('lead');
    document.querySelector<HTMLButtonElement>('[data-group="task"]')!.click();
    expect(document.querySelector('[data-group="task"]')?.getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelectorAll('[data-session]')).toHaveLength(1);
    expect(document.querySelector('[data-sort]')).toBeNull();
    document.querySelector<HTMLButtonElement>('[data-group="time"]')!.click();
    expect(document.querySelector('[data-sort]')).not.toBeNull();
  });
  it('routes the real location hash and hashchange entry',async()=>{
    window.location.hash='#story/SV-X';
    const h=boot(undefined,undefined,true);await settle();
    expect(h.cs.storyChanges).toHaveBeenCalledWith({storyId:'SV-X'});
    window.location.hash='#session/lead/turn/3';
    window.dispatchEvent(new Event('hashchange'));await settle();
    expect(document.querySelector('h1')?.textContent).toBe('Conversation');
    window.location.hash='#story/SV-X';window.dispatchEvent(new Event('hashchange'));await settle();
    expect(document.querySelector('h1')?.textContent).toBe('A story');
  });
  it('uses kit loading rows and shows no linked sessions without claiming a price',async()=>{
    let resolve:(v:unknown)=>void=()=>{};
    boot(vi.fn().mockImplementation(()=>new Promise(r=>{resolve=r;})));await settle();
    expect(document.querySelectorAll('.cs-skeleton-row')).toHaveLength(4);
    resolve(story({sessions:[],totals:{sessions:0,costUsd:null,wallMs:0,activeMs:0,idleMs:0,reviewRounds:0},unavailable:{costUsd:'no priced sessions'}}));await settle();
    expect(document.body.textContent).toContain('No linked sessions');
    expect(document.body.textContent).not.toContain('$0');
  });
  it('saves a user-entered rate through setModelRate',async()=>{
    const h=boot(vi.fn().mockResolvedValue(story({sessions:[session('lead',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'new-model',costUsd:null,costSource:null}]})]})));await settle();
    document.querySelector<HTMLButtonElement>('[data-rate]')!.click();
    const form=document.querySelector<HTMLFormElement>('#rate-form')!;
    (form.elements.namedItem('inputPerMtok') as HTMLInputElement).value='1.5';
    (form.elements.namedItem('outputPerMtok') as HTMLInputElement).value='6';
    form.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
    expect(h.cs.setModelRate).toHaveBeenCalledWith({modelPrefix:'new-model',effectiveFrom:'1969-12-31',inputPerMtok:1.5,outputPerMtok:6});
  });
  it('preserves selection across the session route and back',async()=>{
    const h=boot();await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();
    h.app.navigate('#session/lead/turn/3');await settle();expect(document.body.textContent).toContain('Conversation');
    h.app.navigate('#story/SV-X');await settle();expect(document.querySelector('[data-session="lead"]')?.getAttribute('aria-selected')).toBe('true');
  });
});

it('sorts real row values ascending and descending for every column',async()=>{
  const make=(id:string,agent:string,cost:number,start:number)=>{
    const row=session(id);row.session.agent=agent;row.cost.totalUsd=cost;row.session.timing.startedAtMs=start;return row;
  };
  boot(vi.fn().mockResolvedValue(story({sessions:[make('a','Zed',4,3000),make('b','Ada',6,1000),make('c','Mary',2,2000)]})));await settle();
  const ids=()=>[...document.querySelectorAll('[data-session]')].map(e=>e.getAttribute('data-session'));
  const click=(key:string)=>document.querySelector<HTMLButtonElement>(`[data-sort="${key}"]`)!.click();
  expect(ids()).toEqual(['b','c','a']);click('when');expect(ids()).toEqual(['a','c','b']);
  click('agent');expect(ids()).toEqual(['b','c','a']);click('agent');expect(ids()).toEqual(['a','c','b']);
  click('cost');expect(ids()).toEqual(['c','a','b']);click('cost');expect(ids()).toEqual(['b','a','c']);
});
it('retains rate drafts, task focus and panel focus across refreshes',async()=>{
  const data=story();data.sessions[0].cost={state:'absent',totalUsd:null,source:null,reason:'unpriced'};
  data.sessions[0].models=[{model:'new-model',costUsd:null,costSource:null}];
  const h=boot(vi.fn().mockResolvedValue(data));await settle();
  const nudge=async()=>{window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();};
  document.querySelector<HTMLElement>('[data-task="lead"]')!.focus();await nudge();
  expect(document.activeElement?.getAttribute('data-task')).toBe('lead');
  document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await nudge();
  expect(document.activeElement?.hasAttribute('data-close')).toBe(true);
  document.querySelector<HTMLButtonElement>('[data-close]')!.click();
  document.querySelector<HTMLButtonElement>('[data-rate]')!.click();
  const input=document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!;input.value='12';input.focus();
  await nudge();expect(document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value).toBe('12');
  expect(document.activeElement).toBe(input);
  expect(h.cs.storyChanges).toHaveBeenCalledTimes(4);
});
it('invalidates cached stories while on the conversation route and preserves scroll on back',async()=>{
  const h=boot();await settle();
  const scroll=vi.spyOn(window,'scrollTo').mockImplementation(()=>{});
  const y=vi.spyOn(window,'scrollY','get').mockReturnValue(320);
  h.app.navigate('#session/lead');
  window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();
  expect(h.cs.storyChanges).toHaveBeenCalledTimes(1);
  y.mockReturnValue(0);h.app.navigate('#story/SV-X');await settle();
  expect(h.cs.storyChanges).toHaveBeenCalledTimes(2);
  expect(scroll).toHaveBeenCalledWith(0,320);scroll.mockRestore();y.mockRestore();
});
it('switches rate models explicitly and returns to a stable session after pricing',async()=>{
  const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'model-a',costUsd:null,costSource:null}]});
  const b=session('b',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'model-b',costUsd:null,costSource:null}]});
  const data=story({sessions:[a,b]});
  const read=vi.fn().mockResolvedValueOnce(data).mockResolvedValue(story({sessions:[session('a'),session('b')]}));
  boot(read);await settle();
  document.querySelector<HTMLButtonElement>('[data-rate="model-a"]')!.click();
  document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value='2';
  document.querySelector<HTMLButtonElement>('[data-rate="model-b"]')!.click();
  expect(document.querySelector<HTMLInputElement>('[name="modelPrefix"]')!.value).toBe('model-b');
  const form=document.querySelector<HTMLFormElement>('#rate-form')!;
  form.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
  expect(document.activeElement?.getAttribute('data-open')).toBe('b');
  expect(document.body.textContent).toContain('Rate saved.');
});

it('announces native pending acceptance honestly and shows its next step',async()=>{
  const data=story({sessions:[session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'new',costUsd:null,costSource:null}]})]});
  const h=boot(vi.fn().mockResolvedValue(data));h.cs.setModelRate.mockResolvedValue(0);await settle();
  document.querySelector<HTMLButtonElement>('[data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
  expect(document.body.textContent).toContain('If acceptance is required');expect(document.body.textContent).toContain('Spend cap');expect(document.body.textContent).not.toContain('Rate saved.');
});
it('defaults effective-from to an evening Pacific local day',async()=>{
  vi.stubEnv('TZ','America/Los_Angeles');vi.useFakeTimers({toFake:['Date']});vi.setSystemTime(new Date('2026-10-27T20:30:00-07:00'));
  const row=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'new',costUsd:null,costSource:null}]});row.session.timing.startedAtMs=new Date('2026-10-27T20:00:00-07:00').getTime();
  boot(vi.fn().mockResolvedValue(story({sessions:[row]})));await settle();document.querySelector<HTMLButtonElement>('[data-rate]')!.click();
  expect(document.querySelector<HTMLInputElement>('[name="effectiveFrom"]')!.value).toBe('2026-10-27');vi.useRealTimers();vi.unstubAllEnvs();
});
it('an old save cannot discard a newer model draft',async()=>{
  const make=(id:string)=>session(id,{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:id,costUsd:null,costSource:null}]});
  const h=boot(vi.fn().mockResolvedValue(story({sessions:[make('a'),make('b')]})));await settle();
  let finish:(value:number)=>void=()=>{};h.cs.setModelRate.mockImplementationOnce(()=>new Promise(r=>{finish=r;}));
  document.querySelector<HTMLButtonElement>('[data-rate="a"]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));
  document.querySelector<HTMLButtonElement>('[data-cancel-rate]')!.click();document.querySelector<HTMLButtonElement>('[data-rate="b"]')!.click();
  document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value='9';finish(1);await settle();
  expect(document.querySelector<HTMLInputElement>('[name="modelPrefix"]')!.value).toBe('b');expect(document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value).toBe('9');
});
it('preserves the exact duplicate-model rate opener on refresh and Cancel',async()=>{
  const data=story();data.sessions[0].cost={state:'absent',totalUsd:null,source:null,reason:'unpriced'};data.sessions[0].models=[{model:'new',costUsd:null,costSource:null}];
  boot(vi.fn().mockResolvedValue(data));await settle();
  document.querySelector<HTMLButtonElement>('[data-session="lead"] [data-rate="new"]')!.focus();
  window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();
  expect(document.activeElement?.closest('[data-session]')?.getAttribute('data-session')).toBe('lead');
  (document.activeElement as HTMLButtonElement).click();document.querySelector<HTMLButtonElement>('[data-cancel-rate]')!.click();
  expect(document.activeElement?.closest('[data-session]')?.getAttribute('data-session')).toBe('lead');
});
it.each(['Enter',' '])('toggles tree disclosure with %s',async key=>{
  boot();await settle();const item=document.querySelector<HTMLElement>('[role=treeitem]')!;item.focus();item.dispatchEvent(new KeyboardEvent('keydown',{key,bubbles:true,cancelable:true}));
  expect(document.activeElement?.getAttribute('aria-expanded')).toBe('false');
});
it('shows no recorded work without asserting story absence, and retains unknown-cost reason',async()=>{
  boot(vi.fn().mockResolvedValue(story({tasks:[],sessions:[],unavailable:{costUsd:'no sessions with priced usage for this story'}})));await settle();
  expect(document.body.textContent).toContain('No recorded work for SV-X yet');expect(document.body.textContent).toContain('No linked sessions');expect(document.body.textContent).toContain('no sessions with priced usage for this story');
});
it('keeps the unknown cost reason when a root task exists without sessions',async()=>{
  boot(vi.fn().mockResolvedValue(story({sessions:[],unavailable:{costUsd:'no sessions with priced usage for this story'}})));await settle();
  expect(document.querySelector('.tot')?.textContent).toContain('no sessions with priced usage for this story');
});
it('reopening the same model during a save creates an enabled draft and ignores stale completion',async()=>{
  const data=story();data.sessions[0].cost={state:'absent',totalUsd:null,source:null,reason:'unpriced'};data.sessions[0].models=[{model:'new',costUsd:null,costSource:null}];
  const h=boot(vi.fn().mockResolvedValue(data));await settle();let done:(v:number)=>void=()=>{};h.cs.setModelRate.mockImplementationOnce(()=>new Promise(r=>{done=r;}));
  document.querySelector<HTMLButtonElement>('[data-session="lead"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));
  document.querySelector<HTMLButtonElement>('[data-task="lead"] [data-rate]')!.click();
  expect(document.querySelector<HTMLButtonElement>('[type="submit"]')!.disabled).toBe(false);document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value='8';done(1);await settle();expect(document.querySelector<HTMLInputElement>('[name="inputPerMtok"]')!.value).toBe('8');
});
it('clears conditional rate guidance after native acceptance and after Retry',async()=>{
  const unpriced=story({sessions:[session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]})]});
  const read=vi.fn().mockResolvedValue(unpriced),h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();document.querySelector<HTMLButtonElement>('[data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
  expect(document.body.textContent).toContain('If acceptance is required');read.mockResolvedValue(story({sessions:[session('a')]}));document.querySelector<HTMLButtonElement>('[data-retry]')!.click();await settle();expect(document.body.textContent).not.toContain('If acceptance is required');expect(document.body.textContent).not.toContain('waiting for the app');
});
it('keeps horizontal and vertical section scroll across activity refresh',async()=>{
  boot();await settle();const tree=document.querySelector<HTMLElement>('.treewrap')!,table=document.querySelector<HTMLElement>('.table-wrap')!;tree.scrollLeft=250;tree.scrollTop=10;table.scrollLeft=350;table.scrollTop=20;
  window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();expect(document.querySelector('.treewrap')?.scrollLeft).toBe(250);expect(document.querySelector('.treewrap')?.scrollTop).toBe(10);expect(document.querySelector('.table-wrap')?.scrollLeft).toBe(350);expect(document.querySelector('.table-wrap')?.scrollTop).toBe(20);
});
it('moves the roving tab stop when a leaf is clicked',async()=>{
  boot();await settle();document.querySelector<HTMLElement>('[data-task="lead"] .tree-line')?.click();
  const leaf=document.querySelector<HTMLElement>('[data-task="lead"]')!;leaf.querySelector<HTMLElement>('.tg')!.click();expect(leaf.tabIndex).toBe(0);expect(document.querySelector('[data-task="story:SV-X"]')?.getAttribute('tabindex')).toBe('-1');
});
it('an already-priced sibling with the same model cannot clear the requesting row guidance',async()=>{
  const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]});
  const h=boot(vi.fn().mockResolvedValue(story({sessions:[a,session('b')]})));h.cs.setModelRate.mockResolvedValue(0);await settle();
  document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();expect(document.body.textContent).toContain('If acceptance is required');
  document.querySelector<HTMLButtonElement>('[data-retry]')!.click();await settle();expect(document.body.textContent).toContain('If acceptance is required');
});
it('Retry keeps guidance while the requesting row is unpriced, then clears it when that row is priced',async()=>{
  const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]});
  const read=vi.fn().mockResolvedValue(story({sessions:[a]})),h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();document.querySelector<HTMLButtonElement>('[data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
  document.querySelector<HTMLButtonElement>('[data-retry]')!.click();await settle();expect(document.body.textContent).toContain('If acceptance is required');
  read.mockResolvedValue(story({sessions:[session('a')]}));document.querySelector<HTMLButtonElement>('[data-retry]')!.click();await settle();expect(document.body.textContent).not.toContain('If acceptance is required');
});

it('restores the requesting row when pricing removes focused Retry',async()=>{
 const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]});
 const read=vi.fn().mockResolvedValue(story({sessions:[a]})),h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();
 document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();
 read.mockResolvedValue(story({sessions:[session('a')]}));const retry=document.querySelector<HTMLButtonElement>('[data-retry]')!;retry.focus();retry.click();await settle();
 expect(document.activeElement?.getAttribute('data-open')).toBe('a');
});

it.each(['rate','save','cancel','retry','task','sort','group','details'])('keeps focus intent through %s refresh and removal',async(kind)=>{
 const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]});
 const read=vi.fn().mockResolvedValue(story({sessions:[a]})),h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();
 let selector=({'rate':'[data-session="a"] [data-rate]','save':'[type=submit]','cancel':'[data-cancel-rate]','retry':'[data-retry]','task':'[data-task="lead"]','sort':'[data-sort="cost"]','group':'[data-group="task"]','details':'[data-open="a"]'} as Record<string,string>)[kind];
 if(['save','cancel','retry'].includes(kind)){document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();if(kind==='retry'){document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();}}
 document.querySelector<HTMLElement>(selector)!.focus();
 read.mockResolvedValue(story({sessions:kind==='details'?[]:[session('a')],...(kind==='task'?{tasks:[]}: {})}));
 window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();
 expect(document.activeElement).not.toBe(document.body);
 if(kind==='cancel')document.querySelector<HTMLButtonElement>(selector)!.click();
 if(kind==='save')document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));
 await settle();expect(document.activeElement).not.toBe(document.body);
 if(['cancel','save','rate','retry'].includes(kind))expect(document.activeElement?.getAttribute('data-open')).toBe('a');
 if(kind==='details')expect(document.activeElement?.id).toBe('sessions-title');
 if(kind==='task')expect(document.activeElement?.id).toBe('tasks-title');
});
it('uses the section heading when a removed draft row is cancelled',async()=>{
 const a=session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]});const read=vi.fn().mockResolvedValue(story({sessions:[a]}));boot(read);await settle();
 document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();document.querySelector<HTMLElement>('[data-cancel-rate]')!.focus();read.mockResolvedValue(story({sessions:[]}));window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();document.querySelector<HTMLButtonElement>('[data-cancel-rate]')!.click();expect(document.activeElement?.id).toBe('sessions-title');
});

it('patches changed cost and timing through kit update without replacing the row',async()=>{
 const first=story(),read=vi.fn().mockResolvedValue(first);const h=boot(read);await settle();const row=document.querySelector('[data-session="lead"]')!;const opener=row.querySelector('[data-open]');
 const next=story({sessions:[session('lead',{cost:{state:'present',totalUsd:9,source:'reported'},session:{...session('lead').session,timing:{...session('lead').session.timing,wallMs:120000}}})]});read.mockResolvedValue(next);h.cs.subscribe.mock.calls[0][1].onChange();await settle();expect(document.querySelector('[data-session="lead"]')).toBe(row);expect(row.querySelector('[data-open]')).toBe(opener);expect(row.textContent).toContain('$9.00');expect(row.textContent).toContain('2m');
});

it.each(['conversation','another-story'])('reconciles a priced read against its owning cached story while on %s',async destination=>{
 const unpriced=(id:string)=>story({sessions:[session(id,{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]})]});
 let hold=false,priced=false,finish:(data:unknown)=>void=()=>{};
 const read=vi.fn().mockImplementation(args=>args.storyId==='SV-Y'?Promise.resolve(unpriced('b')):hold?new Promise(resolve=>{finish=resolve;}):Promise.resolve(priced?story({sessions:[session('a')]}):unpriced('a')));
 const h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();expect(document.querySelector('#notice')?.textContent).toContain('If acceptance is required');
 hold=true;h.cs.subscribe.mock.calls[0][1].onChange();await settle();h.app.navigate(destination==='conversation'?'#session/a':'#story/SV-Y');await settle();
 if(destination==='another-story'){document.querySelector<HTMLButtonElement>('[data-session="b"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));await settle();expect(document.querySelector('#notice')?.textContent).toContain('If acceptance is required');}
 hold=false;priced=true;finish(story({sessions:[session('a')]}));await settle();if(destination==='another-story')expect(document.querySelector('#notice')?.textContent).toContain('If acceptance is required');h.app.navigate('#story/SV-X');expect(document.querySelector('[data-session="a"]')?.textContent).toContain('$2.00');expect(document.querySelector('#notice')?.textContent).not.toContain('If acceptance is required');expect(document.querySelector('[data-retry]')).toBeNull();
});
it.each(['resolved','failed'])('applies a rate write %s result to its route while away',async outcome=>{
 const data=story();data.sessions[0].cost={state:'absent',totalUsd:null,source:null,reason:'unpriced'};data.sessions[0].models[0].costUsd=null;const h=boot(vi.fn().mockResolvedValue(data));let resolve:(n:number)=>void=()=>{},reject:(e:Error)=>void=()=>{};h.cs.setModelRate.mockImplementationOnce(()=>new Promise((ok,no)=>{resolve=ok;reject=no;}));await settle();document.querySelector<HTMLButtonElement>('[data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));h.app.navigate('#session/lead');outcome==='resolved'?resolve(1):reject(new Error('rate failed'));await settle();h.app.navigate('#story/SV-X');expect(document.body.textContent).not.toContain('Saving rate…');expect(document.body.textContent).toContain(outcome==='resolved'?'Rate saved.':'rate failed');
});
it('an older priced read cannot accept a newer rate request',async()=>{
 const unpriced=story({sessions:[session('a',{cost:{state:'absent',totalUsd:null,source:null,reason:'unpriced'},models:[{model:'gpt-model',costUsd:null,costSource:null}]})]});const read=vi.fn().mockResolvedValue(unpriced),h=boot(read);h.cs.setModelRate.mockResolvedValue(0);await settle();
 const submit=()=>{document.querySelector<HTMLButtonElement>('[data-session="a"] [data-rate]')!.click();document.querySelector('form')!.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));};submit();await settle();let old:(data:unknown)=>void=()=>{},latest:(data:unknown)=>void=()=>{};read.mockImplementationOnce(()=>new Promise(resolve=>{old=resolve;})).mockImplementationOnce(()=>new Promise(resolve=>{latest=resolve;}));h.cs.subscribe.mock.calls[0][1].onChange();await settle();submit();await settle();old(story({sessions:[session('a')]}));await settle();expect(document.querySelector('#notice')?.textContent).toContain('If acceptance is required');latest(unpriced);await settle();expect(document.querySelector('#notice')?.textContent).toContain('If acceptance is required');
});

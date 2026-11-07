// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import cases from './pr4-case-table.json';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {contributionStory,contributionCommit} from './contribution-fixtures.mjs';
import {session} from './fixtures.mjs';
import {conversation,turn} from './conversation-fixtures.mjs';
const stops:(()=>void)[]=[];
afterEach(()=>{stops.splice(0).forEach(stop=>stop());document.body.replaceChildren();window.history.replaceState(null,'','/');});
const flush=async()=>{for(let i=0;i<80;i++)await Promise.resolve();};
const A='a'.repeat(40),B='b'.repeat(40),C='c'.repeat(40);
const LIMIT='span must be 1-based, inclusive, ordered, and end - start <= 2000';
// StoryChanges with PR files (story_changes::git pr["files"]) and include:["ranges"] commit evidence.
function storyData(extra:Record<string,unknown>={}){
 const data=contributionStory({sourceRepo:'example/project',...extra});
 data.prs=data.prs.map((pr:Record<string,unknown>)=>({...pr,files:['src/app.ts','tests/app.test.ts']}));
 return data;
}
const storyRange=(path:string,start:number,end:number)=>({path,start,end,commit:A,basis:'exact'});
const rangesStory=(extra:Record<string,unknown>={})=>storyData({commits:[contributionCommit(A,{files:['src/app.ts','tests/app.test.ts'],ranges:[storyRange('src/app.ts',2,3),storyRange('src/app.ts',5,5),storyRange('tests/app.test.ts',10,12)]})],...extra});
// why_line ranges after the session and turn joins (attribution::why_line, attribution::turns).
const exact=(start:number,end:number,index:number|null,extra:Record<string,unknown>={})=>({start,end,commit:A,traceId:'trace',tool:'claude',nativeSessionId:'native-lead',model:'claude-sonnet-5-5',basis:'exact',sessionId:'lead',link:'linked',story:'SV-X',turn:index==null?null:{sessionId:'lead',toolUseId:'toolu_1',turn:index},...(index==null?{unavailable:{turn:'edit not recorded by git-ai'}}:{}),...extra});
const why=(ranges:unknown[],sessions:Record<string,unknown>={lead:session('lead')},path='src/app.ts')=>({repo:'example/project',rev:B,resolvedCommit:B,path,notesRef:'sense=1',ranges,sessions});
function deferred(){let resolve!:(value:unknown)=>void;const promise=new Promise(done=>{resolve=done;});return {promise,resolve};}
function mount(initial='#story/SV-X',data=storyData()){
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 let hash=initial,nudge=()=>{};
 const cs={kit:trailTestKit(),
  storyChanges:vi.fn(async(args:{include?:string[]})=>args.include?rangesStory({prs:data.prs,branches:data.branches}):data),
  whyLine:vi.fn().mockResolvedValue(why([exact(2,3,4),exact(5,5,6)])),
  sessionRead:vi.fn().mockResolvedValue({...session('lead'),conversation:conversation({turns:[0,1,2,3,4,5,6].map(index=>turn(index)),turnCount:7})}),sessions:vi.fn().mockResolvedValue([]),
  openDiff:vi.fn().mockResolvedValue({status:'opened'}),openFile:vi.fn().mockResolvedValue({}),openWorkspace:vi.fn().mockResolvedValue({}),
  subscribe:vi.fn((_topic:string,options:{onChange:()=>void})=>{nudge=options.onChange;return Promise.resolve({});}),unsubscribe:vi.fn(),onVisibility:(show:(v:boolean)=>void)=>{show(true);return ()=>{};}};
 const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);
 return {cs,nudge:()=>nudge(),route:(next:string)=>{hash=next;app.route(next);}};
}
const q=<T extends HTMLElement=HTMLElement>(selector:string)=>document.querySelector<T>(selector)!;
function text(selector:string){const node=q(selector);expect(node,selector).not.toBeNull();return node.textContent??'';}
function click(selector:string){const node=q(selector);expect(node,selector).not.toBeNull();node.click();}
const panel=()=>q('#drill-panel');
const parts=(node:Element)=>[...node.children].map(child=>child.textContent?.replace(/\s+/g,' ').trim()).filter(Boolean).join(' ');
const rows=()=>[...panel().querySelectorAll('[data-lines-range]')].map(parts);
async function pick(h:ReturnType<typeof mount>,path='src/app.ts'){await flush();click(`[data-lines-path="${path}"]`);await flush();return h;}
function submit(start:string,end:string){q<HTMLInputElement>('#lines-form [name=linesStart]').value=start;q<HTMLInputElement>('#lines-form [name=linesEnd]').value=end;q<HTMLFormElement>('#lines-form').requestSubmit();}

it.each(cases)('PR4 $id — $name',async row=>{
 switch(row.id){
 case 1:{const h=await pick(mount());expect(panel().hidden).toBe(false);expect(panel().getAttribute('role')).toBe('complementary');expect(text('#panel-title')).toBe('Who changed lines 2–5');expect(text('[data-lines-summary]')).toBe('This story changed 3 of these 4 lines.');
  const group=q('[data-lines-group="session:lead"]');expect(group.getAttribute('role')).toBe('group');expect(group.querySelector('b')?.textContent).toBe('builder@abc');expect(group.querySelector('small')?.textContent).toBe('claude-sonnet-5-5');
  expect(rows()).toEqual(['Lines 2–3 · Written in turn 4 · aaaaaaa · exact Open conversation at turn 4','Line 5 · Written in turn 6 · aaaaaaa · exact Open conversation at turn 6']);
  const open=group.querySelector<HTMLElement>('[data-turn="4"]')!;expect(open.dataset.conversation).toBe('lead');expect(h.cs.whyLine).toHaveBeenCalledWith({repo:'example/project',rev:B,path:'src/app.ts',start:2,end:5});break;}
 case 2:{const h=mount();h.cs.whyLine.mockResolvedValue(why([exact(2,3,null)]));await pick(h);expect(rows()).toEqual(['Lines 2–3 · aaaaaaa · exact · edit not recorded by git-ai Open conversation']);const open=q('[data-lines-group="session:lead"] [data-conversation]');expect(open.dataset.conversation).toBe('lead');expect(open.hasAttribute('data-turn')).toBe(false);break;}
 case 3:{const h=mount();h.cs.whyLine.mockResolvedValue(why([{start:2,end:5,commit:C,basis:'associated',tool:null,nativeSessionId:null,model:null,sessionId:null,link:'unlinked',story:'SV-X',turn:null,candidateSessions:['lead','other'],reason:'story association only; no note on introducing commit'}],{}));await pick(h);
  expect(text('[data-lines-associated]')).toContain('Who changed each line isn’t recorded yet');expect(text('[data-lines-associated]')).toContain('One of them may have changed lines 2–5.');
  const candidates=[...panel().querySelectorAll<HTMLElement>('[data-lines-candidate]')];expect(candidates.map(parts)).toEqual(['builder@abc · associated Open conversation','other · associated Open conversation']);
  expect(q('[data-lines-summary]').hidden).toBe(true);expect(panel().querySelectorAll('[data-lines-group]')).toHaveLength(0);expect(panel().querySelector('[data-turn]')).toBeNull();break;}
 case 4:{const h=mount();h.cs.whyLine.mockResolvedValue(why([{start:2,end:3,commit:A,basis:'unknown',tool:null,nativeSessionId:null,model:null,sessionId:null,link:'unlinked',story:null,turn:null,reason:'no git-ai note on introducing commit'},{start:4,end:5,commit:C,basis:'exact',tool:'human',nativeSessionId:null,model:null,sessionId:null,link:'unlinked',story:null,turn:null,reason:'human'}],{}));await pick(h);
  expect(rows()).toEqual(['Lines 2–3 · aaaaaaa · no git-ai note on introducing commit','Lines 4–5 · ccccccc · human']);expect(panel().querySelector('[data-lines-range] button')).toBeNull();expect(panel().querySelector('[data-conversation]')).toBeNull();break;}
 case 5:{const h=mount();h.cs.whyLine.mockResolvedValue({status:'unavailable',reason:'revision not in local copy'});await pick(h);expect(text('[data-lines-unavailable]')).toBe('revision not in local copy');expect(q('[data-lines-summary]').hidden).toBe(true);expect(rows()).toEqual([]);expect(panel().querySelector('[data-conversation]')).toBeNull();break;}
 case 6:{const h=await pick(mount());expect(h.cs.whyLine).toHaveBeenCalledTimes(1);submit('1','2002');await flush();expect(text('[data-lines-refused]')).toBe(LIMIT);expect(h.cs.whyLine).toHaveBeenCalledTimes(1);submit('9','3');await flush();expect(text('[data-lines-refused]')).toBe(LIMIT);expect(h.cs.whyLine).toHaveBeenCalledTimes(1);submit('1','2001');await flush();expect(q('[data-lines-refused]').hidden).toBe(true);expect(h.cs.whyLine).toHaveBeenCalledTimes(2);expect(h.cs.whyLine).toHaveBeenLastCalledWith(expect.objectContaining({start:1,end:2001}));break;}
 case 7:{const h=await pick(mount());expect(h.cs.storyChanges).toHaveBeenCalledWith({storyId:'SV-X',include:['ranges']});expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('2');expect(q<HTMLInputElement>('[name=linesEnd]').value).toBe('5');click('[data-close]');await pick(h,'tests/app.test.ts');expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('10');expect(q<HTMLInputElement>('[name=linesEnd]').value).toBe('12');expect(h.cs.whyLine).toHaveBeenLastCalledWith(expect.objectContaining({path:'tests/app.test.ts',start:10,end:12}));break;}
 case 8:{const merged=await pick(mount());expect(merged.cs.whyLine.mock.calls[0][0].rev).toBe(B);expect(text('[data-lines-rev]')).toBe('Reads the merged change (bbbbbbb)');stops.pop()?.();
  const open=await pick(mount('#story/SV-X',storyData({prs:[]})));expect(open.cs.whyLine.mock.calls[0][0].rev).toBe('feat/SV-X');expect(text('[data-lines-rev]')).toBe('Reads branch feat/SV-X');break;}
 case 9:{const h=await pick(mount());click('[data-lines-group] [data-turn="4"]');await flush();expect(h.cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#session/lead/turn/4',newTab:true});h.route('#session/lead/turn/4');await flush();expect(q('#turn-4').classList.contains('highlight')).toBe(true);expect(document.querySelectorAll('.turn.highlight')).toHaveLength(1);break;}
 case 10:{const h=await pick(mount());click('[data-lines-diff]');await flush();expect(h.cs.openDiff).toHaveBeenCalledWith({repo:'example/project',rev:B,path:'src/app.ts',storyId:'SV-X',range:{start:2,end:5}});h.cs.openDiff.mockResolvedValue({status:'commitNotLocal',reason:"This commit isn't on this Mac yet",fetchCommand:'git fetch origin '+B});click('[data-lines-diff]');await flush();expect(panel().querySelector('[data-diff-result]')?.textContent).toContain("This commit isn't on this Mac yet");break;}
 case 11:{const h=mount(),first=deferred(),second=deferred();h.cs.whyLine.mockImplementationOnce(()=>first.promise).mockImplementationOnce(()=>second.promise);await flush();click('[data-lines-path="src/app.ts"]');await flush();click('[data-lines-path="tests/app.test.ts"]');await flush();expect(h.cs.whyLine).toHaveBeenCalledTimes(2);
  first.resolve(why([exact(2,3,4)]));await flush();expect(rows()).toEqual([]);expect(text('[data-lines-file]')).toBe('tests/app.test.ts');second.resolve(why([exact(10,12,9)],undefined,'tests/app.test.ts'));await flush();expect(rows()).toEqual(['Lines 10–12 · Written in turn 9 · aaaaaaa · exact Open conversation at turn 9']);expect(h.cs.whyLine).toHaveBeenCalledTimes(2);expect(h.cs.whyLine.mock.calls[1][0]).toMatchObject({path:'tests/app.test.ts',start:10,end:12});break;}
 case 12:{const h=await pick(mount());const control=q('[data-lines-group] [data-turn="4"]');h.cs.whyLine.mockResolvedValue(why([]));control.dispatchEvent(new MouseEvent('pointerdown',{bubbles:true,button:0}));h.nudge();await flush();expect(control.isConnected).toBe(true);expect(control.getAttribute('aria-disabled')).toBeNull();control.dispatchEvent(new MouseEvent('pointerup',{bubbles:true,button:0}));control.click();await flush();expect(h.cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#session/lead/turn/4',newTab:true});expect(h.cs.whyLine).toHaveBeenCalledTimes(2);await flush();expect(rows()).toEqual([]);expect(q<HTMLButtonElement>('#lines-form [type=submit]').getAttribute('aria-disabled')).toBeNull();break;}
 case 13:{const h=mount(),late=deferred();h.cs.whyLine.mockImplementationOnce(()=>late.promise);await pick(h);const away=q('[data-flat]');away.focus();expect(document.activeElement).toBe(away);late.resolve(why([exact(2,3,4)]));await flush();expect(rows()).toHaveLength(1);expect(document.activeElement).toBe(away);break;}
 case 14:{await pick(mount());expect(document.activeElement).toBe(q('#drill-panel [data-close]'));document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));expect(panel().hidden).toBe(true);expect(document.activeElement).toBe(q('[data-lines-path="src/app.ts"]'));
  click('[data-lines-path="src/app.ts"]');await flush();expect(panel().hidden).toBe(false);click('#drill-panel [data-close]');expect(panel().hidden).toBe(true);expect(document.activeElement).toBe(q('[data-lines-path="src/app.ts"]'));break;}
 case 15:{const h=mount();h.cs.whyLine.mockResolvedValue(why([exact(2,3,4,{sessionId:null,link:'unlinked',story:null,turn:null,unavailable:{turn:'edit not recorded by git-ai'},reason:'no session record (hand-started or before SV-CDE-8)'})],{}));await pick(h);
  const group=q('[data-lines-group="runtime:claude"]');expect(group.querySelector('b')?.textContent).toBe('claude');expect(group.querySelector('small')?.textContent).toBe('claude-sonnet-5-5 · session unknown');expect(rows()).toEqual(['Lines 2–3 · aaaaaaa · exact · no session record (hand-started or before SV-CDE-8)']);
  expect(panel().querySelector('[data-conversation]')).toBeNull();expect(panel().textContent).not.toContain('turn 4');expect(panel().textContent).not.toContain('builder@abc');expect(q('[data-lines-summary]').hidden).toBe(true);break;}
 case 16:{const h=mount(),late=deferred();h.cs.whyLine.mockImplementationOnce(()=>late.promise);const errors:unknown[]=[],record=(event:PromiseRejectionEvent)=>errors.push(event.reason);window.addEventListener('unhandledrejection',record);const onError=(error:unknown)=>errors.push(error);process.on('unhandledRejection',onError);
  try{await pick(h);click('#drill-panel [data-close]');late.resolve(why([exact(2,3,4)]));await flush();expect(errors).toEqual([]);expect(h.cs.whyLine).toHaveBeenCalledTimes(1);expect(h.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(1);expect(panel().hidden).toBe(true);
   stops.pop()?.();const fresh=mount(),pending=deferred();fresh.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>args.include?pending.promise:storyData());await pick(fresh);document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));expect(panel().hidden).toBe(true);pending.resolve(rangesStory());await flush();expect(errors).toEqual([]);expect(fresh.cs.whyLine).not.toHaveBeenCalled();expect(fresh.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(1);
   stops.pop()?.();const paged=mount(),first=deferred();paged.cs.storyChanges.mockImplementation(async(args:{include?:string[],cursor?:string})=>!args.include?storyData():args.cursor?storyData({commits:[contributionCommit(A,{ranges:[storyRange('tests/app.test.ts',4,4)]})],nextCursor:`${args.cursor}+`}):first.promise);await pick(paged,'tests/app.test.ts');click('#drill-panel [data-close]');first.resolve(storyData({commits:[],nextCursor:'p2'}));await flush();expect(errors).toEqual([]);expect(paged.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(1);expect(paged.cs.whyLine).not.toHaveBeenCalled();}
  finally{window.removeEventListener('unhandledrejection',record);process.off('unhandledRejection',onError);}break;}
 case 17:{const h=mount('#story/SV-X?lines=secret%2Fother.ts&start=1&end=3');await flush();expect(panel().hidden).toBe(false);expect(text('[data-lines-unavailable]')).toBe('Not a file this story changed.');expect(h.cs.whyLine).not.toHaveBeenCalled();expect(h.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(0);expect(q('[data-lines-diff]').hidden).toBe(true);stops.pop()?.();
  const prefill=mount('#story/SV-X?lines=secret%2Fother.ts');await flush();expect(text('[data-lines-unavailable]')).toBe('Not a file this story changed.');expect(prefill.cs.whyLine).not.toHaveBeenCalled();expect(prefill.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(0);break;}
 case 18:{const live=await pick(mount('#story/SV-X',storyData({prs:[]})));expect(q('[data-lines-diff]').dataset.diffRev).toBe('WORKTREE');click('[data-lines-diff]');await flush();expect(live.cs.openDiff).toHaveBeenCalledWith({repo:'example/project',rev:'WORKTREE',path:'src/app.ts',storyId:'SV-X',range:{start:2,end:5}});stops.pop()?.();
  const committed=await pick(mount('#story/SV-X',storyData({prs:[],worktrees:[{path:'/removed',state:'removed',basis:'exact'}],commits:[contributionCommit(C,{authoredAt:1793179000000}),contributionCommit(A)]})));expect(q('[data-lines-diff]').dataset.diffRev).toBe(C);click('[data-lines-diff]');await flush();expect(committed.cs.openDiff).toHaveBeenCalledWith(expect.objectContaining({rev:C,path:'src/app.ts'}));stops.pop()?.();
  await pick(mount('#story/SV-X',storyData({prs:[],worktrees:[],commits:[contributionCommit(C,{files:['other.ts']})]})));expect(q('[data-lines-diff]').hidden).toBe(true);break;}
 case 19:{const h=await pick(mount());expect(window.location.hash).toBe('#story/SV-X?lines=src%2Fapp.ts&start=2&end=5');h.route('#session/lead?from=0');await flush();window.history.replaceState(null,'','#session/lead?from=0');h.route('#story/SV-X');await flush();expect(panel().hidden).toBe(false);expect(window.location.hash).toBe('#story/SV-X?lines=src%2Fapp.ts&start=2&end=5');
  const replace=vi.spyOn(window.history,'replaceState');h.nudge();await flush();expect(h.cs.whyLine.mock.calls.length).toBeGreaterThan(1);expect(replace).not.toHaveBeenCalled();replace.mockRestore();break;}
 case 20:{const h=mount(),page=(cursor:string|null,ranges:unknown[])=>storyData({commits:[contributionCommit(A,{ranges})],nextCursor:cursor});
  h.cs.storyChanges.mockImplementation(async(args:{include?:string[],cursor?:string})=>!args.include?storyData():args.cursor==='p2'?page(null,[storyRange('tests/app.test.ts',7,8)]):page('p2',[storyRange('src/app.ts',2,3)]));
  await pick(h,'tests/app.test.ts');expect(h.cs.storyChanges.mock.calls.filter(([args])=>args.include).map(([args])=>args.cursor??null)).toEqual([null,'p2']);expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('7');expect(h.cs.whyLine).toHaveBeenLastCalledWith(expect.objectContaining({path:'tests/app.test.ts',start:7,end:8}));stops.pop()?.();
  const endless=mount();let n=0;endless.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>args.include?page(`p${++n}`,[]):storyData());await pick(endless);expect(endless.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(10);expect(q('[data-lines-hint]').hidden).toBe(false);expect(text('[data-lines-hint]')).toBe('Pick lines to see who changed them — ranges not all loaded.');expect(endless.cs.whyLine).not.toHaveBeenCalled();stops.pop()?.();
  const unranged=mount();unranged.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>args.include?page(null,[]):storyData());await pick(unranged);expect(text('[data-lines-hint]')).toBe('Pick lines to see who changed them.');break;}
 case 21:{const h=mount();h.cs.whyLine.mockResolvedValue(why([exact(2,3,4),exact(5,5,6,{story:'SV-OTHER'}),exact(7,7,8,{story:null})]));await pick(h);expect(rows()).toEqual(['Lines 2–3 · Written in turn 4 · aaaaaaa · exact Open conversation at turn 4','Line 5 · Written in turn 6 · aaaaaaa · exact · another story (SV-OTHER) Open conversation at turn 6','Line 7 · Written in turn 8 · aaaaaaa · exact · story unknown Open conversation at turn 8']);expect(text('[data-lines-summary]')).toBe('This story changed 2 of these 4 lines.');break;}
 case 22:{const branch=(name:string,extra:Record<string,unknown>)=>({...contributionStory().branches[0],name,...extra});
  const own=await pick(mount('#story/SV-X',storyData({prs:[],branches:[branch('feat/associated',{basis:'associated'}),branch('feat/missing',{availability:'missing'}),branch('feat/SV-X',{})]})));expect(own.cs.whyLine.mock.calls[0][0].rev).toBe('feat/SV-X');stops.pop()?.();
  const none=await pick(mount('#story/SV-X',storyData({prs:[],branches:[branch('feat/associated',{basis:'associated'}),branch('feat/remote',{availability:'remote'})]})));expect(none.cs.whyLine).not.toHaveBeenCalled();expect(text('[data-lines-unavailable]')).toBe('No merged change or local story branch is recorded for this file.');break;}
 case 23:{const h=mount();let fail=true;h.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>{if(!args.include)return storyData();if(fail){fail=false;throw new Error('ranges read failed');}return rangesStory();});await pick(h);expect(text('[data-lines-error]')).toContain('ranges read failed');expect(h.cs.whyLine).not.toHaveBeenCalled();
  click('[data-lines-retry]');await flush();expect(q('[data-lines-error]').hidden).toBe(true);expect(h.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(2);expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('2');expect(h.cs.whyLine).toHaveBeenCalledTimes(1);break;}
 case 24:{const h=mount(),prefill=deferred();h.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>args.include?prefill.promise:storyData());await pick(h);submit('3','4');await flush();expect(h.cs.whyLine).toHaveBeenCalledTimes(1);prefill.resolve(rangesStory());await flush();expect(h.cs.whyLine.mock.calls.map(([args])=>[args.start,args.end])).toEqual([[3,4],[3,4]]);expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('3');expect(window.location.hash).toBe('#story/SV-X?lines=src%2Fapp.ts&start=3&end=4');break;}
 case 25:{const h=await pick(mount());expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('2');click('#drill-panel [data-close]');h.cs.storyChanges.mockImplementation(async(args:{include?:string[]})=>args.include?storyData({commits:[contributionCommit(A,{ranges:[storyRange('src/app.ts',7,9)]})],nextCursor:null}):storyData());h.nudge();await flush();
  click('[data-lines-path="src/app.ts"]');await flush();expect(h.cs.storyChanges.mock.calls.filter(([args])=>args.include)).toHaveLength(2);expect(q<HTMLInputElement>('[name=linesStart]').value).toBe('7');expect(h.cs.whyLine).toHaveBeenLastCalledWith(expect.objectContaining({start:7,end:9}));break;}
 default: throw new Error(`Unhandled PR4 case ${row.id}`);
 }
});

it('entry: a Files row opens the line panel for its file',async()=>{
 await pick(mount());const opener=q('tr[data-row-focus="file:src/app.ts"] [data-lines-path]');expect(opener.getAttribute('aria-controls')).toBe('drill-panel');expect(text('[data-lines-file]')).toBe('src/app.ts');expect(panel().dataset.rowId).toBe('file:src/app.ts');expect(panel().getAttribute('aria-labelledby')).toBe('panel-title');
});
it('entry: the panel opens a conversation at its turn',async()=>{
 const h=await pick(mount());click('[data-lines-group] [data-turn="6"]');await flush();expect(h.cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#session/lead/turn/6',newTab:true});
});
it('entry: the panel opens the diff at its range',async()=>{
 const h=await pick(mount());submit('3','4');await flush();click('[data-lines-diff]');await flush();expect(h.cs.openDiff).toHaveBeenLastCalledWith({repo:'example/project',rev:B,path:'src/app.ts',storyId:'SV-X',range:{start:3,end:4}});
});
it('entry: a remount with the panel route reopens the same lines',async()=>{
 await pick(mount());const hash=window.location.hash;expect(hash).toBe('#story/SV-X?lines=src%2Fapp.ts&start=2&end=5');stops.pop()?.();
 const h=mount(hash);await flush();expect(panel().hidden).toBe(false);expect(h.cs.whyLine).toHaveBeenCalledWith({repo:'example/project',rev:B,path:'src/app.ts',start:2,end:5});expect(h.cs.storyChanges).not.toHaveBeenCalledWith(expect.objectContaining({include:['ranges']}));expect(rows()).toHaveLength(2);
 click('[data-close]');expect(window.location.hash).toBe('#story/SV-X');
});

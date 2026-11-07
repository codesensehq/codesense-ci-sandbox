// SPDX-License-Identifier: MIT
import {createElement} from 'react';
import {act,cleanup,render,screen} from '@testing-library/react';
import {afterEach,beforeEach,expect,it,vi} from 'vitest';
import cases from './pr3-case-table.json';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {contributionStory,contributionCommit} from './contribution-fixtures.mjs';
import {session} from './fixtures.mjs';
import {conversation} from './conversation-fixtures.mjs';
const host=vi.hoisted(()=>({readDiff:vi.fn(),open:vi.fn(),register:vi.fn(),review:vi.fn()}));
vi.mock('@/tauri/commands',async original=>({...await original<typeof import('@/tauri/commands')>(),openDiffRead:host.readDiff}));
vi.mock('@/shared/lib/open-file-in-team',()=>({openFileInTeam:host.open}));
vi.mock('@/shared/lib/open-management-tab',()=>({openManagementTab:host.open}));
vi.mock('@/shared/lib/virtual-viewer-registry',()=>({registerVirtualTab:host.register}));
vi.mock('@/shared/components/code-editor/CodeDiffReview',()=>({CodeDiffReview:(props:unknown)=>{host.review(props);return createElement('div',{'data-readonly-diff':''},'Read-only diff');}}));
import {HTML_BRIDGE_ALLOWLIST as bridge} from '@/features/dashboard/components/html-bridge';
const stops:(()=>void)[]=[];
const flush=async()=>{for(let i=0;i<60;i++)await Promise.resolve();};
beforeEach(()=>vi.clearAllMocks());
afterEach(()=>{stops.splice(0).forEach(stop=>stop());cleanup();document.body.replaceChildren();});
function mount(data=contributionStory({sourceRepo:'example/project'})){
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 let hash='#story/SV-X',nudge=()=>{};
 const cs={kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(data),sessionRead:vi.fn().mockResolvedValue({...session('lead'),conversation:conversation()}),openWorkspace:vi.fn().mockResolvedValue({}),openFile:vi.fn().mockResolvedValue({}),openDiff:vi.fn((args:unknown)=>{expect(bridge.openDiff,'openDiff is a registered page action').toBeTypeOf('function');return bridge.openDiff(args,{docPath:'/pane.html',frame:{frameId:'frame',grant:{team:{org:'example/project',team:'dev'}}}} as never);}),subscribe:vi.fn((_topic:string,options:any)=>{nudge=options.onChange;return Promise.resolve({});}),unsubscribe:vi.fn(),onVisibility:(show:(v:boolean)=>void)=>{show(true);return ()=>{};}};
 const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);
 return {cs,nudge:()=>nudge(),route:(next:string)=>{hash=next;app.route(next);}};
}
const q=(selector:string)=>document.querySelector<HTMLElement>(selector)!;
function text(selector:string){const node=q(selector);expect(node,selector).not.toBeNull();return node.textContent??'';}
function control(selector:string){const node=q(selector);expect(node,selector).not.toBeNull();return node;}
function click(selector:string){const node=q(selector);expect(node,selector).not.toBeNull();node.click();}
const opened={status:'opened',resolvedRev:'a'.repeat(40),redline:{before:'old\nsecond\n',after:'new\nsecond\n',binary:false,status:'modified'},files:[{path:'src/app.ts',status:'modified'}]};
async function openFromRow(h:ReturnType<typeof mount>,rev='a'.repeat(40)){
 await flush();click(`[data-diff-rev="${rev}"]`);await flush();expect(h.cs.openDiff).toHaveBeenCalledTimes(1);
}
async function mountDiffTab(){
 const [target]=host.open.mock.calls.at(-1)!;const filePath=typeof target==='string'?target:target.path;
 const component=host.register.mock.calls.at(-1)?.[2];expect(component).toBeTypeOf('function');
 await act(async()=>{render(createElement(component,{filePath}));});
 return filePath;
}

it.each(cases)('PR3 $id — $name',async row=>{
 const data=contributionStory({sourceRepo:'example/project'});
 switch(row.id){
 case 1:{mount(data);await flush();const header=q('[data-contribution-meta]');expect(header).not.toBeNull();expect(header.textContent).toMatch(/Merged.*PR #17.*branch feat\/SV-X/s);expect(text('[data-commits]')).toContain('aaaaaaa');expect(text('[aria-label="Files changed"]')).toContain('+12 −3');break;}
 case 2:{data.prs=[{...data.prs[0],source:'squash-subject',branch:null,commits:{unavailable:'squash-merged'},files:['src/app.ts']}];data.branches=[];data.commits=[];data.worktrees=[];const h=mount(data);await flush();expect(text('[aria-label="Files changed"]')).toContain('src/app.ts');expect(text('[data-changes]')?.split('branch commits not kept (squash-merged)')).toHaveLength(2);expect(document.querySelector(`tr[data-row-focus="file:tests/app.test.ts"] [data-diff-rev]`)).toBeNull();await openFromRow(h,'b'.repeat(40));expect(h.cs.openDiff.mock.calls[0][0]).toMatchObject({repo:'example/project',rev:'b'.repeat(40),path:'src/app.ts',storyId:'SV-X'});break;}
 case 3:{data.prs.push({...data.prs[0],number:18,mergedSha:'c'.repeat(40)});data.actual=[{...data.actual[0],lines:{added:20,removed:4}}];mount(data);await flush();expect(text('[aria-label="Files changed"]')).toContain('+20 −4');expect(text('[data-changes]')).toContain("lines changed by this story's merged PRs");break;}
 case 4:{data.prs=[];data.branches=[{...data.branches[0],name:'parent'},{...data.branches[0],name:'child'}];data.actual=[{...data.actual[0],lines:{added:8,removed:2}}];mount(data);await flush();expect(text('[aria-label="Files changed"]')).toContain('+8 −2');expect(text('[data-branches]')).toContain('child');break;}
 case 5: case 6:{const reason=row.id===5?'story spans branches with no single final tree':'binary';data.actual=[{...data.actual[0],lines:null,linesReason:reason}];data.unavailable.fileLines=reason;if(row.id===6)data.actual.push({...data.actual[0],path:'second.bin'});mount(data);await flush();expect(text('[data-lines-reason]')).toBe(reason);expect(text('[aria-label="Files changed"]')).toContain('unknown');expect(text('[aria-label="Files changed"]')).not.toContain('+0');if(row.id===6)expect(text('[data-changes]').split(reason)).toHaveLength(2);break;}
 case 7:{data.planned=null;data.plannedUntouched=[];data.unavailable.planned='story has no Touchpoints section';mount(data);await flush();expect(document.body.textContent?.split(data.unavailable.planned)).toHaveLength(2);expect([...document.querySelectorAll('[data-file-planned]')].map(node=>node.textContent)).toEqual(['','']);break;}
 case 8:{mount(data);await flush();expect(text('[data-untouched] h3')).toBe('Planned but not changed');expect(text('[data-untouched] summary')).toBe('+2 more');expect(q('[data-untouched] details').hasAttribute('open')).toBe(false);break;}
 case 9:{data.worktrees=[{path:'/removed',state:'removed',basis:'exact'}];mount(data);await flush();expect(text('[data-worktree-note]')).toContain('worktree was removed');expect(q('[data-session=lead]')).not.toBeNull();expect(q('[data-task=lead]')).not.toBeNull();expect(text('[data-session=lead]')).toContain('$');break;}
 case 10:{data.prs=[{...data.prs[0],url:null,urlReason:'origin is not GitHub'}];mount(data);await flush();const prs=q('[data-prs]');expect(prs?.textContent).toContain('PR #17');expect(prs.textContent).toContain('origin is not GitHub');expect(prs.querySelector('a')).toBeNull();break;}
 case 11:{const h=mount(data);await flush();h.cs.storyChanges.mockImplementation(async args=>args.cursor?contributionStory({commits:[contributionCommit('c'.repeat(40))],nextCursor:null}):data);click('[data-more-commits]');await flush();h.cs.storyChanges.mockResolvedValue(contributionStory({commits:[contributionCommit('d'.repeat(40))],nextCursor:'changed',total:3}));h.nudge();await flush();expect(text('[data-changes-error]')).toContain('Commit history changed');expect(text('[data-commits]')).toContain('aaaaaaa');click('[data-reload-commits]');await flush();expect(text('[data-commits]')).not.toContain('aaaaaaa');expect(text('[data-commits]')).toContain('ddddddd');break;}
 case 12:{host.readDiff.mockResolvedValue(opened);const h=mount(data);await openFromRow(h);await mountDiffTab();expect(host.review.mock.calls.at(-1)?.[0]).toMatchObject({before:opened.redline.before,after:opened.redline.after});expect(screen.getByRole('button',{name:'Open in workspace'})).toBeTruthy();expect(document.querySelector('[data-readonly-diff]')).not.toBeNull();
  const params={repo:'example/project',rev:'a'.repeat(40),path:'src/app.ts',storyId:'SV-X',range:{start:1,end:2}};await bridge.openDiff(params);await mountDiffTab();expect(host.review.mock.calls.at(-1)?.[0]).toMatchObject({afterRange:params.range,collapseUnchanged:false});break;}
 case 13:{host.readDiff.mockResolvedValue({status:'commitNotLocal',reason:"This commit isn't on this Mac yet",fetchCommand:'git fetch origin '+ 'a'.repeat(40)});const h=mount(data);await openFromRow(h);expect(text('[data-diff-result]')).toContain("This commit isn't on this Mac yet");expect((q('[aria-label="Fetch command"]') as HTMLInputElement).value).toBe('git fetch origin '+ 'a'.repeat(40));click('[data-open-workspace]');expect(h.cs.openWorkspace).toHaveBeenCalledWith({storyId:'SV-X'});expect(host.open).not.toHaveBeenCalled();break;}
 case 14:{host.readDiff.mockResolvedValue(opened);const h=mount(data);await openFromRow(h,'WORKTREE');await mountDiffTab();expect(host.readDiff.mock.calls[0][0]).toMatchObject({repo:'example/project',rev:'WORKTREE',storyId:'SV-X'});expect(host.readDiff).toHaveBeenCalledTimes(2);host.readDiff.mockResolvedValue({...opened,redline:{...opened.redline,after:'live edit'}});await act(async()=>screen.getByText('Refresh').click());expect(host.review.mock.calls.at(-1)?.[0].after).toBe('live edit');break;}
 case 15:{host.readDiff.mockResolvedValue({status:'worktreeRemoved',reason:'The worktree was removed'});const h=mount(data);await openFromRow(h,'WORKTREE');expect(text('[data-diff-result]')).toContain('The worktree was removed');expect(host.readDiff).toHaveBeenCalledTimes(1);expect(host.open).not.toHaveBeenCalled();break;}
 case 16:{for(const [status,reason] of [['refused','Use a repository-relative file path'],['unavailable','Source repository is not configured'],['refused','Use a committed SHA'],['refused','Use an after-side range']]){host.readDiff.mockResolvedValue({status,reason});const h=mount(data);await openFromRow(h);expect(text('[data-diff-result]')).toContain(reason);h.cs.openDiff.mockClear();stops.pop()?.();document.body.replaceChildren();}expect(host.open).not.toHaveBeenCalled();break;}
 case 17:{const h=mount(data);await flush();let finish!:(value:unknown)=>void;h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;})).mockImplementation(async args=>args.cursor?contributionStory({commits:[contributionCommit('c'.repeat(40))],nextCursor:null}):data);h.nudge();await flush();expect(control('[data-more-commits]').getAttribute('aria-disabled')).not.toBe('true');click('[data-more-commits]');await flush();expect(text('[data-commits]')).toContain('ccccccc');finish(data);await flush();expect(text('[data-commits]')).toContain('ccccccc');const latest=contributionStory({commits:[contributionCommit('d'.repeat(40))],nextCursor:'new'});h.cs.storyChanges.mockResolvedValue(latest);h.nudge();await flush();h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));h.nudge();await flush();expect(control('[data-reload-commits]').getAttribute('aria-disabled')).not.toBe('true');click('[data-reload-commits]');await flush();finish(latest);await flush();expect(text('[data-commits]')).toContain('ddddddd');expect(q('[data-reload-commits]')).toBeNull();break;}
 case 18:{const h=mount(data);await flush();let finish!:(value:unknown)=>void;h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;})).mockResolvedValue(contributionStory({commits:[contributionCommit('b'.repeat(40))]}));h.nudge();await flush();h.route('#story/SV-B');await flush();finish(contributionStory({commits:[contributionCommit('c'.repeat(40))]}));await flush();expect(text('[data-commits]')).toContain('bbbbbbb');expect(text('[data-commits]')).not.toContain('ccccccc');h.cs.storyChanges.mockImplementationOnce(()=>new Promise(()=>{}));h.route('#story/SV-X');expect(text('[data-commits]')).toContain('ccccccc');break;}
 case 19:{data.actual=[...data.actual,{path:'src/later.ts',inPlanned:false,area:'src/later.ts',decision:null,lines:null,linesReason:"not in this story's merged PRs"}];mount(data);await flush();expect(text('tr[data-row-focus="file:src/later.ts"]')).toContain("not in this story's merged PRs");expect(text('tr[data-row-focus="file:src/later.ts"]')).not.toContain('+0');expect(text('tr[data-row-focus="file:src/app.ts"]')).toContain('+12 −3');break;}
 case 20:{host.readDiff.mockResolvedValue({status:'refused',reason:'The path leaves the story worktree'});const h=mount(data);await openFromRow(h,'WORKTREE');expect(text('[data-diff-result]')).toContain('The path leaves the story worktree');expect(host.open).not.toHaveBeenCalled();break;}
 case 21: case 22: case 23: case 24: case 25: break; // native-only reader facts
 default: throw new Error(`Unhandled PR3 case ${row.id}`);
 }
});

// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {contributionCommit,contributionStory} from './contribution-fixtures.mjs';
const stops:(()=>void)[]=[];afterEach(()=>{stops.splice(0).forEach(stop=>stop());document.body.replaceChildren();});
const settle=async()=>{for(let i=0;i<30;i++)await Promise.resolve();};
function mount(data=contributionStory()){
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 const cs={kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(data),sessionRead:vi.fn(),openDiff:vi.fn().mockResolvedValue({status:'opened'}),openWorkspace:vi.fn().mockResolvedValue({}),openFile:vi.fn().mockResolvedValue({}),context:vi.fn().mockResolvedValue({org:'example/project',team:'dev'}),queryContext:vi.fn().mockResolvedValue({rows:[{path:'/context/stories/SV-X.md',textMatches:[{title:'Decisions',anchor:'decisions'}]}],total:1}),subscribe:vi.fn().mockResolvedValue({}),unsubscribe:vi.fn(),onVisibility:(f:(shown:boolean)=>void)=>{f(true);return ()=>{};}};
 const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);return {cs,app};
}
it('shows real PR, branch, file counts, numstat and author/session facts with plain diff targets',async()=>{
 mount();await settle();expect(document.querySelector('[data-contribution-meta]')?.textContent).toContain('Merged');expect(document.querySelector('[data-prs] a')?.getAttribute('href')).toBe('https://github.com/example/project/pull/17');expect(document.querySelector('[data-contribution-meta]')?.textContent).toContain('feat/SV-X');
 expect(document.querySelector('[data-key="commits"]')?.textContent).toContain('2');expect(document.querySelector('[data-key="files"]')?.textContent).toContain('1 planned · 1 not planned');
 const files=document.querySelector('[aria-label="Files changed"]')!;expect(files.textContent).toContain('+12 −3');expect(files.textContent).toContain('+8 −0');expect(files.querySelectorAll('[data-decision]')).toHaveLength(1);expect(files.querySelector('[data-decision]')?.textContent).toBe('Decision 2');
 const commits=document.querySelector('[data-commits]')!;expect(commits.textContent).toContain('aaaaaaa');expect(commits.textContent).toContain('Builder');expect(commits.textContent).toContain('exact');expect(commits.querySelector('a[href*="aaaaaaa"]')).toBeNull();expect(files.querySelector('[data-open-diff]')).toBeNull();
});
it('shows absent Touchpoints once and never labels its actual rows no',async()=>{
 mount(contributionStory({planned:null,plannedUntouched:[],unavailable:{planned:'story has no Touchpoints section'}}));await settle();expect(document.body.textContent?.split('story has no Touchpoints section')).toHaveLength(2);expect([...document.querySelectorAll('[data-file-planned]')].map(el=>el.textContent)).toEqual(['','']);
});
it('retains sessions, task rows and totals when the worktree was removed',async()=>{
 mount(contributionStory({worktrees:[{path:'/repo/removed',state:'removed',basis:'exact'}]}));await settle();expect(document.querySelector('[data-worktree-note]')?.textContent).toMatch(/worktree was removed/i);expect(document.querySelector('[data-worktree-note]')?.textContent).toContain('git');expect(document.querySelector('[data-session="lead"]')).not.toBeNull();expect(document.querySelector('[data-task="lead"]')).not.toBeNull();
});
it.each(['squash','open','missing','binary'])('shows %s evidence without inventing unavailable facts',async kind=>{
 const data=contributionStory();if(kind==='squash')data.prs=[{...data.prs[0],source:'squash-subject',commits:{unavailable:'squash-merged'}}];if(kind==='open')data.prs=[];if(kind==='missing'){data.prs=[];data.branches=[{...data.branches[0],availability:'missing',unavailable:'branch not found locally'}];}if(kind==='binary')data.actual=[{...data.actual[0],lines:null,linesReason:'binary'}];mount(data);await settle();expect(document.querySelector('[data-changes]')?.textContent).toContain(({squash:'branch commits not kept (squash-merged)',open:'not merged; open PR lookup needs GitHub',missing:'branch not found locally',binary:'binary'})[kind]);
});
it('pages using the real commit cursor and keeps the existing row after refresh',async()=>{
 const h=mount();await settle();const first=document.querySelector('[data-row-focus="commit:'+ 'a'.repeat(40)+'"]');h.cs.storyChanges.mockImplementation(async args=>args.cursor?contributionStory({commits:[contributionCommit('c'.repeat(40),{basis:'associated',sessionIds:[],author:null})],nextCursor:null}):contributionStory());document.querySelector<HTMLButtonElement>('[data-more-commits]')!.click();await settle();expect(h.cs.storyChanges).toHaveBeenCalledWith({storyId:'SV-X',cursor:'cursor-two'});expect(document.querySelectorAll('[data-commits] [data-row-focus]')).toHaveLength(2);expect(document.querySelector('[data-row-focus="commit:'+ 'a'.repeat(40)+'"]')).toBe(first);h.cs.subscribe.mock.calls[0][1].onChange();await settle();expect(document.querySelectorAll('[data-commits] [data-row-focus]')).toHaveLength(2);
});
it('opens the recorded Decisions section through scoped context lookup',async()=>{
 const h=mount();await settle();document.querySelector<HTMLButtonElement>('[data-decision="2"]')!.click();await settle();expect(h.cs.queryContext).toHaveBeenCalledWith({match:{id:'SV-X'},text:'Decisions',where:{orgs:['example/project']},limit:2});expect(h.cs.openFile).toHaveBeenCalledWith({path:'/context/stories/SV-X.md#decisions',newTab:true});
});
it('uses native disclosure for planned-but-untouched paths after the first three',async()=>{
 mount();await settle();expect(document.querySelector('[data-untouched]')?.textContent).toContain('src/untouched-a.ts');expect(document.querySelector('[data-untouched] summary')?.textContent).toBe('+2 more');expect(document.querySelector('[data-untouched] details')?.hasAttribute('open')).toBe(false);
});
it('keeps loaded commit facts on cursor invalidation until an explicit reload',async()=>{
 const h=mount();await settle();h.cs.storyChanges.mockResolvedValue(contributionStory({commits:[contributionCommit('c'.repeat(40))],nextCursor:null}));document.querySelector<HTMLButtonElement>('[data-more-commits]')!.click();await settle();h.cs.storyChanges.mockResolvedValue(contributionStory({commits:[contributionCommit('d'.repeat(40))],total:3,nextCursor:'new-cursor'}));h.cs.subscribe.mock.calls[0][1].onChange();await settle();expect(document.querySelector('[data-row-focus="commit:'+ 'a'.repeat(40)+'"]')).not.toBeNull();expect(document.querySelector('[data-row-focus="commit:'+ 'c'.repeat(40)+'"]')).not.toBeNull();expect(document.querySelector('[data-changes-error]')?.textContent).toContain('Commit history changed');document.querySelector<HTMLButtonElement>('[data-reload-commits]')!.click();await settle();expect(document.querySelectorAll('[data-commits] [data-row-focus]')).toHaveLength(1);expect(document.querySelector('[data-row-focus="commit:'+ 'd'.repeat(40)+'"]')).not.toBeNull();
});
it('a passive story read never blocks a user loading more commits',async()=>{
 const h=mount();await settle();let finish:(value:unknown)=>void=()=>{};h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;})).mockImplementation(async args=>args.cursor?contributionStory({commits:[contributionCommit('c'.repeat(40))],nextCursor:null}):contributionStory());
 h.cs.subscribe.mock.calls[0][1].onChange();await settle();document.querySelector<HTMLButtonElement>('[data-more-commits]')!.click();await settle();expect(document.querySelectorAll('[data-commits] li')).toHaveLength(2);finish(contributionStory());await settle();expect(document.querySelectorAll('[data-commits] li')).toHaveLength(2);
});
it('a passive story read never blocks a user reloading changed commit history',async()=>{
 const h=mount();await settle();h.cs.storyChanges.mockImplementation(async(args:{cursor?:string})=>args?.cursor?contributionStory({commits:[contributionCommit('c'.repeat(40))],nextCursor:null}):contributionStory());document.querySelector<HTMLElement>('[data-more-commits]')!.click();await settle();const latest=contributionStory({commits:[contributionCommit('d'.repeat(40))],nextCursor:'new-cursor'});h.cs.storyChanges.mockResolvedValue(latest);h.cs.subscribe.mock.calls[0][1].onChange();await settle();
 let finish:(value:unknown)=>void=()=>{};h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));h.cs.subscribe.mock.calls[0][1].onChange();await settle();const reload=document.querySelector<HTMLButtonElement>('[data-reload-commits]')!;reload.click();await settle();expect(reload.getAttribute('aria-disabled')).toBe('true');finish(latest);await settle();expect(document.querySelectorAll('[data-commits] li')).toHaveLength(1);expect(document.querySelector('[data-reload-commits]')).toBeNull();
});
it('opens only recorded file revisions or the story-bound live worktree and keeps diff controls stable',async()=>{
 const h=mount(contributionStory({sourceRepo:'example/project'}));await settle();
 const live=document.querySelector<HTMLButtonElement>('[data-diff-rev="WORKTREE"]')!;expect(live).not.toBeNull();live.click();await settle();expect(h.cs.openDiff).toHaveBeenLastCalledWith({repo:'example/project',rev:'WORKTREE',path:live.dataset.diffPath,storyId:'SV-X'});
 const commit=document.querySelector<HTMLButtonElement>('[data-diff-rev="'+ 'a'.repeat(40)+'"]')!;commit.click();await settle();expect(h.cs.openDiff.mock.calls.at(-1)?.[0].rev).toBe('a'.repeat(40));
 h.cs.subscribe.mock.calls[0][1].onChange();await settle();expect(document.querySelector('[data-diff-rev="WORKTREE"]')).toBe(live);
});
it('shows typed missing-commit advice and opens the known story workspace without fetching',async()=>{
 const h=mount(contributionStory({sourceRepo:'example/project'}));await settle();h.cs.openDiff.mockResolvedValue({status:'commitNotLocal',reason:"This commit isn't on this Mac yet",fetchCommand:'git fetch origin '+ 'a'.repeat(40)});document.querySelector<HTMLButtonElement>('[data-diff-rev="'+ 'a'.repeat(40)+'"]')!.click();await settle();expect(document.querySelector('[data-diff-result]')?.textContent).toContain("This commit isn't on this Mac yet");expect(document.querySelector<HTMLInputElement>('[aria-label="Fetch command"]')?.value).toContain('git fetch origin');document.querySelector<HTMLButtonElement>('[data-open-workspace]')!.click();await settle();expect(h.cs.openWorkspace).toHaveBeenCalledWith({storyId:'SV-X'});
});
it('a changed first-page cursor is adopted silently when the user never paged',async()=>{
 const {cs}=mount(contributionStory({sourceRepo:'example/project'}));await settle();
 cs.storyChanges.mockResolvedValue(contributionStory({sourceRepo:'example/project',nextCursor:'cursor-new',total:3}));window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();
 expect(document.querySelector('[data-reload-commits]')).toBeNull();expect(document.querySelector<HTMLElement>('[data-changes-error]')?.hidden).toBe(true);expect(document.querySelector('[data-more-commits]')).not.toBeNull();
});
it('only the latest diff click shows its result and a new click clears the old one',async()=>{
 const {cs}=mount(contributionStory({sourceRepo:'example/project'}));await settle();
 let first!:(value:unknown)=>void;cs.openDiff.mockImplementationOnce(()=>new Promise(resolve=>{first=resolve;})).mockResolvedValueOnce({status:'refused',reason:'Second answer'});
 document.querySelector<HTMLElement>('[data-diff-rev="WORKTREE"]')!.click();await settle();document.querySelector<HTMLElement>(`[data-diff-rev="${'a'.repeat(40)}"]`)!.click();await settle();
 first({status:'refused',reason:'First answer'});await settle();
 expect(document.querySelector('[data-diff-result]')?.textContent).toContain('Second answer');expect(document.querySelector('[data-diff-result]')?.textContent).not.toContain('First answer');
 cs.openDiff.mockImplementationOnce(()=>new Promise(()=>{}));document.querySelector<HTMLElement>('[data-diff-rev="WORKTREE"]')!.click();await settle();
 expect(document.querySelector('[data-diff-result]')?.textContent).not.toContain('Second answer');
});
it('a failed Open workspace from missing-commit advice shows its reason',async()=>{
 const {cs}=mount(contributionStory({sourceRepo:'example/project'}));await settle();
 cs.openDiff.mockResolvedValueOnce({status:'commitNotLocal',reason:"This commit isn't on this Mac yet",fetchCommand:'git fetch origin '+'a'.repeat(40)});cs.openWorkspace.mockRejectedValueOnce(new Error('Workspace could not open'));
 document.querySelector<HTMLElement>(`[data-diff-rev="${'a'.repeat(40)}"]`)!.click();await settle();document.querySelector<HTMLElement>('[data-open-workspace]')!.click();await settle();
 expect(document.querySelector('[data-diff-result]')?.textContent).toContain('Workspace could not open');expect(document.querySelector<HTMLInputElement>('[aria-label="Fetch command"]')?.value).toBe('git fetch origin '+'a'.repeat(40));
});
it('a row keeps its own line reason when it differs from the section note',async()=>{
 mount(contributionStory({unavailable:{fileLines:'story spans branches with no single final tree'},actual:[{path:'src/app.ts',inPlanned:true,area:'src/app.ts',decision:null,lines:null,linesReason:'story spans branches with no single final tree'},{path:'image.bin',inPlanned:false,area:'image.bin',decision:null,lines:null,linesReason:'binary'}]}));await settle();
 expect(document.querySelector('tr[data-row-focus="file:image.bin"]')?.textContent).toContain('binary');expect(document.querySelector('tr[data-row-focus="file:src/app.ts"]')?.textContent).toContain('unknown');
});

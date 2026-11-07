// SPDX-License-Identifier: MIT
// Wire-shaped story/session pages driven by the harness sessions topic.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
// Line ownership: sourceRepo, PR files, the story's own ranges and a whyLine stub switched by window.whyLineMode.
const LINES = `const sha='a'.repeat(40),changes=codesense.storyChanges;window.whyLineMode='exact';window.whyLineCalls=[];window.openedFiles=[];window.rangeReads=[];window.heldWhyLines=[];
    codesense.storyChanges=async(args={})=>{const data=await changes(args);data.sourceRepo='example/project';data.prs=data.prs.map(pr=>({...pr,files:['src/app.ts','tests/app.test.ts']}));if(args.include){window.rangeReads.push(args);data.commits=[contributionCommit(sha,{files:['src/app.ts','tests/app.test.ts'],ranges:[{path:'src/app.ts',start:2,end:3,commit:sha,basis:'exact'},{path:'src/app.ts',start:5,end:5,commit:sha,basis:'exact'},{path:'tests/app.test.ts',start:10,end:12,commit:sha,basis:'exact'}]})];data.nextCursor=null;}return data;};
    codesense.openFile=async args=>{window.openedFiles.push(args);return {};};
    codesense.whyLine=async args=>{window.whyLineCalls.push(args);const exact=(start,end,turn)=>({start,end,commit:sha,traceId:'trace',tool:'claude',nativeSessionId:'native-lead',model:'claude-sonnet-5-5',basis:'exact',sessionId:'lead',link:'linked',story:'SV-X',turn:{sessionId:'lead',toolUseId:'toolu_'+turn,turn}});
      const ranges={exact:[exact(2,3,4),exact(5,5,6)],associated:[{start:args.start,end:args.end,commit:'c'.repeat(40),basis:'associated',tool:null,nativeSessionId:null,model:null,sessionId:null,link:'unlinked',story:'SV-X',turn:null,candidateSessions:['lead'],reason:'story association only; no note on introducing commit'}],empty:[]}[window.whyLineMode];
      const lead=session('lead');lead.session.agent='Builder';const answer={repo:args.repo,rev:args.rev,resolvedCommit:'b'.repeat(40),path:args.path,notesRef:'sense=1',ranges,sessions:window.whyLineMode==='exact'?{lead}:{}};
      return window.holdWhyLine?new Promise(resolve=>window.heldWhyLines.push(()=>resolve(answer))):answer;};`;
export function writeTrailFixture(
  output = "/tmp/cde19-conformance/story-trail.html",
  { route = "#story/SV-X", panel = false, contributions = false, lines = false } = {},
) {
  contributions ||= lines;
  const base = resolve(
    dirname(fileURLToPath(import.meta.url)),
    "../src-tauri/assets/dashboards/story-trail",
  );
  const fixture = [
    "fixtures.mjs",
    "conversation-fixtures.mjs",
    ...(contributions ? ["contribution-fixtures.mjs"] : []),
  ]
    .map((name) =>
      readFileSync(`${base}/${name}`, "utf8")
        .replace(/^import .*;\n/gm, "")
        .replaceAll("export ", ""),
    )
    .join("\n");
  const inject = `<script>(()=>{${fixture}\nlocation.hash=${JSON.stringify(route)};
    codesense.storyChanges=async(args={})=>{const rows=await codesense.sessions();return ${contributions ? "contributionStory" : "story"}({sessions:rows.map(r=>{const s=session(r.id);s.session.agent=r.displayName||r.id;s.session.handle=r.id;return s;}),${contributions ? "...(args.cursor?{commits:[contributionCommit('c'.repeat(40),{subject:'Second recorded change',basis:'associated',sessionIds:[]})],nextCursor:null}:{})" : ""}});};
    codesense.sessionRead=async args=>{const rows=await codesense.sessions();const s=session(args.sessionId);s.session.agent=rows.map(r=>r.displayName||r.id).join(' · ');const start=args.turn??args.cursor??0,limit=args.limit??20;
      s.conversation=conversation({sessionId:args.sessionId,turnCount:42,toolCallCount:1,turns:Array.from({length:Math.max(0,Math.min(limit,42-start))},(_,i)=>turn(start+i,{role:(start+i)%5===0?'user':'assistant',text:(start+i)===37?'Checked the change. [redacted] is omitted.':'Check the recorded behavior and its result.',...((start+i)===37?{tool_calls:[{name:'Edit',input:'{"file_path":"src/app.ts","start_line":3,"end_line":4}',result:'Updated the recorded lines.',is_error:false}]}:{})})),nextCursor:start+limit<42?start+limit:null,lookup:args.turn==null?null:{kind:'turn',requested:String(args.turn),found:args.turn<42,turnIndex:args.turn}});return s;};
    ${lines ? LINES : ""}
    ${panel ? "const observer=new MutationObserver(()=>{const opener=document.querySelector('[data-open]');if(opener){observer.disconnect();opener.click();}});observer.observe(document.getElementById('trail'),{childList:true,subtree:true});" : ""}
  })();</script>`;
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(
    output,
    readFileSync(`${base}/index.html`, "utf8").replace("<script>(()=>{", inject + "<script>(()=>{"),
  );
  return output;
}
if (process.argv[1] === fileURLToPath(import.meta.url))
  console.log(
    writeTrailFixture(process.argv[2], {
      route: process.argv[3] ?? "#story/SV-X",
      panel: process.argv[4] === "panel",
      contributions: process.argv[5] === "contributions",
      lines: process.argv[6] === "lines",
    }),
  );

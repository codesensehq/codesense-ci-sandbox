// SPDX-License-Identifier: MIT
// Wire projections shared by the session panel and the conversation tab.
// Shown where the installed app lacks a call this view uses.
export const NEEDS_NEWER='This view needs a newer CodeSense.';
export function conversationState(page) {
  if (!page) return 'loading';
  if (page.lookup?.found === false) return page.reason ?? 'That turn is not in the stored conversation';
  return ({absent:'not captured',expired:'removed after 90 days',unsupported:page.reason,unavailable:page.reason})[page.state] ?? page.state;
}
export function editChips(turn) {
  return (turn.tool_calls??[]).flatMap(call=>{
    if(!/edit|write|patch/i.test(call.name))return [];
    let input;try{input=JSON.parse(call.input);}catch{
      return [...String(call.input??'').matchAll(/^\*\*\* (?:Update|Add) File: (.+)$/gm)].map(match=>({path:match[1],start:null,end:null}));
    }
    const path=input?.file_path??input?.path;
    if(typeof path!=='string')return [];
    const start=input.start_line??input.line,end=input.end_line??start;
    const ranged=Number.isInteger(start)&&Number.isInteger(end)&&start>0&&end>=start;
    return [{path,start:ranged?start:null,end:ranged?end:null}];
  });
}
export const hasRedaction = turn => [turn.text,...(turn.tool_calls??[]).flatMap(call=>[call.input,call.result])].some(value=>typeof value==='string'&&value.includes('[redacted]'));
export function runningSession(rows, facts, requested) {
  const ids=new Set([requested,facts?.session?.sessionId,...(facts?.nativeSessionIds??[])].filter(Boolean));
  return rows.find(row=>row.state==='running' && (ids.has(row.sessionId)));
}
export const tailCursor = page => Math.max(0,page.turnCount+(page.truncated?.droppedTurnCount??0)-4);

// Each retained contiguous interval starts its own deterministic twenty-turn pages.
export function conversationPageRequest(index,gap=null){
  const at=gap&&index>=gap[0]&&index<=gap[1]?gap[1]+1:index;
  const start=gap&&at>gap[1]?gap[1]+1:0;
  const cursor=start+Math.floor((at-start)/20)*20;
  return {cursor,limit:gap&&cursor<gap[0]?Math.min(20,gap[0]-cursor):20};
}

// Recorded absolute paths may be made relative only to a recorded story worktree.
export function editDiffRequest(facts,chip,storyId){
  if(!facts?.sourceRepo)return {status:'unavailable',reason:'The story’s source repository is unavailable'};
  let path=chip.path;
  if(path.startsWith('/')){
    const root=(facts.worktrees??[]).map(row=>row.path?.replace(/\/$/,'')).filter(Boolean).sort((a,b)=>b.length-a.length).find(root=>path.startsWith(root+'/'));
    if(!root)return {status:'refused',reason:'The recorded path is outside this story’s worktree'};
    path=path.slice(root.length+1);
  }
  return {params:{repo:facts.sourceRepo,rev:'WORKTREE',path,storyId,...(chip.start==null?{}:{range:{start:chip.start,end:chip.end}})}};
}

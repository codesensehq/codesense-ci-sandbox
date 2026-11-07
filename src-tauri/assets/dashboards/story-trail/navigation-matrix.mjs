// SPDX-License-Identifier: MIT
// Shared mounted/WebKit transition contract; fixture reads match reader.rs filter-then-limit.
export const navigationCases=[
 {name:'ordinary',indices:Array.from({length:70},(_,i)=>i),gap:null,jump:37,first:20,earlier:0,boundary:20},
 {name:'retained gap',indices:[0,...Array.from({length:60},(_,i)=>100+i)],gap:[1,99],jump:105,first:100,earlier:0,boundary:100},
];
export function navigationConversation(spec,args){
 const rest=spec.indices.filter(index=>index>=(args.turn??args.cursor??0)),shown=rest.slice(0,args.limit??20);
 return {sessionId:'lead',state:spec.gap?'truncated':'present',reason:null,turns:shown.map(index=>({index,role:index===0?'user':'assistant',text:('Recorded turn '+index+' — reading context.\n').repeat(80),tool_calls:[]})),turnCount:spec.indices.length,toolCallCount:0,nextCursor:rest.length>shown.length?shown.at(-1)+1:null,truncated:spec.gap?{droppedTurns:spec.gap,droppedTurnCount:spec.gap[1]-spec.gap[0]+1,droppedBytes:100000,trimmedFields:0}:null,unavailable:{},lookup:args.turn==null?null:{kind:'turn',requested:String(args.turn),turnIndex:args.turn,found:spec.indices.includes(args.turn)}};
}
export async function runNavigationMatrix(spec,driver,expect){
 const route=(first,highlight)=>`#session/lead?from=${first}${highlight==null?'':`&turn=${highlight}`}`;
 async function check(first,highlight,reading){
  const snapshot=await driver.snapshot(),end=spec.gap&&first<spec.gap[0]?Math.min(first+20,spec.gap[0]):first+20;
  expect(snapshot.indices).toEqual(spec.indices.filter(index=>index>=first&&index<end));
  expect(new Set(snapshot.indices).size).toBe(snapshot.indices.length);
  expect(snapshot.highlight).toEqual(highlight==null?[]:[highlight]);expect(snapshot.route).toBe(route(first,highlight));
  if(reading)expect(snapshot.reading).toEqual(reading);
 }
 async function refresh(first,highlight,reading){await driver.refresh();await check(first,highlight,reading);}
 await driver.open(`#session/lead/turn/${spec.jump}`);await check(spec.first,spec.jump);await driver.revealed(spec.jump);
 let saved=await driver.scroll(500,90);await refresh(spec.first,spec.jump,saved);
 const before=await driver.click('unhighlight');await check(spec.first,null,before);
 saved=await driver.scroll(500,90);await refresh(spec.first,null,saved);
 await driver.click('earlier');await check(spec.earlier,null,{thread:0,rail:0});await refresh(spec.earlier,null,{thread:0,rail:0});
 await driver.click('later');await check(spec.first,null,{thread:0,rail:0});
 saved=await driver.scroll(500,90);await refresh(spec.first,null,saved);
 await driver.story();await driver.back();await check(spec.first,null,saved);await refresh(spec.first,null,saved);
 await driver.open(route(spec.first,null));await check(spec.first,null,{thread:0,rail:0});await refresh(spec.first,null,{thread:0,rail:0});
 await driver.click('jump',spec.boundary);await check(spec.first,spec.boundary);await driver.revealed(spec.boundary);
 saved=(await driver.snapshot()).reading;await refresh(spec.first,spec.boundary,saved);
 await driver.click('earlier');await check(0,null,{thread:0,rail:0});await refresh(0,null,{thread:0,rail:0});
 await driver.click('jump',0);await check(0,0);await driver.revealed(0);saved=(await driver.snapshot()).reading;await refresh(0,0,saved);
 await driver.click('unhighlight');saved=await driver.scroll(500,0);await refresh(0,null,saved);
 await driver.story();await driver.back();await check(0,null,saved);await refresh(0,null,saved);
 await driver.open(route(0,null));await check(0,null,{thread:0,rail:0});await refresh(0,null,{thread:0,rail:0});
}

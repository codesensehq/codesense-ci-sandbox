// SPDX-License-Identifier: MIT
import {expect,it} from 'vitest';
import {conversationState, editChips, hasRedaction} from './conversation.mjs';
import {conversation,turn} from './conversation-fixtures.mjs';
it.each([['absent','not captured'],['expired','removed after 90 days'],['unsupported','reader reason'],['unavailable','reader reason'],['present','present'],['truncated','truncated']])('maps %s', (state,expected)=>expect(conversationState(conversation({state,reason:'reader reason'}))).toBe(expected));
it('maps loading and lookup misses',()=>{expect(conversationState(null)).toBe('loading');expect(conversationState(conversation({turns:[],lookup:{kind:'turn',requested:'99',found:false,turnIndex:99},reason:'turn 99 is not in the stored conversation'}))).toBe('turn 99 is not in the stored conversation');});
it('extracts recorded file edits with optional line ranges',()=>{
  expect(editChips(turn(2,{tool_calls:[{name:'Edit',input:JSON.stringify({file_path:'src/app.ts',start_line:3,end_line:5}),is_error:false},{name:'Edit',input:JSON.stringify({file_path:'src/other.ts',old_string:'before',new_string:'after'})},{name:'Bash',input:'pnpm test'}]}))).toEqual([{path:'src/app.ts',start:3,end:5},{path:'src/other.ts',start:null,end:null}]);
});
it('detects the exact reader redaction marker in text and tool fields',()=>{expect(hasRedaction(turn(0,{text:'[redacted]'}))).toBe(true);expect(hasRedaction(turn(0,{tool_calls:[{name:'Read',result:'[redacted]'}]}))).toBe(true);expect(hasRedaction(turn(0,{text:'ordinary text'}))).toBe(false);});
it('never treats a reused handle as live session identity',async()=>{
 const {runningSession}=await import('./conversation.mjs');const facts={session:{sessionId:'old-session',handle:'reusable'},nativeSessionIds:['old-native']};
 expect(runningSession([{id:'reusable',handle:'reusable',sessionId:'new-session',state:'running'}],facts,'old-session')).toBeUndefined();
 expect(runningSession([{id:'old-session',handle:'old-session',sessionId:'new-session',state:'running'}],facts,'old-session')).toBeUndefined();
 expect(runningSession([{id:'reusable',handle:'reusable',sessionId:'old-session',state:'running'}],facts,'old-session')?.id).toBe('reusable');
});
it('preserves real Claude edit/write and Codex patch paths without inferred line numbers',()=>{
 const calls=[{name:'Edit',input:JSON.stringify({file_path:'src/edit.ts',old_string:'before',new_string:'after'})},{name:'Write',input:JSON.stringify({file_path:'src/new.ts',content:'recorded text'})},{name:'apply_patch',input:'*** Begin Patch\n*** Update File: src/codex.ts\n@@\n-before\n+after\n*** Add File: src/added.ts\n+new\n*** End Patch'}];
 expect(editChips(turn(4,{tool_calls:calls}))).toEqual([{path:'src/edit.ts',start:null,end:null},{path:'src/new.ts',start:null,end:null},{path:'src/codex.ts',start:null,end:null},{path:'src/added.ts',start:null,end:null}]);
});

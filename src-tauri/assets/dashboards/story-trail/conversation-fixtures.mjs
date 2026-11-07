// SPDX-License-Identifier: MIT
// ConversationPage: session_conversation/reader.rs; Turn/ToolCall: agent_cli/output.rs.
export const turn = (index, extra={}) => ({role:'assistant',text:'Built the change.',index,ts_ms:1793178000000,model:'gpt-model',...extra});
export const conversation = (extra={}) => ({sessionId:'lead',state:'present',reason:null,turns:[turn(0)],nextCursor:null,turnCount:1,toolCallCount:0,truncated:null,sourceFormat:'codex-jsonl',unavailable:{},lookup:null,...extra});

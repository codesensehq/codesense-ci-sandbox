// SPDX-License-Identifier: MIT
import {story} from './fixtures.mjs';
// Private StoryChanges fields from story_changes::{StoryChanges,git,planned}.
export const contributionCommit=(sha='a'.repeat(40),extra={})=>({sha,subject:'Keep recorded contributions',authoredAt:1793178000000,author:'Builder',files:['src/app.ts'],sessionIds:['lead'],basis:'exact',...extra});
export const contributionStory=(extra={})=>story({
 totals:{...story().totals,commits:2,files:2},
 branches:[{name:'feat/SV-X',basis:'exact',prs:[],availability:'local',pr:{number:null,url:null,urlReason:'PR number not available',mergedAt:null,mergedAtReason:'not merged',unavailable:'not merged; open PR lookup needs GitHub'},mergedSha:null}],
 prs:[{number:17,url:'https://github.com/example/project/pull/17',urlReason:null,mergedAt:1793178000000,mergedAtReason:null,mergedSha:'b'.repeat(40),source:'merge-commit',branch:'feat/SV-X',basis:'exact'}],
 worktrees:[{path:'/repo/worktree',state:'live',basis:'exact'}],
 commits:[contributionCommit()],total:2,nextCursor:'cursor-two',scanLimit:2000,
 planned:['src/app.ts','src/untouched-a.ts','src/untouched-b.ts','src/untouched-c.ts','src/untouched-d.ts','src/untouched-e.ts'],plannedBase:null,
 actual:[{path:'src/app.ts',inPlanned:true,area:'src/app.ts',decision:null,lines:{added:12,removed:3},linesReason:null},{path:'tests/app.test.ts',inPlanned:false,area:'tests/app.test.ts',decision:2,lines:{added:8,removed:0},linesReason:null}],
 plannedUntouched:['src/untouched-a.ts','src/untouched-b.ts','src/untouched-c.ts','src/untouched-d.ts','src/untouched-e.ts'],unavailable:{},...extra,
});

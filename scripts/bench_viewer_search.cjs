// Copyright (c) 2026 RioPlay. SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic search algorithm comparison; excludes DOM, painting and layout.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {performance} = require('node:perf_hooks');
const assert = require('node:assert/strict');
const ctx = vm.createContext({performance});
vm.runInContext(fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/viewer-search.js'),'utf8'),ctx);
const report = vm.runInContext(`
  const nodes=Array.from({length:50000},(_,i)=>({id:i,label:['render','render_all','pre_render','other'][i%4],anchor:'aden://module/render/src#'+['render','render_all','pre_render','other'][i%4],deg:(i*7919)%997}));
  const name=n=>n.label;
  const q='render';
  const degree=(a,b)=>b.deg-a.deg;
  let legacyNames=0,candidateNames=0;
  function legacy(limit) {
    return nodes.filter(n=>AdenSearch.text(n,name(n)).includes(q))
      .sort((a,b)=>{legacyNames+=2;return AdenSearch.rank(a,name(a),q)-AdenSearch.rank(b,name(b),q)||(limit===Infinity?0:degree(a,b));}).slice(0,limit);
  }
  function candidate(limit) {
    return AdenSearch.select(nodes,n=>{candidateNames++;return name(n);},q,limit,limit===Infinity?null:degree);
  }
  const cells=[];
  for (const limit of [Infinity,8,6]) {
    legacyNames=candidateNames=0;
    const expected=legacy(limit),actual=candidate(limit);
    const parity=JSON.stringify(expected.map(n=>n.id))===JSON.stringify(actual.map(n=>n.id));
    const counts={legacy_name_lookups_in_sort:legacyNames,candidate_name_lookups:candidateNames};
    const timings={legacy:[],candidate:[]};
    for(let repeat=0;repeat<7;repeat++) for(const arm of repeat%2?['candidate','legacy']:['legacy','candidate']) {
      const start=performance.now(); (arm==='legacy'?legacy:candidate)(limit);
      timings[arm].push(performance.now()-start);
    }
    cells.push({limit:limit===Infinity?'all':limit,parity,result_count:actual.length,counts,timings_ms:timings});
  }
  ({schema_version:1,fixture_nodes:nodes.length,query:q,repetitions:7,cells});
`,ctx);
for(const cell of report.cells) assert.equal(cell.parity,true);
report.node=process.version;report.platform=process.platform;
report.scope='Synthetic ranking only; warm caches, excludes DOM/rendering, first-load cost and end-to-end user latency. Concurrent machine load may affect timings.';
const json=JSON.stringify(report,null,2)+'\n';
if(process.argv[2]) fs.writeFileSync(process.argv[2],json); else process.stdout.write(json);

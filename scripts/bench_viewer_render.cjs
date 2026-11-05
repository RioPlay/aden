// Copyright (c) 2026 RioPlay. SPDX-License-Identifier: AGPL-3.0-or-later
// Compare actual 2D envelope callbacks; no browser/GPU latency claims.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const {execFileSync}=require('node:child_process');
const assert=require('node:assert/strict');
const file='crates/aden-cli/assets/view.html';
const baseline=execFileSync('git',['show',(process.argv[3]||'HEAD')+':'+file],{encoding:'utf8'});
const candidate=fs.readFileSync(path.join(__dirname,'../'+file),'utf8');
function harness(html){
  let render,hulls=0;
  const nodes=Array.from({length:10000},(_,i)=>({id:String(i),group:'g'+(i%50),x:(i*17)%331,y:(i*37)%337,loc:i%100}));
  const hidden=new Set(),byId=Object.fromEntries(nodes.map(n=>[n.id,n]));
  const context=vm.createContext({Graph:{onRenderFramePre(fn){render=fn;}},byId,
    envelopes:true,minimal:false,idVisible:id=>!hidden.has(id),palette:['#abcdef'],groupIdx:{},
    groupPulse:new Map(),REDUCED_MOTION:true,hexA:(c,a)=>c+':'+a,countHull:()=>hulls++});
  vm.runInContext(html.slice(html.indexOf('function hullOf('),html.indexOf('let fitPending =')),context);
  vm.runInContext('const originalHull=hullOf;hullOf=points=>{countHull();return originalHull(points);};',context);
  function draw(scale=1){
    const ops=[],before=hulls;
    const canvas={createRadialGradient(...a){ops.push(['gradient',...a]);return {addColorStop(...a){ops.push(['stop',...a]);}};},
      beginPath(){ops.push(['begin']);},arc(...a){ops.push(['arc',...a]);},fill(){ops.push(['fill',String(this.fillStyle)]);},
      moveTo(...a){ops.push(['move',...a]);},quadraticCurveTo(...a){ops.push(['curve',...a]);},closePath(){ops.push(['close']);},
      stroke(){ops.push(['stroke',this.strokeStyle,this.lineWidth]);},fillText(...a){ops.push(['text',...a,this.font]);}};
    render(canvas,scale);return {commands:ops,hulls:hulls-before};
  }
  return {nodes,byId,hidden,draw,context};
}
const before=harness(baseline),after=harness(candidate),observations=[];
function compare(state,scale=1){const a=before.draw(scale),b=after.draw(scale);assert.deepEqual(b.commands,a.commands,state);observations.push({state,baseline_hull_builds:a.hulls,candidate_hull_builds:b.hulls,draw_commands:b.commands.length,parity:true});}
compare('initial');
for(let i=0;i<20;i++)compare('static_frame_'+i);
for(const h of [before,after])h.nodes[0].x+=1;compare('physics_move');
for(const h of [before,after])h.nodes[1].y+=1;compare('drag_move');
for(const h of [before,after])h.hidden.add('2');compare('visibility_filter');
for(const h of [before,after])h.hidden.delete('2');compare('visibility_restore');
for(const h of [before,after])h.nodes[3].loc+=10;compare('source_mass_change');
for(const h of [before,after])h.byId['4']={...h.nodes[4]};compare('graph_replacement');
compare('zoom',2);
for(const h of [before,after])h.context.groupPulse.set('g0',1);compare('pulse_glow',2);
const report={schema_version:1,baseline_revision:process.argv[3]||'HEAD',nodes:10000,groups:50,
  scope:'Actual template geometry/draw-command parity and hull operation counts. Excludes canvas/GPU work and browser latency.',observations};
const json=JSON.stringify(report,null,2)+'\n';if(process.argv[2])fs.writeFileSync(process.argv[2],json);else process.stdout.write(json);

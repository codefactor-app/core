const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const Model=require('../web/codemap-model.js');

const block=(id,label=id)=>({id,label,parent:null,children:[],status:'project',counts:{files:1,types:0,functions:1},incoming:0,outgoing:0,internal:0});
const snapshot=(revision=1,blocks=[block('a')],links=[])=>({revision,blocks,frontier:blocks.map(b=>b.id),links,coverage:{files:blocks.length,complete:true},warnings:[]});
const evidence={entities:[],witnesses:[],total:0,per_page:50};
const settle=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};

// A minimal DOM keeps lifecycle regressions independent of browser/layout behavior.
function controller(){
 const elements=new Map(),pending=[],downloads=[];let controls;
 function element(id){if(!elements.has(id)){
  const classes=new Set(id==='map-inspector'?['hidden']:[]),events=new Map();
  const node={id,events,dataset:{},style:{},textContent:'',clientWidth:900,clientHeight:550,scrollTop:0,scrollLeft:0,
   classList:{add:value=>classes.add(value),remove:value=>classes.delete(value),contains:value=>classes.has(value),toggle(value,force){const active=force??!classes.has(value);if(active)classes.add(value);else classes.delete(value);return active;}},
   addEventListener:(type,handler)=>events.set(type,handler),querySelector:()=>null,querySelectorAll:()=>[],setAttribute(){},getBoundingClientRect:()=>({left:0,top:0})};
  let html='';Object.defineProperty(node,'innerHTML',{get:()=>html,set(value){html=value;if(id==='map-content'){node.scrollTop=node.scrollLeft=0;elements.delete('map-matrix-spacer');if(value.includes('id="map-matrix-spacer"'))element('map-matrix-spacer');}}});elements.set(id,node);
 }return elements.get(id);}
 const context={window:{},CodefactorMapModel:Model,CodefactorMapControls:{create(options){controls=options;return {sync(){},resize(){},setNamedSets(){}};}},
  document:{getElementById:id=>id==='map-matrix-spacer'&&!elements.has(id)?null:element(id),createElement:()=>({click(){downloads.push(this.href);}})},URLSearchParams,Blob,
  URL:{createObjectURL:body=>{const url='blob:'+downloads.length;downloads.bodies.push(body);return url;},revokeObjectURL(){}},
  setTimeout:(fn,delay)=>delay===1000?0:setTimeout(fn,delay),clearTimeout,Map,Set,Date,Number,JSON,Math};
 downloads.bodies=[];vm.runInNewContext(fs.readFileSync(require.resolve('../web/codemap.js'),'utf8'),context);
 const map=context.window.CodefactorMap.create({get:path=>new Promise((resolve,reject)=>pending.push({path,resolve,reject})),onChange(){},onSource(){}});
 const click=attributes=>element('page-codemap').events.get('click')({type:'click',detail:0,target:{closest:selector=>attributes[selector]||null}});
 return {map,pending,downloads,element,click,change:(...args)=>controls.change(...args),action:(...args)=>controls.action(...args)};
}

test('reverting to a cached grouping invalidates the intervening request',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1,[block('directory','Directory snapshot')]));await initial;
 c.change('group','file');assert.equal(c.pending.length,2);c.change('group','directory');
 c.pending[1].resolve(snapshot(1,[block('file','File snapshot')]));await settle();
 assert.match(c.element('map-content').innerHTML,/Directory snapshot/);assert.doesNotMatch(c.element('map-content').innerHTML,/File snapshot/);
});

test('a stale response retries until the requested revision instead of using the old cache',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1));await initial;
 const refreshing=c.map.refresh(3);c.pending[1].resolve(snapshot(2));await settle();assert.equal(c.pending.length,3);
 c.pending[2].resolve(snapshot(3));await refreshing;await c.map.refresh(2);assert.equal(c.pending.length,3);
});

test('a later request for an older revision cannot lower the revision floor',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1));await initial;
 const newer=c.map.refresh(5),older=c.map.refresh(4);c.pending[1].resolve(snapshot(5));c.pending[2].resolve(snapshot(4));await settle();
 assert.equal(c.pending.length,4);c.pending[3].resolve(snapshot(5));await Promise.all([newer,older]);
});

test('restoring a selected block from a URL reopens evidence on a cached snapshot',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1));await initial;
 c.map.readState(new URLSearchParams('map_selected=a&map_direction=incoming'));const restored=c.map.refresh(1);
 assert.equal(c.pending.length,2);const params=new URLSearchParams(c.pending[1].path.split('?')[1]);assert.equal(params.get('block'),'a');assert.equal(params.get('mode'),'incoming');
 c.pending[1].resolve(evidence);await restored;assert.equal(c.element('map-inspector').classList.contains('hidden'),false);
});

test('a refreshed link inspector stays on the link even when a block was selected first',async()=>{
 const c=controller(),data=snapshot(1,[block('a'),block('b')],[{from:'a',to:'b',count:1}]),initial=c.map.refresh(1);c.pending[0].resolve(data);await initial;
 c.click({'[data-map-block]':{dataset:{mapBlock:'a'}}});c.pending[1].resolve(evidence);await settle();
 c.click({'[data-map-link]':{dataset:{mapLink:'0'}}});c.pending[2].resolve(evidence);await settle();
 const refreshing=c.map.refresh(2);c.pending[3].resolve({...data,revision:2});await settle();
 const params=new URLSearchParams(c.pending[4].path.split('?')[1]);assert.equal(params.get('mode'),'link');assert.equal(params.get('from'),'a');assert.equal(params.get('to'),'b');c.pending[4].resolve(evidence);await refreshing;
});

test('export waits for the current filters and requested revision',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1));await initial;
 c.change('kinds',['call']);c.action('export','json');assert.equal(c.downloads.length,0);assert.equal(c.pending.length,3);
 c.pending[2].resolve(snapshot(2));await settle();assert.equal(c.downloads.length,1);
 const exported=JSON.parse(await c.downloads.bodies[0].text());assert.equal(exported.revision,2);assert.deepEqual(exported.ui_state.kinds,['call']);
 c.pending[1].resolve(snapshot(1));await settle();
});

test('removing the selected external block clears the inactive directional filter',async()=>{
 const c=controller();c.map.readState(new URLSearchParams('map_selected=external&map_direction=outgoing'));const refreshing=c.map.refresh(1);c.pending[0].resolve(snapshot(1));await refreshing;
 assert.equal(c.map.getState().selected,'');assert.equal(c.map.getState().direction,'both');assert.equal(c.pending.length,1);
});

test('selecting a block retains the current virtualized matrix scroll position',async()=>{
 const c=controller(),initial=c.map.refresh(1);c.pending[0].resolve(snapshot(1,Array.from({length:500},(_,i)=>block('file:'+i))));await initial;c.change('view','matrix');
 c.element('map-content').scrollTop=8000;c.element('map-content').scrollLeft=6000;c.click({'[data-map-block]':{dataset:{mapBlock:'file:200'}}});
 assert.equal(c.element('map-content').scrollTop,8000);assert.equal(c.element('map-content').scrollLeft,6000);c.pending[1].resolve(evidence);await settle();
});

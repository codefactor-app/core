const {test}=require('node:test');
const assert=require('node:assert/strict');
const {View,LazyGraph,chainRows}=require('../web/graph-model.js');
const node=(id,extra={})=>({id,label:id,kind:'function',file:id+'.ts',module:'app',...extra});
const edge=(from,to,key,extra={})=>({from,to,load_key:key,confidence:'resolved',callsite:{file:from+'.ts',start_line:key+1,start_column:1},guard:[],in_loop:[],...extra});
function server(nodes,edges,revision=4) {
 const byId=new Map(nodes.map(n=>[n.id,n]));
 return request=>{
  const roots=request.roots||[request.id],ids=new Set(roots),selected=new Map();
  for(const dir of request.dir==='both'?['callers','callees']:[request.dir]) {
   const seen=new Set(roots),queue=roots.map(id=>[id,0]);
   for(let at=0;at<queue.length;at++) {
    const [id,depth]=queue[at];if(request.depth!=null&&depth>=request.depth)continue;
    for(const e of edges.filter(e=>(dir==='callers'?e.to:e.from)===id)) {
     const next=dir==='callers'?e.from:e.to;selected.set(e.load_key,e);ids.add(next);
     if(!seen.has(next)){seen.add(next);queue.push([next,depth+1]);}
    }
   }
  }
  const frontier=Object.fromEntries([...ids].map(id=>[id,{callers:edges.filter(e=>e.to===id).map(e=>({key:e.load_key,target:e.from})),callees:edges.filter(e=>e.from===id).map(e=>({key:e.load_key,target:e.to}))}]));
  const included=[...ids].map(id=>byId.get(id)).filter(Boolean);
  return {root:roots[0],roots,nodes:included,edges:[...selected.values()],coverage_nodes:included,coverage_edges:[...selected.values()],coverage_cycles:[],frontier,coverage:{vendor_boundary:false},revision};
 };
}
function create(nodes,edges,options={direction:'callees'}) {
 const query=server(nodes,edges),first=query({id:'root',dir:options.direction,depth:1});
 const session=new LazyGraph(first,options),view=new View(session.data,options.direction);
 return {session,view,query};
}

test('one-step loads only the selected visible frontier and preserves focus and prior branches',async()=>{
 const nodes=['root','child','grand','leaf','side','sideLeaf'].map(id=>node(id)),edges=[edge('root','child',0),edge('child','grand',1),edge('grand','leaf',2),edge('root','side',3),edge('side','sideLeaf',4)];
 const {session,view,query}=create(nodes,edges),requests=[];
 assert.deepEqual(session.data.nodes.map(n=>n.id).sort(),['child','root','side']);
 assert.equal(session.data.partial,true);
 await session.expand(view,'child',false,request=>{requests.push(request);return query(request);});
 assert.deepEqual(requests,[{id:'child',dir:'callees',depth:1}]);
 assert(view.revealed.has('grand'));assert(!view.byId.has('sideLeaf'));assert.equal(view.root,'root');
 await session.expand(view,'child',false,request=>{requests.push(request);return query(request);});
 assert.deepEqual(requests[1],{id:'grand',dir:'callees',depth:1});
 assert(view.revealed.has('leaf'));assert(view.expanded('child'));assert(!view.byId.has('sideLeaf'));
 assert.equal(session.data.coverage_scope,'loaded');
 assert(session.data.coverage.functions.includes('leaf'));assert(!session.data.coverage.functions.includes('sideLeaf'));
});

test('whole branch cannot cross focus cycles to fetch unrelated sibling continuation',async()=>{
 const nodes=['root','child','leaf','side','sideLeaf'].map(id=>node(id)),edges=[edge('root','child',0),edge('child','root',1),edge('child','leaf',2),edge('root','side',3),edge('side','sideLeaf',4)];
 const {session,view,query}=create(nodes,edges);
 await session.expand(view,'child',true,query);
 assert(view.byId.has('leaf'));assert(!view.byId.has('sideLeaf'));
 assert(!session.data.coverage.files.includes('sideLeaf.ts'));
 assert.equal(session.data.root,'root');assert.equal(session.data.partial,true);
});

test('caller expansion uses incoming direction independently of callee branches',async()=>{
 const nodes=['root','caller','older','child','leaf'].map(id=>node(id)),edges=[edge('caller','root',0),edge('older','caller',1),edge('root','child',2),edge('child','leaf',3)];
 const {session,view,query}=create(nodes,edges,{direction:'both'}),requests=[];
 await session.expand(view,'caller',true,request=>{requests.push(request);return query(request);});
 assert.deepEqual(requests,[{id:'caller',dir:'callers',depth:null}]);
 assert(view.revealed.has('older'));assert(!view.byId.has('leaf'));
});

test('depth budget is recomputed after a later shortcut, never allowing depth-six nodes at limit five',async()=>{
 const nodes=['root','long1','long2','pivot','leaf1','leaf2','leaf3','overflow','shortcut'].map(id=>node(id));
 const edges=[edge('root','long1',0),edge('long1','long2',1),edge('long2','pivot',2),edge('pivot','leaf1',3),edge('leaf1','leaf2',4),edge('leaf2','leaf3',5),edge('leaf3','overflow',6),edge('root','shortcut',7),edge('shortcut','pivot',8)];
 const {session,view,query}=create(nodes,edges,{direction:'callees',depth:5}),requests=[];
 await session.expand(view,'long1',true,request=>{requests.push(request);return query(request);});
 assert.equal(requests[0].depth,4);assert(view.byId.has('leaf2'));assert(!view.byId.has('leaf3'));
 await session.expand(view,'shortcut',false,query);
 assert.equal(session.remaining('leaf2',1),1);assert(session.pending('leaf2',1).length);
 await session.expand(view,'leaf2',true,request=>{requests.push(request);return query(request);});
 assert.equal(requests[1].depth,1);assert(view.byId.has('leaf3'));assert(!view.byId.has('overflow'));
 assert.equal(session.data.loaded_depth,5);assert.equal(session.data.requested_depth,5);assert.equal(session.data.partial,false);
});

test('stale generations and changed revisions discard all incoming evidence atomically',async()=>{
 const nodes=['root','child','leaf'].map(id=>node(id)),edges=[edge('root','child',0),edge('child','leaf',1)];
 const {session,view,query}=create(nodes,edges);let finish;
 const pending=session.expand(view,'child',true,()=>new Promise(resolve=>{finish=resolve;}),()=>false);
 finish(query({id:'child',dir:'callees',depth:null}));assert.equal((await pending).stale,true);
 assert(!view.byId.has('leaf'));
 const result=await session.expand(view,'child',true,request=>({...query(request),revision:5}));
 assert.equal(result.revisionChanged,true);assert(!view.byId.has('leaf'));assert.equal(session.revision,4);
});

test('parallel independent merges retain expansions and exact call evidence',async()=>{
 const nodes=['root','a','aLeaf','b','bLeaf'].map(id=>node(id)),edges=[edge('root','a',0),edge('a','aLeaf',1),edge('root','b',2),edge('b','bLeaf',3)];
 const {session,view,query}=create(nodes,edges);const completions={};
 const fetch=request=>new Promise(resolve=>{completions[request.id]=()=>resolve(query(request));});
 const a=session.expand(view,'a',true,fetch),b=session.expand(view,'b',true,fetch);
 completions.a();await a;completions.b();await b;
 assert(view.revealed.has('aLeaf'));assert(view.revealed.has('bLeaf'));assert.equal(session.data.edges.length,4);
 assert.deepEqual(session.data.coverage.leaves,['aLeaf','bLeaf']);
 assert.deepEqual(session.data.coverage.entry_points,['root']);
});

test('class members consume zero real-call depth, and caller tree starts at actual methods',()=>{
 const nodes=[node('m',{parent_type:'Box'}),node('caller'),node('leaf')],edges=[edge('caller','m',0),edge('m','leaf',1,{guard:[{normalized:'ready'}]})];
 const query=server(nodes,edges),data=query({id:'m',dir:'both',depth:1});
 data.root='type:Box';data.nodes.push({id:'type:Box',kind:'class',label:'Box'});data.edges.push({from:'type:Box',to:'m',relation:'member'},{from:'m',to:'type:Box',relation:'member'});
 const session=new LazyGraph(data,{direction:'both',depth:1}),view=new View(session.data,'both');
 assert.equal(session.remaining('leaf',1),0);assert.equal(session.data.partial,false);
 assert.deepEqual(session.data.coverage.functions,['caller','leaf','m']);assert.equal(session.data.coverage.classes.length,1);
 const callers=chainRows(session.data,'callers');assert.deepEqual(callers.rows.map(row=>[row.id,row.depth]),[['m',0],['caller',1]]);
 const callees=chainRows(session.data,'callees');assert.deepEqual(callees.rows[1].guards.map(g=>g.normalized),['ready']);
 assert(view.byId.has('caller'));
});

test('vendor projection retains raw coverage and guarded hidden edges after a merge',async()=>{
 const nodes=[node('root'),node('dep',{vendor:true,package:'node_modules/demo',parent_type:'Library'}),node('callback')],edges=[edge('root','dep',0,{guard:[{normalized:'enabled'}]}),edge('dep','callback',1)];
 const {session,view,query}=create(nodes,edges,{direction:'callees',vendor:'expanded'});
 await session.expand(view,'dep',true,query);
 assert.deepEqual(session.data.coverage.vendor_functions,['dep']);assert.deepEqual(session.data.coverage.vendor_classes,['Library']);
 session.vendor='hidden';const hidden=session.project();
 assert(!hidden.nodes.some(n=>n.id==='dep'));assert.equal(hidden.hidden_vendor_edges.length,2);
 assert.deepEqual(hidden.hidden_vendor_edges[0].guard,[{normalized:'enabled'}]);assert.equal(hidden.coverage.vendor_boundary,true);
 session.vendor='collapsed';const collapsed=session.project();assert(collapsed.nodes.some(n=>n.id==='vendor:node_modules/demo'));
 assert(collapsed.edges.some(e=>e.from==='vendor:node_modules/demo'&&e.to==='callback'));
 const classData=query({id:'dep',dir:'both',depth:1});classData.root='type:Library';classData.nodes.push({id:'type:Library',kind:'class',label:'Library'});classData.edges.push({from:'dep',to:'type:Library',relation:'member'},{from:'type:Library',to:'dep',relation:'member'});
 const classProjection=new LazyGraph(classData,{direction:'both',depth:1,vendor:'collapsed'}).data,ids=new Set(classProjection.nodes.map(n=>n.id));
 assert(classProjection.edges.every(e=>ids.has(e.from)&&ids.has(e.to)));
 assert(classProjection.edges.some(e=>e.relation==='member'&&e.from==='vendor:node_modules/demo'));
 assert.deepEqual(classProjection.coverage.vendor_functions,['dep']);
});

test('server requested coverage survives shallow loading, revision checks and partial exports',async()=>{
 const nodes=['root','child','leaf'].map(id=>node(id)),edges=[edge('root','child',0),edge('child','leaf',1)],query=server(nodes,edges);
 const requested={functions:['root','child','leaf'],files:['root.ts','child.ts','leaf.ts'],vendor_boundary:true};
 const first={...query({id:'root',dir:'callees',depth:1}),requested_coverage:requested,coverage_complete:false,coverage_unparsed_vendor_files:2,coverage_revision:4,coverage_depth:null,coverage_direction:'callees',coverage_filters:{external:false}};
 const session=new LazyGraph(first,{direction:'callees'}),view=new View(session.data,'callees');
 assert.equal(session.data.partial,true);assert.equal(session.data.coverage_scope,'requested');assert.deepEqual(session.data.coverage,requested);
 assert.deepEqual(session.data.loaded_coverage.functions,['child','root']);assert.equal(session.data.nodes.length,2);
 const exported=JSON.parse(JSON.stringify(session.data));assert.equal(exported.coverage_unparsed_vendor_files,2);assert.equal(exported.coverage_complete,false);assert.deepEqual(exported.requested_coverage,requested);assert.equal(exported.coverage_revision,exported.revision);
 await session.expand(view,'child',true,request=>({...query(request),requested_coverage:{functions:['other']}}));
 assert.equal(session.data.partial,false);assert.deepEqual(session.data.requested_coverage,requested);assert.deepEqual(session.data.loaded_coverage.functions,['child','leaf','root']);
});

test('partial export metadata includes full requested counts and PNG UTF-8 metadata',async()=>{
 const Graph=require('../web/graph-model.js');
 const data={nodes:[node('root')],edges:[],partial:true,coverage_scope:'requested',coverage_revision:4,revision:4,coverage_complete:false,coverage_unparsed_vendor_files:2,coverage_direction:'callers',coverage_depth:5,coverage_filters:{external:false},coverage:{functions:['корень','leaf']},requested_coverage:{functions:['корень','leaf']},loaded_coverage:{functions:['корень']}};
 const metadata=Graph.exportMetadata(data);assert.equal(metadata.loaded_nodes,1);assert.equal(metadata.partial,true);assert.equal(metadata.coverage_revision,metadata.revision);assert.equal(metadata.requested_coverage.functions.length,2);assert.equal(metadata.loaded_coverage.functions.length,1);
 const png=new Blob([Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC','base64')],{type:'image/png'});
 const annotated=Buffer.from(await (await Graph.pngWithMetadata(png,metadata)).arrayBuffer());
 let offset=8,found;while(offset<annotated.length){const size=annotated.readUInt32BE(offset),type=annotated.subarray(offset+4,offset+8).toString();if(type==='iTXt'){const bytes=annotated.subarray(offset+8,offset+8+size);assert.equal(bytes.subarray(0,10).toString(),'Codefactor');found=JSON.parse(bytes.subarray(15).toString('utf8'));}offset+=size+12;}
 assert.deepEqual(found,metadata);assert.equal(annotated.subarray(-8,-4).toString(),'IEND');
});

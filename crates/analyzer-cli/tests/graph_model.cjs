const {test}=require('node:test');
const assert=require('node:assert/strict');
const {View,frameBounds,externalCallsites,nodeCard}=require('../web/graph-model.js');
const node=(id,extra={})=>({id,label:id,kind:'function',file:'src/box.js',...extra});
const edge=(from,to,extra={})=>({from,to,confidence:'resolved',...extra});

test('method cards separate complete class namespaces and function names in live and imported nodes',()=>{
 for(const [parent_type,label,name] of [
  ['Lib\\Security\\AdminUserPermissionBuilder','Lib\\Security\\AdminUserPermissionBuilder::_build','_build'],
  ['app.services.Box','app.services.Box.run','run'],
  ['crate::module::Box','crate::module::Box::run','run'],
 ]) {
  for(const extra of [{name},{}]) {
   const card=nodeCard(node(label,{label,parent_type,...extra}));
   assert.equal(card.classLines.join(''),parent_type);
   assert.equal(card.functionLines.join(''),name);
  }
 }
 const full='Company\\'+('LongNamespaceSegment\\'.repeat(12))+'PermissionBuilder';
 const card=nodeCard(node('method',{parent_type:full,name:'build'}));
 assert.equal(card.classLines.join(''),full);assert(!card.classLines.join('').includes('…'));
 assert(card.height>170);
 assert.equal(nodeCard(node('free',{name:'free'})).className,'');
});

test('wrapped namespaces fit inside class frames and do not overlap following cards',()=>{
 const parent_type='Company\\'+('LongNamespaceSegment\\'.repeat(12))+'PermissionBuilder';
 const nodes=[node('root',{file:'root.php'}),node('first',{parent_type,name:'first'}),node('second',{parent_type,name:'second'})];
 const layout=new View({root:'root',nodes,edges:[edge('root','first'),edge('root','second')]}).layout({collapseSimple:false});
 const first=layout.positions.get('first'),second=layout.positions.get('second');
 assert(second.y>=first.y+layout.heights.get('first')+42);
 const frame=layout.frames[0],bounds=frameBounds(frame,layout.positions);
 assert.equal(frame.headerLines.join(''),parent_type);
 assert(bounds.y+bounds.height>=second.y+layout.heights.get('second')+22);
 assert(bounds.y+24+(frame.headerLines.length-1)*14<first.y);
});

test('external call details retain each resource and callsite after projection and bundling',()=>{
 const first=edge('root','http',{argument:'https://one.example/',callsite:{file:'api.py',start_line:2}});
 const second=edge('root','http',{argument:'https://two.example/',callsite:{file:'api.py',start_line:3}});
 const view=new View({root:'root',nodes:[node('root'),node('http',{kind:'external',category:'http',argument:null})],edges:[first,second,edge('root','root')],cycles:[]},'callees');
 const calls=externalCallsites(view.data,'http');
 assert.deepEqual(calls,[first,second]);
 assert.deepEqual(view.layout().bundles.find(bundle=>bundle.to==='http').calls,calls);
 assert.equal(calls[1].argument,'https://two.example/');
 assert.equal(calls[1].callsite.start_line,3);
 assert.equal(externalCallsites({...view.data,edges:[],cycles:[second]},'http')[0],second);
});

test('only immediate callers/callees are visible; one step and whole branch stay local',()=>{
 const view=new View({root:'root',nodes:['root','caller','older','child','grandchild','leaf','sibling','siblingLeaf'].map(id=>node(id)),edges:[edge('older','caller'),edge('caller','root'),edge('root','child'),edge('child','grandchild'),edge('grandchild','leaf'),edge('root','sibling'),edge('sibling','siblingLeaf')]});
 assert.deepEqual([...view.revealed].sort(),['caller','child','root','sibling']);
 assert.deepEqual(view.hidden('child'),{nodes:2,branches:1});
 view.expand('child');assert(view.revealed.has('grandchild'));assert(!view.revealed.has('leaf'));assert(!view.revealed.has('siblingLeaf'));
 view.expand('child',true);assert(view.revealed.has('leaf'));assert(!view.revealed.has('older'));assert(!view.revealed.has('siblingLeaf'));
 view.expand('caller');assert(view.revealed.has('older'));view.collapse();assert(!view.revealed.has('leaf'));
});
test('cycles/shared nodes terminate, expand in both relevant directions, do not duplicate nodes',()=>{
 const view=new View({root:'a',nodes:['a','b','c','d'].map(id=>node(id)),edges:[edge('a','b'),edge('b','a'),edge('b','c'),edge('c','d'),edge('d','c')]});
 assert.deepEqual(view.hidden('b'),{nodes:2,branches:1});view.expand('b',true);assert.equal(view.revealed.size,4);assert.equal(view.layout({collapseSimple:false}).nodes.length,4);assert.equal(view.hidden('b').nodes,0);
});
test('accessors are grouped by structural metadata and class; simple leaves have separate groups',()=>{
 const nodes=[node('root'),node('read',{parent_type:'Box',behavior:'getter'}),node('write',{parent_type:'Box',behavior:'setter'}),node('getComputed',{parent_type:'Box',behavior:'simple_leaf'}),node('getComplex',{parent_type:'Box',behavior:'regular'})];
 const view=new View({root:'root',nodes,edges:nodes.slice(1).map(n=>edge('root',n.id))});let layout=view.layout();
 assert.equal(layout.nodes.filter(n=>n.kind==='fold').length,2);assert(layout.nodes.some(n=>n.id==='getComplex'));assert(!layout.nodes.some(n=>n.id==='read'));assert.equal(layout.frames.length,1);
 const group=layout.nodes.find(n=>n.category==='accessors');assert.equal(group.members.length,2);view.groupExpanded.add(group.id);layout=view.layout();assert(layout.nodes.some(n=>n.id==='read'));assert(layout.nodes.some(n=>n.id==='write'));
 const moved=new Map([['read',{x:1300,y:500}]]);layout=view.layout({positions:moved});assert.equal(layout.positions.get('read').x,1300);assert(frameBounds(layout.frames[0],layout.positions).width>320);
});
test('large groups fold before the rendering limit; repeated callsites remain in bundles',()=>{
 const leaves=Array.from({length:2000},(_,i)=>node('leaf'+i,{parent_type:'Box',behavior:'getter'}));
 const view=new View({root:'root',nodes:[node('root'),...leaves],edges:[...leaves.map(n=>edge('root',n.id)),edge('root','leaf0',{guard:[{raw:'ready'}]})]});const layout=view.layout({limit:10});
 assert.equal(layout.nodes.length,2);assert.equal(layout.nodes.find(n=>n.kind==='fold').members.length,2000);assert.equal(layout.remaining,0);assert.equal(layout.bundles[0].calls.length,2001);
});

test('a cyclic branch cannot expand another branch through the focus node',()=>{
 const view=new View({root:'root',nodes:['root','child','leaf','sibling','later'].map(id=>node(id)),edges:[edge('root','child'),edge('child','root'),edge('child','leaf'),edge('root','sibling'),edge('sibling','later')]});
 assert.deepEqual(view.hidden('child'),{nodes:1,branches:1});view.expand('child',true);assert(view.revealed.has('leaf'));assert(!view.revealed.has('later'));
});

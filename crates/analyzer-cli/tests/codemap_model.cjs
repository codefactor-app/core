const {test}=require('node:test');
const assert=require('node:assert/strict');
const Map=require('../web/codemap-model.js');
const block=(id,parent=null)=>({id,label:id,parent,children:[],status:'project',counts:{files:1,types:0,functions:1},incoming:0,outgoing:0,internal:0});
test('map URL preserves paths with commas, Unicode, empty filters and overlapping sets',()=>{
 const ui={...Map.defaults(),group:'set',view:'matrix',expanded:['dir:src/a,b','module:php:Имя\\Модуль'],selected:'set:one',search:'тест',external:true,kinds:[],direction:'incoming'},params=new URLSearchParams();
 Map.writeState(params,ui);assert.deepEqual(Map.readState(params),ui);assert.equal(Map.query(ui).get('kinds'),'none');
});
test('expanded parent frames retain visible cross-boundary links and long names',()=>{
 const blocks=[block('a'),block('a/x','a'),block('a/y','a'),block('b')];blocks[0].children=['a/x','a/y'];blocks[1].label='ОченьДлинноеИмяПространства'.repeat(4);
 const data={blocks,frontier:['a/x','a/y','b'],links:[{from:'a/x',to:'b',count:2},{from:'a/y',to:'a/x',count:1}]};
 const result=Map.layout(data,{...Map.defaults(),expanded:['a']});assert.deepEqual(result.frames.map(f=>f.id),['a']);assert.equal(result.cards.length,3);assert.equal(result.links.length,2);assert.equal(result.cards.find(c=>c.id==='a/x').lines.join(''),blocks[1].label);
 for(const card of result.cards.filter(c=>c.block.parent==='a'))assert(card.y>result.frames[0].y&&card.y+card.height<=result.frames[0].y+result.frames[0].height);
});
test('matrix and map use the same boundary direction while retaining internal witnesses in both mode',()=>{
 const data={blocks:[block('a'),block('b')],frontier:['a','b'],links:[{from:'a',to:'b',count:4},{from:'b',to:'a',count:2},{from:'a',to:'a',count:1}]};
 const ui={...Map.defaults(),selected:'a',direction:'outgoing'},s=Map.selection(data,ui),w=Map.matrixWindow(data,ui);
 assert.deepEqual(s.links.map(l=>l.count),[4]);assert.equal(w.links.get(JSON.stringify(['a','b'])).count,4);assert(!w.links.has(JSON.stringify(['a','a'])));assert(!w.links.has(JSON.stringify(['b','a'])));
 assert.equal(Map.matrixWindow(data,{...ui,direction:'both'}).links.get(JSON.stringify(['a','a'])).count,1);
});
test('5000 file map and matrix bound rendered work without losing aggregate counts',()=>{
 const blocks=Array.from({length:5000},(_,i)=>block('file:'+i)),data={blocks,frontier:blocks.map(b=>b.id),links:[]},ui=Map.defaults();
 const layout=Map.layout(data,ui);assert.equal(layout.cards.length,200);assert.equal(layout.omitted,4800);
 const matrix=Map.matrixWindow(data,ui,80000,60000);assert(matrix.rowEnd-matrix.rowStart<30);assert(matrix.colEnd-matrix.colStart<20);assert(matrix.rowStart>2000);assert.equal(matrix.ids.length,5000);
});

test('legacy namespace URLs and additional grouping preserve the primary structure',()=>{
 const old=Map.readState(new URLSearchParams('map_group=module'));assert.equal(old.structure,'module');
 const ui={...old,group:'set'},params=new URLSearchParams();Map.writeState(params,ui);const restored=Map.readState(params);
 assert.equal(restored.group,'set');assert.equal(restored.structure,'module');
 assert.equal(Map.readState(new URLSearchParams('map_group=set')).structure,'directory');
});

test('directional focus includes only matching neighbors and excludes opposite edges',()=>{
 const data={blocks:[block('a'),block('b'),block('c')],frontier:['a','b','c'],links:[{from:'a',to:'b',count:1},{from:'c',to:'a',count:1}]};
 const l=Map.layout(data,Map.defaults());
 assert.deepEqual([...Map.neighborhood(l,'a','outgoing').neighbors].sort(),['a','b']);
 assert.deepEqual([...Map.neighborhood(l,'a','incoming').neighbors].sort(),['a','c']);
 assert.equal(Map.neighborhood(l,'a','outgoing').edges.size,1);
});
test('dragging a frame moves every descendant, leaf offsets update enclosing bounds',()=>{
 const nodes=[block('a'),block('a/x','a'),block('a/y','a'),block('b')];nodes[0].children=['a/x','a/y'];const data={blocks:nodes,frontier:['a/x','a/y','b'],links:[]};
 const ui={...Map.defaults(),expanded:['a']},l=Map.layout(data,ui),before=l.cards.map(c=>({id:c.id,x:c.x,y:c.y}));
 const offsets=new global.Map([['a',[60,30]]]);Map.applyOffsets(l,offsets);
 for(const c of l.cards){const original=before.find(b=>b.id===c.id);assert.equal(c.x,original.x+(c.block.parent==='a'?60:0));}
 offsets.set('a/x',[-100,0]);Map.applyOffsets(l,offsets);const frame=l.frames[0],child=l.cards.find(c=>c.id==='a/x');assert(frame.x<child.x);assert(frame.x+frame.width>child.x+child.width);
 const rerendered=Map.layout(data,ui,200,offsets);assert.equal(rerendered.cards.find(c=>c.id==='a/x').x,child.x);
});

test('large expanded groups form a compact grid with no overlapping or escaped cards',()=>{
 const parent=block('lib'),leaves=Array.from({length:200},(_,i)=>block('lib/'+i,'lib'));parent.children=leaves.map(b=>b.id);leaves[4].label='ДлинноеИмя'.repeat(15);
 const data={blocks:[parent,...leaves],frontier:parent.children,links:[]},grid=Map.layout(data,Map.defaults()),frame=grid.frames[0];
 assert(new Set(grid.cards.map(c=>c.x)).size>=4);assert(frame.height<frame.width*2);
 for(const c of grid.cards){assert(c.x>frame.x&&c.y>=frame.y+frame.header);assert(c.x+c.width<frame.x+frame.width&&c.y+c.height<frame.y+frame.height);}
 for(let i=0;i<grid.cards.length;i++)for(let j=i+1;j<grid.cards.length;j++){const a=grid.cards[i],b=grid.cards[j];assert(a.x+a.width<=b.x||b.x+b.width<=a.x||a.y+a.height<=b.y||b.y+b.height<=a.y);}
 const list=Map.layout(data,{...Map.defaults(),arrangement:'list'});assert.equal(new Set(list.cards.map(c=>c.x)).size,1);assert(list.height>grid.height*4);
});
test('nested frames pack different sizes without overlapping sibling groups',()=>{
 const root=block('root'),small=block('small','root'),large=block('large','root'),leaves=Array.from({length:40},(_,i)=>block('large/'+i,'large'));root.children=['small','large'];large.children=leaves.map(b=>b.id);
 const data={blocks:[root,small,large,...leaves],frontier:['small',...large.children],links:[]},l=Map.layout(data,Map.defaults()),outer=l.frames.find(f=>f.id==='root'),inner=l.frames.find(f=>f.id==='large'),single=l.cards.find(c=>c.id==='small');
 assert(single.x+single.width<=inner.x||inner.x+inner.width<=single.x||single.y+single.height<=inner.y||inner.y+inner.height<=single.y);
 assert(inner.x>outer.x&&inner.y>=outer.y+outer.header&&inner.x+inner.width<outer.x+outer.width&&inner.y+inner.height<outer.y+outer.height);
});
test('layout choice is restored from URLs and invalid choices use the compact grid',()=>{
 const ui={...Map.defaults(),arrangement:'list'},params=new URLSearchParams();Map.writeState(params,ui);assert.deepEqual(Map.readState(params),ui);
 assert.equal(Map.readState(new URLSearchParams('map_arrangement=radial')).arrangement,'grid');
});
test('direction filters expanded groups using their descendants and preserves only relevant links',()=>{
 const group=block('lib'),nodes=[group,block('lib/a','lib'),block('lib/b','lib'),block('in'),block('out'),block('unrelated')];group.children=['lib/a','lib/b'];
 const data={blocks:nodes,frontier:['lib/a','lib/b','in','out','unrelated'],links:[{from:'in',to:'lib/a'},{from:'lib/b',to:'out'},{from:'out',to:'unrelated'},{from:'lib/a',to:'lib/b'}]};
 const incoming=Map.layout(data,{...Map.defaults(),selected:'lib',direction:'incoming'}),outgoing=Map.layout(data,{...Map.defaults(),selected:'lib',direction:'outgoing'});
 assert.deepEqual(incoming.links.map(l=>[l.from,l.to]),[['in','lib/a']]);assert.deepEqual(outgoing.links.map(l=>[l.from,l.to]),[['lib/b','out']]);
 assert(!Map.neighborhood(incoming,'lib','incoming').neighbors.has('out'));assert(!Map.neighborhood(outgoing,'lib','outgoing').neighbors.has('in'));
 const unfiltered=Map.layout(data,Map.defaults());assert.equal(Map.neighborhood(unfiltered,'lib','incoming').edges.size,1);assert.equal(Map.neighborhood(unfiltered,'lib','outgoing').edges.size,1);
});

test('rewriting an existing URL removes cleared selection, search, expansion and external filters',()=>{
 const params=new URLSearchParams('tab=codemap&map_selected=old&map_search=old&map_expanded=%5B%22old%22%5D&map_external=1');
 Map.writeState(params,Map.defaults());assert.deepEqual(Map.readState(params),Map.defaults());assert.equal(params.get('tab'),'codemap');
});

test('switching a far matrix selection to the bounded map retains the selection and its neighbors',()=>{
 const blocks=Array.from({length:500},(_,i)=>block('file:'+i)),data={blocks,frontier:blocks.map(b=>b.id),links:[{from:'file:499',to:'file:498',count:2}]};
 const layout=Map.layout(data,{...Map.defaults(),selected:'file:499'});
 assert.equal(layout.cards.length,200);assert(layout.positions.has('file:499'));assert(layout.positions.has('file:498'));assert.equal(layout.links.length,1);assert.equal(layout.omitted,300);
});

test('bounded map prioritizes the selected block ahead of a large incoming neighborhood',()=>{
 const blocks=Array.from({length:500},(_,i)=>block('file:'+i)),data={blocks,frontier:blocks.map(b=>b.id),links:blocks.slice(0,499).map(b=>({from:b.id,to:'file:499',count:1}))};
 const layout=Map.layout(data,{...Map.defaults(),selected:'file:499',direction:'incoming'});assert(layout.positions.has('file:499'));assert.equal(layout.cards.length,200);
});

test('matrix windows remain bounded after a large scroll is followed by a narrow search',()=>{
 const blocks=Array.from({length:5000},(_,i)=>block('file:'+i)),data={blocks,frontier:blocks.map(b=>b.id),links:[]};
 const window=Map.matrixWindow(data,{...Map.defaults(),search:'file:4999'},100000,100000);assert.deepEqual(window.ids,['file:4999']);assert.equal(window.rowStart,0);assert.equal(window.colStart,0);assert.equal(window.rowEnd,1);assert.equal(window.colEnd,1);
});

test('same-label modules remain distinct across languages and searches preserve both',()=>{
 const php=block('module:php:Common'),python=block('module:python:Common');php.label=python.label='Common';
 const data={blocks:[php,python],frontier:[php.id,python.id],links:[{from:php.id,to:python.id,count:3}]},ui={...Map.defaults(),group:'module',search:'common'};
 assert.deepEqual(Map.selection(data,ui).ids,[php.id,python.id]);assert.equal(Map.layout(data,ui).cards.length,2);assert.equal(Map.matrixWindow(data,ui).links.get(JSON.stringify([php.id,python.id])).count,3);
});

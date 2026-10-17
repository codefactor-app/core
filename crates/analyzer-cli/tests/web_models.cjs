const {test}=require('node:test');
const assert=require('node:assert/strict');
const Tree=require('../web/tree-model.js');
const Graph=require('../web/graph-model.js');
const Relations=require('../web/relations-model.js');

test('FQN search reveals nested declarations through collapsed file/type/method ancestors',()=>{
 const nodes=Tree.prepare([{id:'src',kind:'directory',name:'src',children:[{id:'src/app.ts',kind:'file',name:'app.ts',path:'src/app.ts',violations:1,children:[{id:'Box',kind:'class',name:'Box',fqn:'app.Box',children:[{id:'run',kind:'method',name:'run',fqn:'app.Box.run',children:[{id:'nested',kind:'function',name:'nested',fqn:'app.Box.run.nested',violations:1,children:[]}]}]}]}]}]);
 assert.equal(Tree.visible(nodes).rows.length,1);
 const filtered=Tree.visible(nodes,'BOX.RUN.NESTED');
 assert.deepEqual(filtered.rows.map(row=>row.node.id),['/src','src/app.ts','Box','run','nested']);
 assert.equal(filtered.files,1);
 assert.equal(nodes[0].violations,1);
 assert.deepEqual(Tree.visible(nodes,'',true).rows.map(row=>row.node.id),filtered.rows.map(row=>row.node.id));
});

test('hidden vendor evidence attaches to visible endpoint without inventing target nodes',()=>{
 const data={nodes:[{id:'app',vendor:false}],hidden_vendor_nodes:[{id:'dep',vendor:true,package:'node_modules/demo'}],hidden_vendor_edges:[{from:'app',to:'dep',callsite:{file:'app.ts',start_line:2}}]};
 const index=Graph.hiddenVendorIndex(data);
 assert.equal(index.get('app').edges.length,1);
 assert.deepEqual([...index.get('app').packages],['node_modules/demo']);
 assert(!index.has('dep'));
});

test('chain tree accumulates each path guards and terminates recursive cycles',()=>{
 const data={root:'a',nodes:['a','b','c'].map(id=>({id})),edges:[{from:'a',to:'b',guard:[{normalized:'enabled'}]},{from:'b',to:'c',guard:[{normalized:'ready'}]},{from:'c',to:'a',guard:[]}]};
 const result=Graph.chainRows(data,'callees');
 assert.deepEqual(result.rows.map(row=>row.id),['a','b','c','a']);
 assert.deepEqual(result.rows[2].guards.map(guard=>guard.normalized),['enabled','ready']);
 assert.equal(result.rows[3].cycle,true);
 assert.equal(result.truncated,false);
 assert.equal(Graph.chainRows(data,'callees',2).truncated,true);
});

test('relations clusters use actual set endpoints and preserve direct versus transitive evidence',()=>{
 const data={a:['a'],b:['b'],nodes:[{id:'a',label:'A',file:'a.ts'},{id:'middle',file:'middle.ts'},{id:'b',label:'B',file:'b.ts'}],a_to_b:[],b_to_a:[{from:'b',to:'a'}],paths_a_to_b:[{from:'a',to:'b',edges:[{from:'a',to:'middle'},{from:'middle',to:'b'}]}]};
 const result=Relations.layout(data,'file');
 assert.deepEqual(result.a.map(group=>group.id),['a.ts']);
 assert.deepEqual(result.b.map(group=>group.id),['b.ts']);
 assert.equal(result.connections.find(edge=>!edge.reverse).paths,1);
 assert.equal(result.connections.find(edge=>edge.reverse).calls,1);
 assert(!result.connections.some(edge=>edge.left==='middle.ts'));
});

const {test}=require('node:test');
const assert=require('node:assert/strict');
const Relations=require('../web/relations-model.js');
const Tree=require('../web/tree-model.js');
const call=(from,to,line=1,extra={})=>({from,to,confidence:'resolved',callsite:{file:from+'.ts',start_line:line},guard:[],in_loop:[],...extra});
function fixture(){const first=call('a','x',3,{guard:[{raw:'enabled',normalized:'enabled',branch:'then'}],in_loop:[{header:'for i in items',line:2}]}),second=call('x','b'),reverse=call('b','a');return {level:'function',a:['a'],b:['b'],nodes:[{id:'a',label:'Alpha',file:'a.ts'},{id:'x',label:'Bridge',file:'x.ts'},{id:'b',label:'Beta',file:'b.ts'}],a_to_b:[],b_to_a:[reverse],paths_a_to_b:[{from:'a',to:'b',nodes:['a','x','b'],edges:[first,second]}],paths_b_to_a:[{from:'b',to:'a',nodes:['b','a'],edges:[reverse]}],internal_a:[call('a','a')],routes_evaluated:true,truncated:true,truncation_reasons:['work_budget'],work_used:7,options:{work_budget:7}};}

test('relation URLs roundtrip selectors, exclusions, direction and budgets without changing other tabs',()=>{
 const ui={...Relations.defaults,a:'ns:App\\Заказы,glob:src/a&b/**',b:'set:second',exclude_a:'file:bad.ts',level:'file',direction:'b_to_a',view:'matrix',transitive:true,internal:true,depth:5,max_paths_total:300};
 const params=Relations.writeState(new URLSearchParams('tab=config&file=src/main.ts'),ui);
 assert.deepEqual(Relations.readState(new URLSearchParams(params.toString())),ui);
 assert.equal(params.get('tab'),'config');assert.equal(params.get('file'),'src/main.ts');
 const malformed=Relations.readState(new URLSearchParams('rel_depth=-2&rel_direction=invalid&rel_max_paths_total=900000'));
 assert.equal(malformed.depth,10);assert.equal(malformed.direction,'both');assert.equal(malformed.max_paths_total,10000);
});
test('fuzzy search matches abbreviations, transpositions and Unicode while rejecting unrelated names',()=>{
 for(const [name,query] of [['app.Box.run','bxrn'],['app.render','rendre'],['ЗагрузитьЗаказы','Згрззк']])assert.notEqual(Tree.fuzzyScore(name,query),null);
 assert.equal(Tree.fuzzyScore('app.Box.run','unrelated'),null);
 assert(Tree.fuzzyScore('render','render')<Tree.fuzzyScore('renderWidget','render'));
 const nodes=Tree.prepare([{id:'a.ts',name:'a.ts',kind:'file',children:[{id:'fn:a',name:'renderWidget',fqn:'app.renderWidget',kind:'function'}]}]);
 assert.deepEqual(Tree.visible(nodes,'rndrw').rows.map(row=>row.node.id),['a.ts','fn:a']);
});
test('tree selection preserves exact declaration identity and makes vendor selection explicit',()=>{
 assert.equal(Tree.selector({kind:'file',path:'src/a.ts'}),'file:src/a.ts');
 assert.equal(Tree.selector({kind:'class',fqn:'app.Box'}),'class:app.Box');
 assert.equal(Tree.selector({kind:'method',id:'tree-id',symbol_id:'fn:typescript:src/a.ts:app.Box.run'}),'fn:typescript:src/a.ts:app.Box.run');
 assert.equal(Tree.selector({kind:'directory',id:'/src/lib',children:[{kind:'file',vendor:false}]}),'dir:src/lib');
 assert.equal(Tree.selector({kind:'file',path:'node_modules/demo/index.ts',vendor:true}),'vendor:node_modules/demo/index.ts');
 assert.equal(Tree.selector({kind:'method',symbol_id:'fn:vendor',vendor:true}),'vendor:fn:vendor');
});
test('transitive layout shows real intermediate nodes and retains every hop in arrow details',()=>{
 const data=fixture(),layout=Relations.layout(data);
 assert.deepEqual(layout.middle.map(node=>node.id),['x']);
 assert(layout.segments.some(edge=>edge.from.side==='a'&&edge.to.side==='x'));
 assert(layout.segments.some(edge=>edge.from.side==='x'&&edge.to.side==='b'));
 const segment=layout.segments.find(edge=>edge.to.side==='x'),detail=layout.entries[segment.details[0]];
 assert.equal(detail.path.edges[0].guard[0].raw,'enabled');assert.equal(detail.path.edges[0].in_loop[0].header,'for i in items');
 assert.equal(layout.connections.find(edge=>!edge.reverse).paths,1);
 assert.equal(Relations.layout(data,'set').middle[0].id,'X');
});
test('direction applies to graph, matrix, evidence and export counters; internal edges are opt in',()=>{
 const data=fixture(),forward=Relations.filtered(data,'a_to_b');
 assert.equal(forward.b_to_a.length,0);assert.equal(forward.paths_b_to_a.length,0);assert.equal(forward.internal_a.length,0);
 assert.equal(forward.route_count,1);assert.equal(forward.direct_call_count,0);
 const matrix=Relations.matrixRows(forward);assert.equal(matrix.length,1);assert.deepEqual([matrix[0].a,matrix[0].b,matrix[0].count,matrix[0].direct],['Alpha','Beta',1,0]);
 assert.equal(Relations.entries(forward)[matrix[0].details[0]].path.nodes[1],'x');
 const reverse=Relations.filtered(data,'b_to_a',true);assert.equal(Relations.layout(reverse).middle.length,0);assert.equal(reverse.direct_call_count,1);assert.equal(reverse.internal_a.length,1);
});
test('Markdown export retains selected routes, callsites, raw guards, loop contexts and truncation',()=>{
 const md=Relations.markdown(Relations.filtered(fixture(),'a_to_b'),{level:'function',direction:'a_to_b'});
 assert(md.includes('Bridge'));assert(md.includes('a.ts:3'));assert(md.includes('enabled'));assert(md.includes('В цикле: for i in items'));assert(md.includes('work\\_budget'));assert(md.includes('direction: a\\_to\\_b'));
});

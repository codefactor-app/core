const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

function render(node) {
  const source=fs.readFileSync(require.resolve('../web/app.js'),'utf8');
  const functions=source.slice(source.indexOf('  function renderGraphFrame('),source.indexOf('  function bindGraphInteractions('));
  const svg=source.slice(source.indexOf('  function svgAction('),source.indexOf('  function renderCoverage('));
  const esc=value=>String(value??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  return vm.runInNewContext(svg+functions+'\nrenderGraphNode(node,"root",positions)',{
    node,positions:new Map([[node.id,{x:0,y:0}]]),esc,num:value=>String(value??0),short:String,shortPath:String,
    algorithmicLabel:n=>n.algorithmic_complexity?.formula||'',
    CodefactorGraph:{nodeCard:n=>({offset:0,height:170,className:n.parent_type,classLines:[n.parent_type],functionLines:[n.label],nameY:45})},
    state:{symbol:{},branchBusy:new Map(),hiddenVendor:new Map(),graphView:{hidden:()=>({nodes:0,branches:0}),expanded:()=>false}},
  });
}

test('function cards keep headings separate from metrics and escape source names',()=>{
  const html=render({id:'root',label:'<script>unsafe</script>',kind:'function',parent_type:'Example',metrics:{parameters:2,complexity:3,fan_in:4,fan_out:5}});
  assert.match(html,/>ФУНКЦИЯ<\/text>/);
  assert.match(html,/P 2 · CYC 3 · ← 4 · → 5/);
  assert.match(html,/&lt;script&gt;unsafe&lt;\/script&gt;/);
  assert.doesNotMatch(html,/<script>/);
});

test('fold cards show a group heading, member count and expansion actions',()=>{
  const html=render({id:'fold',label:'Group',kind:'fold',category:'callers',members:['a','b']});
  assert.match(html,/>ГРУППА<\/text>/);
  assert.match(html,/2 функций скрыто · вызывают выбранный метод/);
  assert.match(html,/data-neighbors-expand="fold"/);
});

test('unknown targets retain boundary information and cost cards retain estimate status',()=>{
  const unknown=render({id:'unknown',label:'Unknown',kind:'unknown',unresolved:{reason:'type_not_analyzed'}});
  assert.match(unknown,/Имя метода известно · код вне каталога/);
  assert.match(unknown,/Продолжение вне каталога/);
  const cost=render({id:'cost',label:'Cost',kind:'function',algorithmic_complexity:{formula:'N',status:'upper_bound'}});
  assert.match(cost,/Граница: N/);
});

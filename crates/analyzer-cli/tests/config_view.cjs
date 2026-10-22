const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

test('active entity patterns render in order with escaped titles and actual thresholds',async()=>{
 const source=fs.readFileSync(require.resolve('../web/app.js'),'utf8');
 const view=source.slice(source.indexOf('  function thresholdTable('),source.indexOf('  const relationFields='));
 const element={innerHTML:''},state={};
 const esc=value=>String(value??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
 const data={effective:{limits:{function_params:{warn:4,error:7},by_entity_glob:[{glob:'first<&>*',function_params:{warn:0,error:1}},{glob:'second*',function_lines:{warn:9,error:12}}]}}};
 await vm.runInNewContext(view+'\nloadConfig()',{$:()=>element,api:async()=>data,state,esc,num:String,help:id=>({title:id,description:''})});
 assert.equal(state.configLoaded,true);
 assert.match(element.innerHTML,/Пороги по шаблонам сущностей/);
 assert.match(element.innerHTML,/<h3>first&lt;&amp;&gt;\*<\/h3>/);
 assert.match(element.innerHTML,/<td>0<\/td><td>1<\/td>/);
 assert(element.innerHTML.indexOf('first&lt;')<element.innerHTML.indexOf('<h3>second*'));
 assert.doesNotMatch(element.innerHTML,/<code>by_entity_glob<\/code>/);
 assert.match(element.innerHTML,/общие → язык → путь → шаблон сущности → точная сущность/);
});

((root, factory) => {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.CodefactorRelations = model;
})(typeof globalThis === 'object' ? globalThis : this, () => {
  'use strict';
  const defaults={a:'',b:'',exclude_a:'',exclude_b:'',level:'function',direction:'both',view:'graph',transitive:false,internal:false,depth:10,path_mode:'one_shortest',max_paths_per_pair:100,max_paths_total:10000,work_budget:1000000};
  function readState(params) {
    const state={...defaults};
    for(const key of Object.keys(defaults)){const value=params.get('rel_'+key);if(value!==null)state[key]=value;}
    for(const key of ['transitive','internal'])state[key]=state[key]===true||state[key]==='true';
    for(const [key,max] of [['depth',100],['max_paths_per_pair',1000],['max_paths_total',10000],['work_budget',1000000]])state[key]=/^\d+$/.test(String(state[key]))&&Number(state[key])>0?Math.min(max,Number(state[key])):defaults[key];
    for(const [key,values] of [['level',['set','file','class','function']],['direction',['a_to_b','b_to_a','both']],['view',['graph','matrix','calls']],['path_mode',['one_shortest','all_shortest','simple']]])if(!values.includes(state[key]))state[key]=defaults[key];
    return state;
  }
  function writeState(params,state) {
    for(const key of Object.keys(defaults)){params.delete('rel_'+key);if(state[key]!==undefined&&String(state[key])!==String(defaults[key]))params.set('rel_'+key,String(state[key]));}
    return params;
  }
  function filtered(data,direction='both',internal=false) {
    const result={...data,a_to_b:direction==='b_to_a'?[]:data.a_to_b||[],b_to_a:direction==='a_to_b'?[]:data.b_to_a||[],paths_a_to_b:direction==='b_to_a'?[]:data.paths_a_to_b||[],paths_b_to_a:direction==='a_to_b'?[]:data.paths_b_to_a||[],internal_a:internal?data.internal_a||[]:[],internal_b:internal?data.internal_b||[]:[],matrix:{},direct_call_matrix:{}};
    const rows=matrixRows(result,data.level||'function');for(const row of rows){(result.matrix[row.a]||={})[row.b]=row.count;(result.direct_call_matrix[row.a]||={})[row.b]=row.direct;}
    result.direct_call_count=new Set([...result.a_to_b,...result.b_to_a].map(edge=>JSON.stringify([edge.from,edge.to,edge.callsite]))).size;
    result.route_count=new Set([...result.paths_a_to_b,...result.paths_b_to_a].map(path=>JSON.stringify(path.edges.map(edge=>[edge.from,edge.to,edge.callsite])))).size;if(!data.routes_evaluated&&!result.paths_a_to_b.length&&!result.paths_b_to_a.length)result.route_count=result.direct_call_count;
    return result;
  }
  function entries(data) {
    const result=[];
    for(const side of ['a_to_b','b_to_a','internal_a','internal_b'])for(const edge of data[side]||[])result.push({side,edge,from:edge.from,to:edge.to});
    for(const side of ['a_to_b','b_to_a'])for(const path of data['paths_'+side]||[])if(path.edges?.length>1)result.push({side,path,from:path.from,to:path.to});
    return result;
  }
  const routeNodes=path=>path.nodes?.length?path.nodes:[path.from,...path.edges.map(edge=>edge.to)];
  function grouping(data,level) {
    const nodes=new Map((data.nodes||[]).map(node=>[String(node.id),node]));
    return (id,side)=>level==='set'?side.toUpperCase():level==='file'?nodes.get(String(id))?.file||String(id):level==='class'?nodes.get(String(id))?.parent_type||nodes.get(String(id))?.file||String(id):String(id);
  }
  function matrixRows(data,level='function') {
    const group=grouping(data,level),nodes=new Map((data.nodes||[]).map(node=>[String(node.id),node])),key=(id,side)=>level==='function'?nodes.get(String(id))?.label||String(id):group(id,side),rows=new Map(),items=entries(data);
    const row=(from,to,side)=>{const reverse=side==='b_to_a',a=key(from,reverse?'b':'a'),b=key(to,reverse?'a':'b'),id=JSON.stringify([a,b]);if(!rows.has(id))rows.set(id,{a,b,count:0,direct:0,details:[],routes:new Set(),calls:new Set()});return rows.get(id);};
    items.forEach((item,index)=>{if(item.side.startsWith('internal'))return;const entry=row(item.from,item.to,item.side);entry.details.push(index);if(item.edge){const id=JSON.stringify([item.from,item.to,item.edge.callsite]);entry.calls.add(id);}});
    for(const side of ['a_to_b','b_to_a'])for(const path of data['paths_'+side]||[]){const entry=row(path.from,path.to,side);entry.routes.add(JSON.stringify(path.edges.map(edge=>[edge.from,edge.to,edge.callsite])));}
    const old=!data.routes_evaluated&&!(data.paths_a_to_b?.length||data.paths_b_to_a?.length);
    return [...rows.values()].map(row=>({a:row.a,b:row.b,count:old?row.calls.size:row.routes.size,direct:row.calls.size,details:[...new Set(row.details)]})).sort((a,b)=>a.a.localeCompare(b.a)||a.b.localeCompare(b.b));
  }
  function layout(data, level = 'function', limit = 60) {
    const nodes=new Map((data.nodes||[]).map(node=>[String(node.id),node])),key=grouping(data,level),membersA=new Set(data.a||[]),membersB=new Set(data.b||[]);
    function groups(ids,side){const result=new Map();for(const id of ids||[]){const group=key(id,side),node=nodes.get(String(id));if(!result.has(group))result.set(group,{id:group,label:level==='function'?node?.label||String(id):group,members:[]});result.get(group).members.push(String(id));}return [...result.values()].sort((a,b)=>a.label.localeCompare(b.label));}
    const allA=groups(data.a,'a'),allB=groups(data.b,'b'),a=allA.slice(0,limit),b=allB.slice(0,limit),shownA=new Set(a.map(n=>n.id)),shownB=new Set(b.map(n=>n.id));
    const middleIds=new Set();for(const item of entries(data))for(const id of item.path?routeNodes(item.path).slice(1,-1):[])if(!membersA.has(id)&&!membersB.has(id))middleIds.add(id);
    const allMiddle=groups(middleIds,'x'),middle=allMiddle.slice(0,limit),shownMiddle=new Set(middle.map(n=>n.id)),connections=new Map(),segments=new Map(),items=entries(data);
    function point(id,preferred){const side=preferred||(membersA.has(id)?'a':membersB.has(id)?'b':'x'),group=key(id,side),shown=side==='a'?shownA:side==='b'?shownB:shownMiddle;return shown.has(group)?{side,id:group}:null;}
    function segment(from,to,index,transitive){if(!from||!to)return;const id=JSON.stringify([from,to]);if(!segments.has(id))segments.set(id,{from,to,details:[],transitive:false});const entry=segments.get(id);entry.details.push(index);entry.transitive ||= transitive;}
    items.forEach((item,index)=>{
      const internal=item.side.startsWith('internal'),reverse=item.side==='b_to_a',from=point(item.from,internal?item.side.slice(-1):reverse?'b':'a'),to=point(item.to,internal?item.side.slice(-1):reverse?'a':'b');
      if(!from||!to)return;
      if(!internal){const left=reverse?to.id:from.id,right=reverse?from.id:to.id,id=JSON.stringify([left,right,reverse]);if(!connections.has(id))connections.set(id,{left,right,reverse,calls:0,paths:0,routes:0,details:[]});const entry=connections.get(id);entry.details.push(index);if(item.path){entry.paths++;entry.routes++;}else entry.calls++;}
      if(item.path){const route=[from,...routeNodes(item.path).slice(1,-1).map(id=>point(id)),to];for(let i=1;i<route.length;i++)segment(route[i-1],route[i],index,true);}
      else segment(from,to,index,false);
    });
    return {a,b,middle,connections:[...connections.values()],segments:[...segments.values()],entries:items,omittedA:allA.length-a.length,omittedB:allB.length-b.length,omittedMiddle:allMiddle.length-middle.length};
  }
  const cell=value=>String(value??'—').replace(/\\/g,'\\\\').replace(/[|`*_[\]<>]/g,ch=>ch==='<'?'&lt;':ch==='>'?'&gt;':'\\'+ch).replace(/[\n\r]/g,' ');
  function markdown(data,ui={}) {
    const labels=new Map((data.nodes||[]).map(n=>[n.id,n.label||n.id])),name=id=>cell(labels.get(id)||id);
    const out=['# Связи между наборами кода','',...Object.entries(ui).map(([key,value])=>`- ${key}: ${cell(value)}`),'',`Прямые A → B: ${(data.a_to_b||[]).length}; B → A: ${(data.b_to_a||[]).length}.`, `Режим путей: ${cell(data.path_mode)}. Бюджет: ${data.work_used||0} / ${data.options?.work_budget||0}.`, `Усечение: ${data.truncated?'да: '+cell((data.truncation_reasons||[]).join(', ')):'нет'}.`,...((data.warnings||[]).map(w=>`Предупреждение: ${cell(w)}`)), ''];
    for(const side of ['a','b'])out.push(`## Набор ${side.toUpperCase()}`,'',...(data[side]||[]).map(id=>`- ${name(id)}`),'');
    out.push('## Матрица','', '| Источник | Цель | Маршруты | Прямые вызовы |','|---|---|---:|---:|',...matrixRows(data,ui.level||data.level).map(row=>`| ${name(row.a)} | ${name(row.b)} | ${row.count} | ${row.direct} |`),'','## Места вызова и пути','');
    for(const item of entries(data)){out.push(`### ${item.path?'Путь':'Вызов'} ${name(item.from)} → ${name(item.to)}`,'');for(const edge of item.path?.edges||[item.edge]){out.push(`- ${name(edge.from)} → ${name(edge.to)} · ${cell(edge.callsite?.file)}:${Number(edge.callsite?.start_line)||1} · ${cell(edge.confidence)}`);for(const guard of edge.guard||[])out.push(`  - Условие (${cell(guard.branch||guard.kind)}): ${cell(guard.normalized||guard.raw)}; исходное: ${cell(guard.raw)}`);for(const loop of edge.in_loop||[])out.push(`  - В цикле: ${cell(loop.header||loop.kind)} · строка ${Number(loop.line)||1}`);}out.push('');}
    return out.join('\n');
  }
  return {defaults,readState,writeState,filtered,entries,matrixRows,layout,markdown};
});

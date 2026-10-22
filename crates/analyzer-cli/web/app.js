(() => {
  'use strict';
  const $ = (selector, root = document) => root.querySelector(selector);
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const num = value => Number(value || 0).toLocaleString('ru-RU', {maximumFractionDigits:2});
  const state = {tree:[], fileTab:'code', fileIssuesRequest:0, fileIssuesPage:1, fileIssuesPages:1, file:null, fileData:null, fileRequest:0, filePending:null, tab:'code', catalog:new Map(), query:'', warningsOnly:false,
    symbol:null, graph:null, graphRequest:0, lazyGraph:null, branchTokens:new Map(), branchBusy:new Map(), branchEpoch:0, direction:'both', depth:'', external:true, category:'', vendor:'collapsed', raw:false,
    scale:1, pan:[0,0], history:[], historyAt:-1, graphLimit:600, imported:null, sourceLinks:[],
    relation:null, relationRequest:0, relationPage:1, qualityPage:1, qualityPages:1, qualityRequest:0, architecturePage:1, architecturePages:1, architectureRequest:0, revision:null, configLoaded:false, graphView:null, graphLayout:null, nodePositions:new Map(), collapseSimple:true, groupClasses:true, suppressGraphClickUntil:0};
  const graphModal = $('#graph-modal');
  const graphResponses=new Map(),symbolDetails=new Map();
  const debounce = (fn, delay = 250) => {let timer; return (...args) => {clearTimeout(timer); timer=setTimeout(()=>fn(...args),delay);};};
  async function api(path) {
    const response = await fetch(path, {headers:{Accept:'application/json'}, cache:'no-store'});
    if (!response.ok) throw new Error(`Не удалось загрузить данные (${response.status}).`);
    const data=await response.json();
    for(const [field,source,marker] of [['coverage_nodes','nodes','coverage_nodes_same_as_nodes'],['coverage_edges','edges','coverage_edges_same_as_edges'],['coverage_cycles','cycles','coverage_cycles_same_as_cycles'],['requested_coverage','coverage','requested_coverage_same_as_coverage'],['loaded_coverage','coverage','loaded_coverage_same_as_coverage']]) {
      if(data[marker])data[field]=Array.isArray(data[source])?[...data[source]]:data[source];
    }
    return data;
  }
  function toast(message) {const box=$('#toast');box.textContent=message;box.classList.remove('hidden');clearTimeout(toast.timer);toast.timer=setTimeout(()=>box.classList.add('hidden'),4500);}
  function help(id) {const base=String(id||'').split('.')[0]; const item=state.catalog.get(base);return item ? {...item,title:item.title+({'.avg':' · среднее','.median':' · медиана','.max':' · максимум','.p90':' · p90','.p95':' · p95'}[String(id).slice(base.length)]||String(id).slice(base.length))} : {title:id||'Правило проекта',description:'Дополнительное правило проекта. Значение сравнивается с заданным порогом.'};}
  function setHash() {
    if(state.restoringHash)return;
    const params=new URLSearchParams({tab:state.tab});
    if(state.file) params.set('file',state.file);
    if(graphModal.open && state.symbol) {params.set('symbol',state.symbol.id);params.set('direction',state.direction);if(state.depth)params.set('depth',state.depth);}
    CodefactorRelations.writeState(params,relationUi());if($('#relations-modal').open)params.set('relations','1');
    mapUi.writeState(params);
    // Internal updates do not create browser history entries for zoom or filters.
    history.replaceState(null,'',`#${params}`);
  }
  function switchTab(tab) {
    if(tab!=='codemap')document.body.classList.remove('map-fullscreen');
    state.tab=['code','codemap','quality','architecture','config'].includes(tab)?tab:'code';
    for(const button of document.querySelectorAll('[data-tab]')) {const active=button.dataset.tab===state.tab;button.classList.toggle('active',active);button.setAttribute('aria-selected',String(active));button.tabIndex=active?0:-1;}
    for(const page of document.querySelectorAll('main > [role=tabpanel]'))page.classList.toggle('hidden',page.id!==`page-${state.tab}`);
    if(state.tab==='codemap')mapUi.refresh(state.revision);
    if(state.tab==='quality')loadViolations();
    if(state.tab==='architecture')loadArchitecture();
    if(state.tab==='config'&&!state.configLoaded)loadConfig();
    if(state.tab==='code'){requestAnimationFrame(renderTree);if(!state.treeLoaded)loadTree(true).catch(error=>toast(error.message));}
    setHash();
  }
  function renderTree() {
    const pane=$('#tree');if(state.tab!=='code')return;
    const {rows,files}=CodefactorTree.visible(state.tree,state.query,state.warningsOnly),height=34,start=Math.max(0,Math.floor(pane.scrollTop/height)-8),end=Math.min(rows.length,start+Math.ceil((pane.clientHeight||400)/height)+16);
    if(start>=rows.length&&rows.length){pane.scrollTop=0;return renderTree();}
    const icons={directory:'▱',dir:'▱',file:'▤',class:'C',struct:'S',interface:'I',trait:'T',enum:'E',method:'m',function:'ƒ'};
    pane.innerHTML=rows.length ? `<div style="height:${start*height}px"></div>`+rows.slice(start,end).map(({node,depth})=>`<button class="tree-row${node.id===(state.treeSelection||state.file)?' selected':''}" style="--depth:${depth}" data-tree-node="${esc(node.id)}" ${node.children.length?`aria-expanded="${Boolean(state.query||state.warningsOnly||!node.collapsed)}"`:''} title="${esc(node.fqn||node.path||node.name)}"><span class="tree-disclosure" ${node.children.length?`data-tree-toggle="${esc(node.id)}"`:''}>${node.children.length?node.collapsed&&!state.query&&!state.warningsOnly?'›':'⌄':''}</span><span class="tree-kind">${icons[node.kind]||'◈'}</span><span class="tree-name">${esc(node.name)}</span>${node.vendor?'<span class="tree-vendor" title="Код зависимости">V</span>':''}${node.metrics?.complexity!=null?`<span class="tree-metric" title="Параметры / цикломатическая сложность">${num(node.metrics.parameters)} / ${num(node.metrics.complexity)}</span>`:node.loc?`<span class="tree-metric" title="Строки кода">${num(node.loc.code)}</span>`:''}${node.violations?`<span class="tree-warning" title="Нарушения">${num(node.violations)}</span>`:''}</button>`).join('')+`<div style="height:${Math.max(0,rows.length-end)*height}px"></div>` : '<div class="empty">Сущности не найдены.</div>';
    $('#tree-count').textContent=`${num(files)} файлов · нечёткий поиск по пути, имени и FQN`;
  }
  function findFileNode(id, nodes=state.tree) {for(const node of nodes){if(node.id===id)return node;const found=findFileNode(id,node.children);if(found)return found;}return null;}
  async function loadTree(full=state.tab==='code'||state.treeLoaded) {
    const token=(state.treeLoadToken||0)+1;state.treeLoadToken=token;const data=await api('/api/tree?compact=true'+(full?'':'&summary_only=true'));if(token!==state.treeLoadToken)return;state.treeLoaded=!data.summary_only;const collapsed=new Map();const save=list=>list.forEach(n=>{collapsed.set(n.id,n.collapsed);save(n.children);});save(state.tree);
    state.tree=CodefactorTree.prepare(data.children||[]);const restore=list=>list.forEach(n=>{if(collapsed.has(n.id))n.collapsed=collapsed.get(n.id);restore(n.children);});restore(state.tree);
    state.strategyCapabilities=data.strategy_capabilities||{};
    mapUi.setNamedSets(data.named_sets||{});
    const named=Object.keys(data.named_sets||{});for(const side of ['a','b']){const select=$('#relation-set-'+side),old=select.value;select.innerHTML='<option value="">Свои селекторы</option>'+named.map(name=>`<option value="${esc(name)}">${esc(name)}</option>`).join('');select.value=old;}$('#project-name').textContent=data.project||'Проект';const s=data.summary||{};
    $('#project-stats').textContent=`${num(s.files)} файлов · ${num(s.functions)} функций · ${num(s.types)} типов`;
    const architectureWarnings=s.architecture_warnings??0;
    $('#violation-count').textContent=num(s.errors+s.warnings-architectureWarnings);$('#architecture-count').textContent=num(architectureWarnings);renderTree();
  }
  async function loadFile(id,line) {
    const request=++state.fileRequest;++state.fileIssuesRequest;
    state.filePending={id,line};
    if(id!==state.file||line){state.fileIssuesPage=1;$('#file-severity').value='';switchFileTab('code');}
    $('#file-tab-issues').disabled=true;
    $('#file-title').textContent=id;$('#code').innerHTML='<div class="empty">Загрузка исходного кода…</div>';$('#file-functions').classList.add('hidden');
    try {
      const file=await api(`/api/file?id=${encodeURIComponent(id)}`);if(request!==state.fileRequest)return;
      state.file=file.path||id;state.fileData=file;$('#file-title').textContent=state.file;$('#file-language').textContent=String(file.language||'КОД').toUpperCase();
      $('#file-loc').textContent=file.loc?`${num(file.loc.total)} строк · ${num(file.loc.code)} строк кода`:'';
      $('#function-select').innerHTML=(file.functions||[]).map((f,i)=>`<option value="${i}">${esc(f.fqn||f.name)} · P ${f.metrics?.parameters??'—'} / CYC ${f.metrics?.complexity??'—'}${algorithmicLabel(f)?' · '+esc(algorithmicLabel(f)):''} · строка ${f.line||f.range?.start_line||1}</option>`).join('');
      $('#file-functions').classList.toggle('hidden',state.fileTab!=='code'||!file.functions?.length);$('#file-tab-issues').disabled=false;$('#file-issue-count').textContent=num((file.violation_count??file.violations?.length??0)+(file.parse_error_count||0));$('#file-tab-issues').classList.toggle('has-issues',Number(file.violation_count??file.violations?.length??0)+(file.parse_error_count||0)>0);renderCode(file,line);renderTree();if(state.fileTab==='issues')loadFileIssues();setHash();
    } catch(error) {if(request===state.fileRequest){state.fileData=null;$('#code').innerHTML=`<div class="empty error">${esc(error.message)}<br/>Исходный код зависимостей может быть недоступен.</div>`;toast(error.message);}}
    finally {if(request===state.fileRequest)state.filePending=null;}
  }
  function highlight(text) {
    const pattern=/(\/\/[^\n]*|#[^\n]*|\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\b(?:async|await|fn|function|class|interface|trait|struct|enum|impl|if|else|for|while|loop|return|match|switch|case|try|catch|throw|new|const|let|var|def|import|from|use|public|private|static|mut|self|this|true|false|null|None|Some)\b|\b\d+(?:\.\d+)?\b)/g;
    let result='',cursor=0;for(const match of String(text).matchAll(pattern)){const token=match[0];result+=esc(text.slice(cursor,match.index));const cls=/^(\/\/|#|\/\*)/.test(token)?'comment':/^["']/.test(token)?'string':/^\d/.test(token)?'number':'keyword';result+=`<span class="tok-${cls}">${esc(token)}</span>`;cursor=match.index+token.length;}return result+esc(text.slice(cursor));
  }
  function byteIndex(line,column) {let bytes=0,index=0;const encoder=new TextEncoder();for(const ch of line){const width=encoder.encode(ch).length;if(bytes+width>column)break;bytes+=width;index+=ch.length;}return index;}
  function renderCode(file,jumpLine) {
    const declarations=new Map();for(const fun of file.functions||[]){const line=Number(fun.range?.start_line||fun.line);if(!declarations.has(line))declarations.set(line,[]);declarations.get(line).push(fun);}
    const rows=String(file.source||'').split(/\r?\n/), refs=new Map(), marked=new Set(file.violation_lines||(file.violations||[]).map(v=>Number(v.line)));state.sourceLinks=[];
    function add(line,start,end,targets){const index=state.sourceLinks.push(targets)-1;if(!refs.has(line))refs.set(line,[]);refs.get(line).push({start,end,index});}
    for(const fun of file.functions||[]) {
      const range=fun.range||{};const line=Number(range.start_line||fun.line),row=rows[line-1]||'';
      const start=row.indexOf(fun.name,byteIndex(row,Math.max(0,(range.start_column||1)-1)));
      if(start>=0)add(line,start,start+fun.name.length,[{...fun,label:fun.fqn,file:file.path,line}]);
    }
    for(const call of file.calls||[]) {
      const r=call.range||{},line=Number(r.start_line),row=rows[line-1]||'',targets=(call.resolved_target_details||[]).filter(n=>n.category!=='runtime'&&!n.standard);
      if(!targets.length)continue;
      const token=String(call.name||'').split(/::|->|\?\.|\./).filter(Boolean).at(-1);
      if(!token)continue;const from=byteIndex(row,Math.max(0,(r.start_column||1)-1)), end=r.end_line===r.start_line?byteIndex(row,Math.max(0,r.end_column-1)):row.length;
      const start=row.indexOf(token,from);if(start>=from&&start+token.length<=end)add(line,start,start+token.length,targets);
    }
    $('#code').innerHTML=`<div class="code-lines">${rows.map((row,i)=>{let content='',cursor=0;for(const ref of (refs.get(i+1)||[]).sort((a,b)=>a.start-b.start)){if(ref.start<cursor)continue;content+=highlight(row.slice(cursor,ref.start))+`<button class="source-link" data-source-link="${ref.index}" title="Открыть граф вызовов · ${esc(state.sourceLinks[ref.index].map(n=>n.label||n.fqn).join(', '))}">${highlight(row.slice(ref.start,ref.end))}</button>`;cursor=ref.end;}content+=highlight(row.slice(cursor));return `<div class="code-row${marked.has(i+1)?' violation-line':''}" id="line-${i+1}"><span class="line-no">${i+1}</span><code>${content}</code>${(declarations.get(i+1)||[]).map(fun=>`<span class="function-counters" title="${esc(fun.parent_type||'Свободная / вложенная функция')} · P — параметры, CYC — независимые пути, ← / → — рёбра вызова">${nodeMetrics(fun,true)}</span>`).join('')}</div>`;}).join('')}</div>`;
    if(jumpLine)requestAnimationFrame(()=>{const row=$(`#line-${Number(jumpLine)}`);row?.scrollIntoView({block:'center'});row?.classList.add('focus-line');});
  }
  function chooseTargets(targets) {
    state.imported=null;
    if(targets.length===1){openGraph(targets[0]);return;}
    $('#target-options').innerHTML=targets.map((n,i)=>`<button data-target-option="${i}">${esc(n.label||n.id)}<br/><small>${esc(n.file||'Внешняя операция')}:${n.line||''}</small></button>`).join('');
    $('#target-options').onclick=e=>{const button=e.target.closest('[data-target-option]');if(button){$('#target-modal').close();openGraph(targets[Number(button.dataset.targetOption)]);}};
    $('#target-modal').showModal();
  }
  function normalizeSymbol(node) {return {...node,id:String(node.id||node.fqn),label:node.label||node.fqn||node.name||node.id,line:node.line||node.range?.start_line,file:node.file||node.file_id};}
  function openGraph(node,record=true) {
    const symbol=normalizeSymbol(node);if(!symbol.id||symbol.category==='runtime'||symbol.standard)return;
    if(symbol.kind==='unknown'){toast('Цель динамического вызова не определена. Связи за этой границей неизвестны.');return;}
    state.symbol=symbol;state.lazyGraph=null;state.branchTokens.clear();state.branchBusy.clear();state.branchEpoch++;state.graphLimit=600;state.scale=1;state.graph=null;state.graphView=null;state.nodePositions.clear();state.inspectedNode=null;$('#graph-inspector').classList.add('hidden');
    if(record && state.history[state.historyAt]?.id!==symbol.id){state.history=state.history.slice(0,state.historyAt+1);state.history.push(symbol);state.historyAt=state.history.length-1;}
    $('#symbol-title').textContent=symbol.label;$('#graph-source').disabled=!symbol.file;
    if(!graphModal.open)graphModal.showModal();renderHistory();setHash();loadGraph();
  }
  function renderHistory() {
    $('#graph-history').innerHTML=state.history.map((node,index)=>`<button data-history="${index}" class="${index===state.historyAt?'active':''}" ${index===state.historyAt?'aria-current="step"':''} title="${esc(node.label)}">${index+1}. ${esc(node.label)}</button>`).join('<span aria-hidden="true">›</span>');
    $('#history-back').disabled=state.historyAt<=0;$('#history-forward').disabled=state.historyAt>=state.history.length-1;
    $('#graph-history .active')?.scrollIntoView({block:'nearest',inline:'nearest'});
  }
  function goHistory(index) {if(index<0||index>=state.history.length)return;state.historyAt=index;openGraph(state.history[index],false);}
  function graphParams(direction, depth) {
    const params=new URLSearchParams({dir:direction,vendor:state.vendor,external:String(state.external),lazy:'true',compact:'true'});
    if(depth!=null)params.set('depth',String(depth));if(state.category)params.set('category',state.category);
    if(state.symbol?.id.startsWith('type:')&&state.symbol.language)params.set('language',state.symbol.language);
    return params;
  }
  async function expandBranch(id, all=false) {
    if(!state.graphView||state.branchBusy.has(id))return;
    if(!state.lazyGraph){state.graphView.expand(id,all);renderGraph();return;}
    const session=state.lazyGraph,view=state.graphView,generation=state.graphRequest,epoch=state.branchEpoch,token=(state.branchTokens.get(id)||0)+1;
    state.branchTokens.set(id,token);state.branchBusy.set(id,token);
    const current=()=>generation===state.graphRequest&&epoch===state.branchEpoch&&session===state.lazyGraph&&view===state.graphView&&graphModal.open&&state.branchTokens.get(id)===token;
    renderGraph();
    try{
      const result=await session.expand(view,id,all,request=>api(`/api/graph/${encodeURIComponent(request.id)}?${graphParams(request.dir,request.depth)}`),current);
      if(!current()||result.stale)return;
      if(result.revisionChanged){loadGraph();return;}
      state.graph=result.data;state.hiddenVendor=CodefactorGraph.hiddenVendorIndex(result.data);rootMetrics();
    }catch(error){if(current())toast(error.message);}
    finally{if(state.branchBusy.get(id)===token)state.branchBusy.delete(id);if(current())renderGraph();}
  }
  function importGraphDirection(reverse,starts,byId,map,included,edges) {
          const seen=new Set(starts),queue=starts.map(member=>[member,0]);let at=0;
          while(at<queue.length){const [current,depth]=queue[at++];if(state.depth&&depth>=Number(state.depth))continue;for(const edge of map.get(current)||[]){const next=reverse?edge.from:edge.to;const n=byId.get(next);if(!state.external&&n?.kind==='external')continue;if(state.category&&n?.kind==='external'&&n.category!==state.category)continue;edges.add(edge);included.add(next);if(!seen.has(next)){seen.add(next);queue.push([next,depth+1]);}}}
  }
  function importedGraphData(id) {
        // Imported graph navigation uses the same directed traversal as server queries.
        let source=state.imported;
        if(id.startsWith('type:')&&!source.nodes.some(n=>n.id===id)) {
          const members=source.nodes.filter(n=>n.parent_type===state.symbol.label);
          const memberEdge=(from,to)=>({from,to,confidence:'resolved',relation:'member'}),links=[];if(state.direction!=='callers')links.push(...members.map(n=>memberEdge(id,n.id)));if(state.direction!=='callees')links.push(...members.map(n=>memberEdge(n.id,id)));source={...source,nodes:[...source.nodes,{id,label:state.symbol.label,kind:'class'}],edges:[...source.edges,...links]};
        }
        const classMembers=id.startsWith('type:')?source.nodes.filter(n=>n.parent_type===state.symbol.label).map(n=>n.id):null;
        const starts=classMembers||[id], outgoing=new Map(), incoming=new Map();
        const byId=new Map(source.nodes.map(n=>[n.id,n]));
        for(const edge of source.edges.filter(edge=>edge.relation!=='member')){if(!outgoing.has(edge.from))outgoing.set(edge.from,[]);if(!incoming.has(edge.to))incoming.set(edge.to,[]);outgoing.get(edge.from).push(edge);incoming.get(edge.to).push(edge);}
        const included=new Set([id,...starts]), edges=new Set();
        if(classMembers){const memberEdge=(from,to)=>({from,to,confidence:'resolved',relation:'member'});if(state.direction!=='callers')classMembers.forEach(member=>edges.add(memberEdge(id,member)));if(state.direction!=='callees')classMembers.forEach(member=>edges.add(memberEdge(member,id)));}
        for(const reverse of state.direction==='both'?[true,false]:[state.direction==='callers']){
          importGraphDirection(reverse,starts,byId,reverse?incoming:outgoing,included,edges);
        }
        return {nodes:source.nodes.filter(n=>included.has(n.id)),edges:[...edges],root:id,roots:starts,coverage:source.coverage,loaded_coverage:source.loaded_coverage,requested_coverage:source.requested_coverage,coverage_scope:source.coverage_scope,coverage_complete:source.coverage_complete,coverage_unparsed_vendor_files:source.coverage_unparsed_vendor_files,coverage_unparsed_vendor_packages:source.coverage_unparsed_vendor_packages,coverage_direction:source.coverage_direction,coverage_depth:source.coverage_depth,coverage_revision:source.coverage_revision,coverage_filters:source.coverage_filters,coverage_vendor_view:source.coverage_vendor_view,vendor_view:source.vendor_view,revision:source.revision,partial:source.partial,lazy:source.lazy,requested_depth:source.requested_depth,loaded_depth:source.loaded_depth,imported_snapshot:true};
  }
  async function loadGraph() {
    if(!graphModal.open||!state.symbol)return;
    const request=++state.graphRequest,id=state.symbol.id;$('#graph').innerHTML='<div class="empty">Строим связи выбранной функции…</div>';
    try {
      let data;
      if(state.imported) {
        data=importedGraphData(id);
      } else {
        const p=graphParams(state.direction,1);
        if(state.depth)p.set('coverage_depth',state.depth);else p.set('coverage_unbounded','true');
        const key=`${state.revision}:${id}:${p}`,cached=graphResponses.get(key);data=cached?structuredClone(cached):await api(`/api/graph/${encodeURIComponent(id)}?${p}`);if(!cached){if(graphResponses.size>=8)graphResponses.delete(graphResponses.keys().next().value);graphResponses.set(key,structuredClone(data));}
      }
      if(request!==state.graphRequest||id!==state.symbol.id||!graphModal.open)return;
      if(id.startsWith('type:')&&!data.nodes.some(n=>n.id===id)){
        const members=data.roots||[];data.nodes.push({id,label:state.symbol.label,kind:'class',name:state.symbol.label,member_count:members.length});
        const memberEdge=(from,to)=>({from,to,confidence:'resolved',guard:[],in_loop:[],relation:'member'});if(state.direction!=='callers')data.edges.push(...members.map(member=>memberEdge(id,member)));if(state.direction!=='callees')data.edges.push(...members.map(member=>memberEdge(member,id)));data.root=id;
      }
      // Recursive/back edges arrive separately from the server traversal. Keep them visible.
      data.edges=[...new Map([...data.edges,...(data.cycles||[])].map(edge=>[JSON.stringify(edge),edge])).values()];
      data=CodefactorGraph.projectData(data);state.lazyGraph=state.imported?null:new CodefactorGraph.LazyGraph(data,{direction:state.direction,depth:state.depth||null,vendor:state.vendor});if(state.lazyGraph)data=state.lazyGraph.data;state.graph=data;state.hiddenVendor=CodefactorGraph.hiddenVendorIndex(data);state.graphView=new CodefactorGraph.View(data,state.direction);const root=data.nodes.find(n=>String(n.id)===String(data.root||data.roots?.[0]||id));if(root){state.symbol={...state.symbol,...root};$('#symbol-title').textContent=root.label||id;$('#graph-source').disabled=!root.file;}
      const cov=data.coverage||{};$('#coverage').textContent=`${num(data.nodes.length)} узлов · ${num(data.edges.length)} связей · ${num(cov.files?.length||new Set(data.nodes.map(n=>n.file).filter(Boolean)).size)} файлов${state.imported?' · импортированный граф':''}`;
      rootMetrics();renderGraph(true);
    } catch(error) {if(request===state.graphRequest)$('#graph').innerHTML=`<div class="empty error">${esc(error.message)}<br/><button id="retry-graph">Повторить</button></div>`;}
  }
  function visibleGuards(edge) {return (edge.guard||edge.guards||[]).filter(g=>String(g.normalized||g.raw||'').trim());}
  function hasEdgeCondition(edge) {return visibleGuards(edge).length>0;}
  function edgeGuard(edge) {return visibleGuards(edge).map(g=>String((state.raw?g.raw:g.normalized)||g.raw||g.normalized||'').trim()).filter(Boolean).join(' ∧ ');}
  function shortPath(value,max=40){const s=String(value||'');return s.length>max?s.slice(0,10)+'…'+s.slice(-(max-11)):s;}
  function short(value,max=38){const s=String(value||'');return s.length>max?s.slice(0,max-1)+'…':s;}
  function layoutGraph(data) {
    if(!state.graphView)state.graphView=new CodefactorGraph.View(data,state.direction);
    return state.graphView.layout({collapseSimple:state.collapseSimple,groupClasses:state.groupClasses,positions:state.nodePositions,limit:state.graphLimit});
  }
  function algorithmicLabel(node) {
    const e=node.algorithmic_complexity;if(!e)return '';
    return ['tight','upper_bound','heuristic'].includes(e.status)?e.formula||'':'';
  }
  function algorithmicBadge(node) {
    const e=node.algorithmic_complexity;if(!algorithmicLabel(node))return '';
    const label=({tight:'Точный класс в модели',upper_bound:'Верхняя граница',heuristic:'Эвристическая оценка',unknown:'Стоимость неизвестна',unsupported:'Язык пока не поддерживается',disabled:'Анализ отключён'})[e.status]||e.status;
    return `<span class="algorithmic-badge" title="${esc(label)}">Время <b>${esc(algorithmicLabel(node))}</b></span>`;
  }
  function algorithmicDetails(node,explainUnknown=false) {
    const e=node.algorithmic_complexity;if(!e||(!algorithmicLabel(node)&&!explainUnknown))return '';
    const status=({tight:'Точный класс в модели',upper_bound:'Верхняя граница',heuristic:'Эвристическая оценка',unknown:'Стоимость неизвестна',unsupported:'Язык пока не поддерживается',disabled:'Анализ отключён'})[e.status]||e.status;
    const list=(items,render)=>items?.length?`<ul>${items.map(render).join('')}</ul>`:'';
    return `<section class="algorithmic-details"><h4>Алгоритмическая сложность · время</h4><p><b>${esc(algorithmicLabel(node)||status)}</b>${algorithmicLabel(node)?' · '+esc(status):''}</p>${e.normalized?`<p>Общий масштаб: ${esc(e.normalized)}</p>`:''}${e.local_formula?`<p>Локальная стоимость без выполнения функций проекта: ${esc(e.local_formula)}</p>`:''}${e.status==='unknown'&&e.known_formula?`<p>Известная часть: ${esc(e.known_formula)}. Полная стоимость не установлена.</p>`:''}${list(e.variables,v=>`<li><code>${esc(v.symbol)}</code>: ${esc(v.definition)}</li>`)}${list(e.unknown_reasons,r=>`<li>Неизвестно: ${esc(r)}</li>`)}${list(e.assumptions,a=>`<li>Предположение: ${esc(a)}</li>`)}${e.explanations?.length?`<details><summary>Как получена оценка</summary>${list(e.explanations,x=>`<li><button data-graph-source-file="${esc(x.range?.file||node.file||'')}" data-jump-line="${Number(x.range?.start_line||1)}">${esc(x.range?.file||node.file||'')}:${Number(x.range?.start_line||1)}</button> ${esc(x.detail)}</li>`)}</details>`:''}<p class="cost-model">${esc(e.model)}</p></section>`;
  }
  function nodeMetrics(node,compact=false) {
    const m=node.metrics;if(!m)return algorithmicBadge(node);
    const metrics=[['Параметры',m.parameters,'Параметры с учётом настроек подсчёта'],['CYC',m.complexity,'Цикломатическая сложность: независимые пути выполнения'],['Входы',m.fan_in,'Число входящих рёбер вызова'],['Выходы',m.fan_out,'Число исходящих рёбер вызова'],['Вложенность',m.nesting_depth,'Максимальная глубина управляющих конструкций']];
    return metrics.slice(0,compact?4:5).map(([label,value,title])=>`<span title="${esc(title)}">${label} <b>${num(value)}</b></span>`).join('')+(m.errors?`<span class="metric-error">Ошибки <b>${num(m.errors)}</b></span>`:'')+(m.warnings?`<span class="metric-warn">Замечания <b>${num(m.warnings)}</b></span>`:'')+algorithmicBadge(node);
  }
  function rootMetrics() {
    const node=state.symbol;$('#symbol-metrics').innerHTML=(node.parent_type?`<span class="owner-label" title="${esc(node.parent_type)}">Класс: ${esc(node.parent_type)}</span>`:'')+nodeMetrics(node,true);
  }
  function nodeHeight(id) {return state.graphLayout?.heights.get(String(id))||170;}
  function edgePath(bundle,positions) {
    const a=positions.get(bundle.from),b=positions.get(bundle.to);if(!a||!b)return '';
    const reverse=b.x<a.x,x1=a.x+(reverse?0:320),y1=a.y+nodeHeight(bundle.from)/2,x2=b.x+(reverse?320:0),y2=b.y+nodeHeight(bundle.to)/2,bend=Math.max(100,Math.abs(x2-x1)*.5),sign=reverse?-1:1;
    return bundle.from===bundle.to?`M${a.x+275},${a.y} C${a.x+360},${a.y-95} ${a.x-15},${a.y-95} ${a.x+40},${a.y}`:`M${x1},${y1} C${x1+sign*bend},${y1} ${x2-sign*bend},${y2} ${x2},${y2}`;
  }
  function edgeCaption(bundle) {
    if(!bundle.calls.some(hasEdgeCondition))return '';
    const conditions=[...new Set(bundle.calls.map(edgeGuard).filter(Boolean))];
    return short(conditions.join(' · '),70);
  }
  function edgePoint(bundle,positions) {
    const a=positions.get(bundle.from),b=positions.get(bundle.to);return {x:(a.x+b.x+320)/2,y:(a.y+nodeHeight(bundle.from)/2+b.y+nodeHeight(bundle.to)/2)/2-6};
  }
  function drawGraphPositions() {
    const layout=state.graphLayout;if(!layout)return;
    for(const element of document.querySelectorAll('#graph [data-graph-node]')){const p=layout.positions.get(element.dataset.graphNode);if(p)element.setAttribute('transform',`translate(${p.x},${p.y})`);}
    for(const element of document.querySelectorAll('#graph [data-class-frame]')) {
      const frame=layout.frames.find(f=>f.id===element.dataset.classFrame),b=CodefactorGraph.frameBounds(frame,layout.positions),rect=$('.class-frame-box',element);
      for(const key of ['x','y','width','height'])rect.setAttribute(key,b[key]);$('.class-frame-heading',element).setAttribute('transform',`translate(${b.x+14},${b.y+24})`);
    }
    for(const element of document.querySelectorAll('#graph [data-edge-index]')) {
      const bundle=layout.bundles[Number(element.dataset.edgeIndex)];for(const path of element.querySelectorAll('path'))path.setAttribute('d',edgePath(bundle,layout.positions));
      const point=edgePoint(bundle,layout.positions);$('.edge-chip',element)?.setAttribute('transform',`translate(${point.x},${point.y})`);
    }
  }
  function svgAction(x,y,width,label,attributes,title) {
    return `<g class="node-action" ${attributes} transform="translate(${x},${y})" tabindex="0" role="button" aria-label="${esc(title)}"><title>${esc(title)}</title><rect width="${width}" height="25" rx="5"/><text x="${width/2}" y="17" text-anchor="middle">${esc(label)}</text></g>`;
  }
  function svgTextLines(css,lines,y,lineHeight) {
    return `<text class="${css}">${lines.map((line,i)=>`<tspan x="14" y="${y+i*lineHeight}">${esc(line)}</tspan>`).join('')}</text>`;
  }
  function renderCoverage() {
    const data=state.graph,cov=data.coverage||{},hidden=data.hidden_vendor_edges?.length||0,scope=data.imported_snapshot?'Охват исходной импортированной выгрузки'+(data.coverage_scope==='loaded'?' (загруженный подграф)':''):data.coverage_scope==='requested'?'Полный охват запроса по разобранному графу':data.lazy?'Охват загруженного подграфа':'Охват выборки';
    const names=[['Функции проекта',cov.functions],['Классы проекта',cov.classes],['Файлы',cov.files],['Модули',cov.modules],['Функции зависимостей',cov.vendor_functions],['Классы зависимостей',cov.vendor_classes],['Внешние операции',cov.external],['Неразрешённые цели',cov.unresolved],['Точки входа',cov.entry_points],['Листья',cov.leaves]];
    $('#coverage-details').innerHTML=`<p><b>${scope}</b>${data.imported_snapshot?' · снимок; серверная загрузка продолжений недоступна':''}${data.lazy?` · загружено до ${num(data.loaded_depth)} вызовов · заданная глубина ${data.requested_depth??'∞'}${data.partial?' · есть незагруженные продолжения':' · продолжения в заданных пределах загружены'}`:''}</p>`+`<p>Загружено для просмотра: ${num(data.coverage_nodes?.length||data.nodes.length)} узлов · ${num(data.coverage_edges?.length||data.edges.length)} связей. ${data.coverage_scope==='requested'?`Охват рассчитан сервером независимо от раскрытых ветвей · направление ${esc(data.coverage_direction)} · глубина ${data.coverage_depth??'∞'} · ревизия ${num(data.coverage_revision)}.`:''}</p>`+names.map(([name,ids])=>`<details><summary>${esc(name)}: ${num(ids?.length)}</summary><div>${(ids||[]).slice(0,100).map(id=>`<div>${esc(id)}</div>`).join('')}${ids?.length>100?'<p>Показаны первые 100 сущностей.</p>':''}</div></details>`).join('')+`<p>${data.coverage_complete===false?`Не разобрано файлов зависимостей: ${num(data.coverage_unparsed_vendor_files)}. Охват полон только для разобранного графа; вызовы из неразобранных файлов неизвестны.`:cov.vendor_boundary?'Граница зависимостей: часть кода не разобрана или скрыта. Обратные вызовы из неразобранного кода неизвестны.':'Граница зависимостей в этой выборке не обнаружена.'}</p><p>Неразрешённые цели: ${num(cov.unresolved?.length)}. Продолжение таких цепочек неизвестно. Скрытых рёбер зависимостей: ${num(hidden)}.</p>${data.requested_depth||data.depth?`<p>Заданная глубина ограничена ${num(data.requested_depth||data.depth)} вызовами. Методы выбранного класса находятся на нулевом уровне.</p>`:''}`;
    return ` · ${data.lazy?'загруженный подграф'+(data.partial?' · есть незагруженные ветви':''):''}${data.coverage_scope==='requested'?' · полный охват запроса':''} · ${num(cov.functions?.length)} функций / ${num(cov.classes?.length)} классов проекта · vendor ${num(cov.vendor_functions?.length)} функций · ${num(cov.files?.length)} файлов${cov.vendor_boundary?' · граница vendor':''}${cov.unresolved?.length?' · неизвестных '+num(cov.unresolved.length):''}`;
  }
  function renderChainTree() {
    const data=state.graph,signs=state.direction==='both'?['callers','callees']:[state.direction];
    $('#graph').classList.add('chain-mode');
    $('#graph').innerHTML=signs.map(direction=>{const result=CodefactorGraph.chainRows(data,direction,state.graphLimit);return `<section class="chain-section"><h3>${direction==='callers'?'Кто вызывает':'Кого вызывает'}</h3>${result.rows.map(row=>{const guards=[...new Set(row.guards.map(g=>String((state.raw?g.raw:g.normalized)||g.raw||g.normalized||'')))];return `<div class="chain-row" style="--depth:${Math.min(20,row.depth)}"><div><span>${row.depth?'↳':'●'}</span> <button data-chain-go="${esc(row.id)}">${esc(row.node.behavior==='module_initializer'?'Инициализация модуля':row.node.label||row.id)}</button>${row.cycle?'<span class="tree-warning">Цикл · обход остановлен</span>':''}${row.node.kind==='unknown'?'<span class="tree-warning">Граница: цель неизвестна</span>':''}${row.node.vendor?'<span class="tree-vendor">vendor</span>':''}${!row.cycle&&state.lazyGraph?.remote(state.graphView,row.id)?`<span class="muted">Незагруженная ветвь</span><button data-branch-expand="${esc(row.id)}">+1</button><button data-branch-all="${esc(row.id)}">Всю ветку</button>`:''}</div>${row.edge?.callsite?.file?`<button data-graph-source-file="${esc(row.edge.callsite.file)}" data-jump-line="${row.edge.callsite.start_line||1}">${esc(row.edge.callsite.file)}:${row.edge.callsite.start_line||1}</button>`:''}${guards.length?`<div class="chain-guards" title="Конъюнкция условий мест вызова вдоль показанного пути">Накопленные условия: ${guards.map(esc).join(' ∧ ')}</div>`:''}</div>`;}).join('')}${result.truncated?'<button id="chain-more">Показать ещё 600 строк цепочек</button>':''}</section>`;}).join('');
    $('#coverage').textContent=`Дерево путей${renderCoverage()}`;
    $('#neighbor-controls').innerHTML='';
  }
  function inspectHiddenVendor(id) {
    const item=state.hiddenVendor?.get(id);if(!item)return;
    $('#graph-inspector-body').innerHTML=`<h3>Скрытые рёбра зависимостей: ${num(item.edges.length)}</h3><p>${[...item.packages].map(esc).join(', ')||'Пакеты зависимостей'}</p><button id="show-hidden-vendor">Временно показать зависимости</button>${item.edges.slice(0,100).map(edge=>`<section class="call-detail"><p>${esc(edge.from)} → ${esc(edge.to)}</p>${edge.callsite?.file?`<button data-graph-source-file="${esc(edge.callsite.file)}" data-jump-line="${edge.callsite.start_line||1}">${esc(edge.callsite.file)}:${edge.callsite.start_line||1}</button>`:''}<pre>${esc(edgeGuard(edge))}</pre></section>`).join('')}${item.edges.length>100?'<p>Показаны первые 100 мест вызова.</p>':''}`;
    $('#graph-inspector').classList.remove('hidden');
  }
  function renderGraphFrame(frame,positions) {
    const b=CodefactorGraph.frameBounds(frame,positions);return `<g class="class-frame" data-class-frame="${esc(frame.id)}"><rect class="class-frame-box" x="${b.x}" y="${b.y}" width="${b.width}" height="${b.height}" rx="14"/><g class="class-frame-heading" transform="translate(${b.x+14},${b.y+24})"><text data-class-info="${esc(frame.id)}" tabindex="0" role="button">${(frame.headerLines||[frame.label]).map((line,i)=>`<tspan x="0" y="${i*14}">${esc(line)}</tspan>`).join('')}</text>${svgAction(Math.min(b.width-55,335),-17,28,'↗',frame.owner?`data-class-go="${esc(frame.owner)}" data-class-language="${esc(frame.language||'')}"`:`data-graph-source-file="${esc(frame.file)}" data-jump-line="1"`,frame.owner?'Открыть граф методов класса '+frame.owner:'Открыть файл '+frame.file)}</g></g>`;
  }
  function renderGraphEdge(bundle,index,positions) {

      const confidences=new Set(bundle.calls.map(e=>e.confidence||'dynamic')),confidence=confidences.has('dynamic')?'dynamic':confidences.has('ambiguous')?'ambiguous':'resolved',conditional=bundle.calls.some(hasEdgeCondition),caption=edgeCaption(bundle),point=edgePoint(bundle,positions),chipWidth=Math.max(110,caption.length*6+16);
      return `<g class="graph-edge ${confidence} ${conditional?'conditional-edge':'passive-edge'}" data-edge-index="${index}" ${conditional?`tabindex="0" role="button" aria-label="Показать условия связи: ${esc(caption)}"`:''}><path d="${edgePath(bundle,positions)}" marker-end="url(#arrow)"/>${conditional?`<path class="edge-hit" d="${edgePath(bundle,positions)}"/><g class="edge-chip" transform="translate(${point.x},${point.y})"><rect x="${-chipWidth/2}" y="-15" width="${chipWidth}" height="22" rx="6"/><text text-anchor="middle">${esc(caption)}</text></g>`:''}</g>`;

  }
  function graphNodeOwner(node) {
    if(node.behavior==='module_initializer')return 'Модуль: '+shortPath(node.file||'',34);
    if(node.kind==='fold'&&['callers','callees'].includes(node.category))return 'Прямые связи выбранного метода';
    if(node.kind==='class')return 'Класс · методов '+num(node.member_count);
    if(node.parent_type)return 'Класс: '+node.parent_type;
    if(node.kind==='unknown')return 'Тип получателя не установлен';
    if(node.kind==='external')return node.category==='runtime'?'Стандартная функция языка':'Внешняя операция · '+(node.category||'');
    if(node.kind==='vendor_package')return 'Пакет зависимости';
    return 'Свободная функция';
  }
  function graphNodeActions(node,root,label,offset,hidden,remote,busy) {
      let actions='';const hiddenVendor=state.hiddenVendor?.get(String(node.id));if(hiddenVendor)actions+=svgAction(14,124+offset,176,'Vendor: '+num(hiddenVendor.edges.length)+' скрытых рёбер',`data-vendor-boundary="${esc(node.id)}"`,'Показать пакеты и места скрытых вызовов');
      if(node.kind==='fold'&&['callers','callees'].includes(node.category))actions=svgAction(191,135+offset,61,'+20',`data-neighbors-expand="${esc(node.id)}"`,'Показать следующие 20 методов')+svgAction(258,135+offset,49,'Всё',`data-neighbors-all="${esc(node.id)}"`,'Показать все методы направления');
      else if(node.kind==='fold')actions=svgAction(193,135+offset,114,'Показать '+num(node.members.length),`data-group-expand="${esc(node.id)}"`,'Раскрыть функции группы');
      else if(hidden.nodes||remote)actions+=svgAction(206,135+offset,43,busy?'…':'+1',`data-branch-expand="${esc(node.id)}"`,'Раскрыть следующий уровень')+svgAction(255,135+offset,52,busy?'…':'⇉ всё',`data-branch-all="${esc(node.id)}"`,'Раскрыть всю дальнейшую ветку');
      if(node.kind!=='fold'&&state.graphView.expanded(String(node.id)))actions+=svgAction(125,135+offset,73,'Свернуть',`data-branch-close="${esc(node.id)}"`,'Свернуть раскрытую ветку');
      if(node.kind!=='fold'&&node.kind!=='unknown'&&node.id!==root)actions+=svgAction(284,9,25,'↗',`data-graph-go="${esc(node.id)}"`,'Открыть граф '+label);
      return actions;
  }
  function graphNodeStats(node,boundary) {
    const m=node.metrics||{};
    if(node.kind==='fold')return `${num(node.members.length)} функций скрыто · ${node.category==='accessors'?'чтение / запись поля':node.category==='callers'?'вызывают выбранный метод':node.category==='callees'?'вызываются из выбранного метода':'до порога длины без вызовов'}`;
    if(node.behavior==='module_initializer')return `${num(m.callsites)} мест вызова · ← ${num(m.fan_in)} · → ${num(m.fan_out)}`;
    if(node.metrics)return `P ${num(m.parameters)} · CYC ${num(m.complexity)} · ← ${num(m.fan_in)} · → ${num(m.fan_out)}${m.errors?' · ! '+num(m.errors):''}`;
    if(node.kind==='unknown')return boundary?'Имя метода известно · код вне каталога':node.parent_type?'Класс известен · реализация не найдена':'Тип получателя не установлен';
    if(node.kind==='class')return 'Прямые связи методов класса';
    return 'См. сведения об операции';
  }
  function graphNodeContinuation(node,boundary,remote,hidden,behavior) {return remote?'Есть незагруженное продолжение':hidden.nodes?`Далее ${num(hidden.nodes)}${state.graphView.expanded(String(node.id))?'':' · ветвей '+num(hidden.branches)}`:node.kind==='fold'?node.label:behavior||(boundary?'Продолжение вне каталога':'Продолжения нет');}
  function graphNodeHeading(node,card,offset,owner) {return (card.className?`<text class="node-field-label" x="14" y="20">КЛАСС</text>${svgTextLines('node-owner node-class',card.classLines,36,14)}`:'')+(card.functionLines.length?`<text class="node-field-label" x="14" y="${card.nameY-18}">${node.kind==='fold'?'ГРУППА':'ФУНКЦИЯ'}</text>${svgTextLines('node-name',card.functionLines,card.nameY,18)}`:'')+(!card.className?`<text class="node-owner" x="14" y="${63+offset}">${esc(owner)}</text>`:'');}
  function graphNodeCost(node,offset) {return algorithmicLabel(node)?`<text class="node-cost" x="14" y="${119+offset}">${esc(short((node.algorithmic_complexity?.status==='heuristic'?'Предположение: ':node.algorithmic_complexity?.status==='upper_bound'?'Граница: ':'')+algorithmicLabel(node),43))}</text>`:'';}
  function graphNodeClass(node,root,boundary) {const cls=node.kind==='fold'?'fold':node.id===root?'root':node.vendor?'vendor':node.kind||'';const self=node.id!==root&&node.parent_type&&node.parent_type===(state.symbol.parent_type||(state.symbol.kind==='class'?state.symbol.label:null))&&(!state.symbol.language||node.language===state.symbol.language);return cls+(boundary?' boundary':'')+(self?' self-method':'')+(state.inspectedNode===node.id?' inspected':'');}
  function renderGraphNode(node,root,positions) {

      const p=positions.get(String(node.id)),label=String(node.behavior==='module_initializer'?'Инициализация модуля':node.label||node.id),card=CodefactorGraph.nodeCard(node),offset=card.offset;
      const boundary=node.unresolved?.reason==='type_not_analyzed';
      const owner=graphNodeOwner(node);
      const hidden=node.kind==='fold'?{nodes:0,branches:0}:state.graphView.hidden(String(node.id)),remote=node.kind!=='fold'&&state.lazyGraph?.remote(state.graphView,String(node.id)),busy=state.branchBusy.has(String(node.id));
      const behavior=({getter:'Геттер по структуре',setter:'Сеттер по структуре',simple_leaf:'Короткая · без вызовов',module_initializer:'Инициализация модуля'})[node.behavior]||'';
      const header=graphNodeHeading(node,card,offset,owner);
      const actions=graphNodeActions(node,root,label,offset,hidden,remote,busy);
      return `<g class="graph-node ${esc(graphNodeClass(node,root,boundary))}" data-graph-node="${esc(node.id)}" transform="translate(${p.x},${p.y})" tabindex="0" role="button" aria-label="Сведения о ${esc(label)}"><title>${esc(label)}\n${esc(owner)}\nПеретаскивание — перемещение, ↗ — переход</title><rect class="node-box" width="320" height="${card.height}" rx="10"/>${header}<text class="node-stats" x="14" y="${82+offset}">${graphNodeStats(node,boundary)}</text><text class="node-subtitle" x="14" y="${100+offset}">${esc(shortPath(node.file||node.category||'',43))}${node.line?':'+node.line:''}</text>${graphNodeCost(node,offset)}<text class="node-hidden" x="14" y="${151+offset}">${esc(graphNodeContinuation(node,boundary,remote,hidden,behavior))}</text>${actions}</g>`;

  }
  function bindGraphInteractions(svg,layout,width,height) {
    const {positions,frames}=layout;let drag=null;
    svg.addEventListener('pointerdown',event=>{
      if(event.button!==0||event.target.closest('.node-action,[data-class-info]'))return;
      const node=event.target.closest('[data-graph-node]'),frame=event.target.closest('[data-class-frame]');if(event.target.closest('.graph-edge'))return;
      const ids=node?[node.dataset.graphNode]:frame?frames.find(f=>f.id===frame.dataset.classFrame)?.members:null;
      drag={x:event.clientX,y:event.clientY,pan:[...state.pan],ids,points:ids?new Map(ids.map(id=>[id,{...positions.get(id)}])):null,moved:false};svg.setPointerCapture?.(event.pointerId);
    });
    svg.addEventListener('pointermove',event=>{
      if(!drag)return;const rect=svg.getBoundingClientRect(),dx=(event.clientX-drag.x)*width/(rect.width||width),dy=(event.clientY-drag.y)*height/(rect.height||height);
      if(!drag.moved&&Math.hypot(dx,dy)<4)return;drag.moved=true;
      if(drag.ids){for(const id of drag.ids){const p=drag.points.get(id),next={x:p.x+dx/state.scale,y:p.y+dy/state.scale};positions.set(id,next);state.nodePositions.set(id,next);}drawGraphPositions();}
      else{state.pan=[drag.pan[0]+dx,drag.pan[1]+dy];applyTransform();}
      $('#edge-tooltip').classList.add('hidden');
    });
    const endDrag=()=>{if(drag?.moved)state.suppressGraphClickUntil=Date.now()+300;drag=null;};svg.addEventListener('pointerup',endDrag);svg.addEventListener('pointercancel',endDrag);svg.addEventListener('lostpointercapture',endDrag);
    svg.addEventListener('wheel',event=>{event.preventDefault();const rect=svg.getBoundingClientRect();zoom(state.scale*Math.exp(-event.deltaY*.0015),[(event.clientX-rect.left)*width/(rect.width||width),(event.clientY-rect.top)*height/(rect.height||height)]);},{passive:false});
    svg.addEventListener('pointerover',event=>{if(drag)return;const edge=event.target.closest('.conditional-edge[data-edge-index]');if(edge)showEdgeTooltip(layout.bundles[Number(edge.dataset.edgeIndex)],event);});
    svg.addEventListener('pointerout',event=>{if(event.target.closest('[data-edge-index]')&&!event.relatedTarget?.closest?.('[data-edge-index]'))$('#edge-tooltip').classList.add('hidden');});
    svg.addEventListener('keydown',event=>{if(['Enter',' '].includes(event.key)&&event.target.closest('[role=button]')){event.preventDefault();event.target.dispatchEvent(new MouseEvent('click',{bubbles:true}));}});
  }
  function renderGraph(center=false) {
    const host=$('#graph'),data=state.graph;if(!data)return;
    if(!data.nodes?.length){host.innerHTML='<div class="empty">Функция не найдена в графе.</div>';return;}
    if($('#graph-view-mode').value==='tree'){renderChainTree();return;}host.classList.remove('chain-mode');
    const layout=layoutGraph(data),{root,nodes,bundles,positions,frames}=layout,width=host.clientWidth||1000,height=host.clientHeight||500;state.graphLayout=layout;
    if(center){const p=positions.get(root)||{x:0,y:0};state.pan=[width/2-(p.x+160)*state.scale,height/2-(p.y+nodeHeight(root)/2)*state.scale];}
    const framesSvg=frames.map(frame=>renderGraphFrame(frame,positions)).join('');
    const edgeSvg=bundles.map((bundle,index)=>renderGraphEdge(bundle,index,positions)).join('');
    const nodeSvg=nodes.map(node=>renderGraphNode(node,root,positions)).join('');
    host.innerHTML=`<svg class="graph-canvas" viewBox="0 0 ${width} ${height}" aria-label="Граф функции ${esc(state.symbol.label)}"><defs><marker id="arrow" markerWidth="9" markerHeight="7" refX="8" refY="3.5" orient="auto"><path d="M0,0 L9,3.5 L0,7 Z" fill="#8a9dbd"/></marker></defs><g class="graph-world">${framesSvg}${edgeSvg}${nodeSvg}</g></svg>${layout.remaining?`<div class="graph-limit">Ещё ${num(layout.remaining)} раскрытых элементов.<button id="graph-more">Показать ещё 600</button></div>`:''}`;
    $('#coverage').textContent=`На холсте ${num(nodes.length)} элементов · раскрыто ${num(layout.revealed)} из ${num(data.nodes.length)} узлов${renderCoverage()}${state.imported?' · импорт':''}`;
    $('#neighbor-controls').innerHTML=[...state.graphView.neighborGroups].filter(([key])=>(state.graphView.neighborShown.get(key)||0)>0).map(([key,g])=>`<button data-neighbors-close="${esc(key)}">Свернуть ${g.sign<0?'вызывающие':'вызываемые'} методы</button>`).join('');
    applyTransform();$('#edge-tooltip').classList.add('hidden');
    bindGraphInteractions($('svg',host),layout,width,height);
  }
  function showEdgeTooltip(bundle,event) {
    const tooltip=$('#edge-tooltip'),stage=$('.graph-stage').getBoundingClientRect(),labels=new Map(state.graph.nodes.map(n=>[String(n.id),n.label]));
    const guards=[...new Set(bundle.calls.map(edgeGuard).filter(Boolean))];
    if(!guards.length){tooltip.classList.add('hidden');return;}
    tooltip.innerHTML=`<b>${esc(short(labels.get(bundle.calls[0].from)||bundle.from,55))} → ${esc(short(labels.get(bundle.calls[0].to)||bundle.to,55))}</b><p>${guards.slice(0,4).map(g=>esc(g)).join('<br/>')}</p><small>${num(bundle.calls.length)} мест вызова · нажмите для подробностей и кнопки перехода к коду</small>`;
    tooltip.classList.remove('hidden');tooltip.style.left=`${Math.max(8,Math.min(event.clientX-stage.left+15,stage.width-430))}px`;tooltip.style.top=`${Math.max(8,Math.min(event.clientY-stage.top+15,stage.height-170))}px`;
  }
  function inspectNode(node,limit=50) {
    state.inspectedNodeObject=node;state.inspectedMemberLimit=limit;
    state.inspectedNode=node.id;$('#graph-inspector').classList.remove('hidden');
    if(node.kind==='fold'){
      const neighbors=['callers','callees'].includes(node.category), purpose=neighbors?(node.category==='callers'?'Эти методы вызывают выбранную функцию.':'Эти методы вызываются из выбранной функции.'):'Функции объединены по структуре тела.';
      $('#graph-inspector-body').innerHTML=`<h3>${esc(node.label)}</h3><p>${num(node.members.length)} методов скрыто. ${purpose}</p>${node.members.slice(0,limit).map(n=>`<div class="inspector-member">${esc(n.label)} ${n.kind!=='unknown'?`<button data-graph-go="${esc(n.id)}" title="Открыть граф">↗</button>`:''}</div>`).join('')}${node.members.length>limit?'<button id="more-group-members">Ещё 50 методов</button>':''}${neighbors?`<button data-neighbors-expand="${esc(node.id)}">Показать следующие 20</button><button data-neighbors-all="${esc(node.id)}">Показать все</button>`:`<button data-group-expand="${esc(node.id)}">Показать на холсте</button>`}`;return;
    }
    if(node.details_available&&!node.details_loaded){
      const generation=state.graphRequest,id=node.id,key=`${state.revision}:${id}`;
      const hydrate=details=>{if(generation!==state.graphRequest||state.inspectedNode!==id||!graphModal.open)return;inspectNode({...node,...details,details_loaded:true},limit);};
      if(symbolDetails.has(key)){hydrate(symbolDetails.get(key));return;}
      api(`/api/symbol/${encodeURIComponent(id)}`).then(details=>{if(symbolDetails.size>=20)symbolDetails.delete(symbolDetails.keys().next().value);symbolDetails.set(key,details);hydrate(details);}).catch(()=>{});
    }
    const m=node.metrics||{},behavior=({getter:'Геттер: прямой возврат поля получателя.',setter:'Сеттер: прямая запись параметра в поле получателя.',simple_leaf:'Короткая функция без вызовов: простота по порогу, чистота не доказана.',module_initializer:'Вызовы, выполняемые при загрузке модуля. Метрики объявленных функций сюда не применяются.'})[node.behavior]||'';
    $('#graph-inspector-body').innerHTML=`<h3>${esc(node.behavior==='module_initializer'?'Инициализация модуля':node.label||node.id)}</h3><p>${node.kind==='unknown'?esc(({type_not_analyzed:'Класс и имя метода определены. Это ссылка на метод: код реализации находится за пределами выбранного каталога или в исключённой зависимости. Продолжение цепочки станет доступно при анализе этого кода.',method_not_found:'Тип найден в проекте, но реализация метода не установлена. Возможны наследование, динамический вызов или неполный разбор.',binding_unknown:'Имя связано с локальной переменной, но её тип или вызываемая функция не установлены. Импорт или одноимённый метод другого класса не доказывают цель этого вызова.',receiver_unknown:'Тип получателя не установлен. Поэтому анализатор не может выбрать реализацию метода.',function_not_found:'Функция не найдена среди проанализированных исходников и известных внешних операций.'})[node.unresolved?.reason]||'Анализатор не смог установить цель вызова. Красный пунктир обозначает неопределённость, а не нарушение качества.'):node.kind==='external'?(node.category==='runtime'?'Стандартная функция языка.':'Внешняя операция за границей исходного кода проекта.'):node.kind==='vendor_package'?'Свёрнутый пакет зависимости. Раскрытие запускается отдельной кнопкой.':node.behavior==='module_initializer'?'Выполнение кода при загрузке модуля.':node.parent_type?'Класс: '+esc(node.parent_type):'Принадлежность к классу не установлена; свободная или вложенная функция.'}</p>${behavior?`<p>${esc(behavior)}</p>`:''}${node.simple_threshold?`<p>Порог ${esc(node.language)}: тело ≤ ${node.simple_threshold.body_lines} строк, параметры ≤ ${node.simple_threshold.parameters}, CYC ≤ 1, вложенность 0, вызовов нет.</p>`:''}${node.metrics?`<dl>${[['Строки',m.lines],['Параметры',m.parameters],['Цикломатическая сложность',m.complexity],['Когнитивная сложность',m.cognitive_complexity],['Вложенность',m.nesting_depth],['Входящие рёбра',m.fan_in],['Исходящие рёбра',m.fan_out],['Различные вызывающие',m.callers],['Различные вызываемые',m.callees],['Индекс сопровождаемости',m.maintainability_index],['Ошибки / предупреждения',`${m.errors||0} / ${m.warnings||0}`]].map(([label,value])=>`<dt>${label}</dt><dd>${value==null?'—':typeof value==='number'?num(value):esc(value)}</dd>`).join('')}</dl>`:''}${node.unresolved?`<h4>Места вызова · ${num(node.unresolved.total)}</h4>${(node.unresolved.callsites||[]).map(site=>`<div class="inspector-member"><span>${esc(site.receiver||node.label)} · ${esc(site.file)}:${site.line}</span><button data-graph-source-file="${esc(site.file)}" data-jump-line="${site.line}">Код ↗</button></div>`).join('')}${node.unresolved.total>50?'<p>Показаны первые 50 мест вызова.</p>':''}`:''}<div class="inspector-actions">${node.kind!=='unknown'&&node.id!==state.symbol.id?`<button data-graph-go="${esc(node.id)}">Граф функции ↗</button>`:''}${node.file?`<button data-graph-source-file="${esc(node.file)}" data-jump-line="${Number(node.line||1)}">Исходный код ↗</button>`:''}</div>`;
    $('#graph-inspector-body').insertAdjacentHTML('beforeend',algorithmicDetails(node));
    if(node.kind==='external'&&node.category!=='runtime'){
      const calls=CodefactorGraph.externalCallsites(state.graph,node.id);
      if(calls.length)$('#graph-inspector-body').insertAdjacentHTML('beforeend',`<h4>Места внешних вызовов · ${num(calls.length)}</h4>${calls.slice(0,limit).map(edge=>`<section class="call-detail"><p>${esc(edge.from)}</p>${edge.argument!=null?`<p>Аргумент: <code>${esc(edge.argument)}</code></p>`:''}${edge.callsite?.file?`<button data-graph-source-file="${esc(edge.callsite.file)}" data-jump-line="${Number(edge.callsite.start_line||1)}">${esc(edge.callsite.file)}:${Number(edge.callsite.start_line||1)} ↗</button>`:''}</section>`).join('')}${calls.length>limit?'<button id="more-external-calls">Ещё 50 мест вызова</button>':''}`);
    }
  }
  function inspectEdge(bundle,limit=50) {
    state.inspectedBundle=bundle;state.inspectedCalls=limit;$('#graph-inspector').classList.remove('hidden');$('#edge-tooltip').classList.add('hidden');
    const names=new Map(state.graph.nodes.map(n=>[String(n.id),n.label]));
    $('#graph-inspector-body').innerHTML=`<h3>Связь вызова</h3><p>${esc(names.get(String(bundle.calls[0].from))||bundle.from)} → ${esc(names.get(String(bundle.calls[0].to))||bundle.to)}</p><p>${num(bundle.calls.length)} мест вызова. Условия относятся к каждому конкретному месту.</p>${bundle.calls.slice(0,limit).map(edge=>{const guards=visibleGuards(edge),site=edge.callsite||{},loops=edge.in_loop||[];return `<section class="call-detail"><span class="confidence-label">${({resolved:'Цель найдена',ambiguous:'Несколько возможных целей',dynamic:'Динамический вызов'})[edge.confidence]||'Связь методов класса'}</span>${edge.argument!=null?`<p>Аргумент: <code>${esc(edge.argument)}</code></p>`:''}${guards.length?guards.map(g=>`<div class="guard-detail"><b>${esc(g.branch||g.kind||'Условие')}</b><pre>${esc(g.normalized||g.raw)}</pre>${g.raw&&g.raw!==g.normalized?`<details><summary>Исходное условие</summary><pre>${esc(g.raw)}</pre></details>`:''}</div>`).join(''):''}${loops.length?`<p>В цикле: ${loops.map(l=>esc(l.header||'цикл')).join(', ')}</p>`:''}${site.file?`<p>${esc(site.file)}:${site.start_line||1}</p><button data-graph-source-file="${esc(site.file)}" data-jump-line="${Number(site.start_line||1)}">Перейти к месту вызова ↗</button>`:''}</section>`;}).join('')}${bundle.calls.length>limit?`<button id="more-call-details">Ещё ${Math.min(50,bundle.calls.length-limit)} мест вызова</button>`:''}`;
  }
  function inspectClass(frame,limit=50) {
    state.inspectedClass=frame;state.inspectedMemberLimit=limit;
    $('#graph-inspector').classList.remove('hidden');
    const units=state.graphLayout.nodes.filter(n=>frame.members.includes(n.id));
    const nodes=units.flatMap(n=>n.members||[n]);
    $('#graph-inspector-body').innerHTML=`<h3>${esc(frame.label)}</h3><p>${num(nodes.length)} функций файла ${esc(frame.file)} на уровне ${frame.level}. Рамка появляется только для нескольких видимых элементов этого файла на одном уровне.</p>${frame.owner?`<button data-class-go="${esc(frame.owner)}" data-class-language="${esc(frame.language||'')}">Граф класса ↗</button>`:`<button data-graph-source-file="${esc(frame.file)}" data-jump-line="1">Открыть файл ↗</button>`}${nodes.slice(0,limit).map(n=>`<div class="inspector-member">${esc(n.name||n.label)} <button data-graph-go="${esc(n.id)}">↗</button></div>`).join('')}${nodes.length>limit?'<button id="more-class-members">Ещё 50 функций</button>':''}`;
  }
  function applyTransform() {$('.graph-world')?.setAttribute('transform',`translate(${state.pan[0]},${state.pan[1]}) scale(${state.scale})`);$('#zoom-reset').textContent=`${Math.round(state.scale*100)}%`;}
  function zoom(scale,point) {const host=$('#graph');point=point||[host.clientWidth/2,host.clientHeight/2];const next=Math.max(.15,Math.min(8,scale)),ratio=next/state.scale;state.pan=[point[0]-(point[0]-state.pan[0])*ratio,point[1]-(point[1]-state.pan[1])*ratio];state.scale=next;applyTransform();}
  function centerGraph() {const host=$('#graph'),p=state.graphLayout?.positions.get(state.graphLayout.root)||{x:0,y:0};state.pan=[host.clientWidth/2-(p.x+160)*state.scale,host.clientHeight/2-(p.y+nodeHeight(state.graphLayout?.root)/2)*state.scale];applyTransform();}
  function violationCard(v) {
    if(v.architecture)return architectureCard(v);
    if(v.module_cycle)return moduleCycleCard(v);
    const h=help(v.rule),d=v.algorithmic_complexity;
    const value=d?esc(d.estimate?.formula||'Оценка недоступна'):num(v.value),bound=d?esc(d.threshold||'Полнота анализа'):num(v.limit);
    return `<article class="violation-row"><div><span class="severity severity-${esc(v.severity)}">${v.severity==='error'?'ОШИБКА':'ПРЕДУПРЕЖДЕНИЕ'}</span></div><div><div class="violation-rule">${esc(h.title)}</div><div class="rule-id">${esc(v.rule)} · ${esc(({function:'функция',class:'класс',file:'файл',module:'модуль',aggregate:'сводка'})[v.scope]||v.scope)}</div><p class="violation-description">${esc(d?.message||h.description)}</p><div class="violation-entity">${esc(v.entity)}</div>${v.file?`<button class="source-jump" data-jump-file="${esc(v.file)}" data-jump-line="${Number(v.line)}">Открыть ${esc(v.file)}:${Number(v.line)}</button>`:'<button class="source-jump" data-tab="config">Посмотреть критерии</button>'}</div><div class="violation-value">Значение / порог<b>${value} / ${bound}</b>${d?algorithmicDetails({algorithmic_complexity:d.estimate,file:v.file},true):`<small>Превышение: ${num(v.value-v.limit)}</small>`}</div></article>`;
  }
  function moduleCycleCard(v) {
    const d=v.module_cycle;
    return `<article class="architecture-card"><div><span class="severity severity-${esc(v.severity)}">${v.severity==='error'?'ОШИБКА':'ПРЕДУПРЕЖДЕНИЕ'}</span> <b>${esc(help(v.rule).title)}</b></div><p class="rule-id">${esc(v.rule)} · ${esc(v.entity)}</p><p>Значение ${num(v.value)} / порог ${num(v.limit)}</p><section><h3>Модули компоненты</h3><ul>${d.participants.map(module=>`<li><code>${esc(module)}</code></li>`).join('')}</ul></section><section><h3>Замкнутый путь</h3><ol>${d.cycle.path.map(module=>`<li><code>${esc(module)}</code></li>`).join('')}</ol></section><details><summary>Все файлы компоненты · ${num(d.files.length)}</summary>${d.files.map(file=>`<div class="architecture-evidence"><button data-jump-file="${esc(file)}" data-jump-line="1">${esc(file)} ↗</button></div>`).join('')}</details><details><summary>Места зависимостей · ${num(d.evidence_total)}</summary>${d.evidence.map(e=>`<div class="architecture-evidence"><button data-jump-file="${esc(e.range.file)}" data-jump-line="${Number(e.range.start_line)}">${esc(e.range.file)}:${num(e.range.start_line)} ↗</button><p>${esc(e.description)}</p></div>`).join('')}${d.evidence_total>d.evidence.length?`<p>Показано ${num(d.evidence.length)} из ${num(d.evidence_total)} свидетельств.</p>`:''}</details></article>`;
  }
  function updateRuleFilter(selector, rules, emptyLabel) {
    const select=$(selector),selected=select.value,counts={...(rules||{})};
    // Keep the active filter when watch removes its last finding.
    if(selected&&!Object.hasOwn(counts,selected))counts[selected]=0;
    select.innerHTML=`<option value="">${esc(emptyLabel)}</option>`+Object.entries(counts).map(([id,count])=>`<option value="${esc(id)}">${esc(help(id).title)} (${num(count)})</option>`).join('');
    select.value=selected;
  }
  function architectureCard(v) {
    const d=v.architecture;
    return `<article class="architecture-card"><div><span class="severity severity-warn">ПРЕДУПРЕЖДЕНИЕ</span> <b>${esc(d.principle)} · ${esc(d.title)}</b></div><p class="rule-id">${esc(v.rule)} · ${esc(v.entity)}</p><p><b>Факт:</b> ${esc(d.fact)}</p><p><b>Оценка:</b> ${esc(d.hypothesis)}</p>${d.cycle?`<section><h3>Замкнутый путь</h3><ol>${d.cycle.path.map(m=>`<li><code>${esc(m)}</code></li>`).join('')}</ol></section>`:''}${Object.keys(d.measurements||{}).length?`<details><summary>Измерения</summary><dl class="settings-list">${Object.entries(d.measurements).map(([k,n])=>`<dt>${esc(k)}</dt><dd>${num(n)}</dd>`).join('')}</dl></details>`:''}<details><summary>Места в коде · ${num(d.evidence_total)}</summary>${(d.evidence||[]).map(e=>`<div class="architecture-evidence"><button data-jump-file="${esc(e.range.file)}" data-jump-line="${Number(e.range.start_line)}">${esc(e.range.file)}:${num(e.range.start_line)} ↗</button><p>${esc(e.description)}</p></div>`).join('')}${d.evidence_total>d.evidence.length?`<p>Показано ${num(d.evidence.length)} из ${num(d.evidence_total)} свидетельств.</p>`:''}</details>${(d.recommendations||[]).map(r=>`<section class="architecture-recommendation"><h3>${esc(r.pattern)}</h3><p>${esc(r.proposal)}</p><ul>${(r.conditions||[]).map(c=>`<li>${esc(c)}</li>`).join('')}</ul></section>`).join('')}<details><summary>Ограничения вывода</summary><ul>${(d.limitations||[]).map(l=>`<li>${esc(l)}</li>`).join('')}</ul></details></article>`;
  }
  async function loadArchitecture() {
    const request=++state.architectureRequest,p=new URLSearchParams({kind:'architecture',page:state.architecturePage,per_page:50,q:$('#architecture-search').value,rule:$('#architecture-rule').value,principle:$('#architecture-principle').value});
    $('#architecture-issues').innerHTML='<div class="empty">Загрузка архитектурного анализа…</div>';
    try {
      const data=await api(`/api/violations?${p}`);if(request!==state.architectureRequest)return;
      const a=data.architecture||{};state.architecturePage=data.page;state.architecturePages=data.pages;
      $('#architecture-count').textContent=num(data.all_total);$('#architecture-total').textContent=`Найдено ${num(data.total)} из ${num(data.all_total)}`;
      $('#architecture-summary').innerHTML=[['Профиль',a.enabled?a.profile:'Отключён'],['Предупреждения',num(a.warnings)],['Проверено файлов',num(a.analyzed_files)],['Неизвестные зависимости',num(a.unresolved_dependencies)]].map(([title,value])=>`<div class="summary-card"><span>${esc(title)}</span><b>${esc(value)}</b></div>`).join('');
      const statuses={complete:'В доступной модели',partial:'Частично',unsupported:'Нет необходимых данных',disabled:'Отключено',skipped:'Не применяется'};
      $('#architecture-coverage').innerHTML=(a.rules||[]).map(r=>`<p><b>${esc(r.title)}</b> · ${esc(statuses[r.status]||r.status)}<br/>${esc(r.reason)}</p>`).join('')+(a.diagnostics||[]).map(d=>`<p>${esc(d)}</p>`).join('')+'<p>Отсутствие предупреждений не доказывает соблюдение принципов. Строгий профиль включает проверки с подтверждёнными структурными основаниями; расширенный профиль также показывает экспериментальные эвристики.</p>';
      updateRuleFilter('#architecture-rule',data.rules,'Все правила');
      const principle=$('#architecture-principle').value;const principles=[...new Set((a.rules||[]).flatMap(r=>r.principle==='Layers / DIP'?['Layers / DIP','DIP']:[r.principle]))].sort();$('#architecture-principle').innerHTML='<option value="">Все принципы</option>'+principles.map(p=>`<option value="${esc(p)}">${esc(p)}</option>`).join('');$('#architecture-principle').value=principle;
      $('#architecture-issues').innerHTML=(data.violations||[]).map(architectureCard).join('')||'<div class="empty">Архитектурных предупреждений по выбранным фильтрам нет.</div>';
      $('#architecture-page').textContent=`Страница ${num(data.page)} из ${num(data.pages)}`;$('#architecture-prev').disabled=data.page<=1;$('#architecture-next').disabled=data.page>=data.pages;
    }catch(error){if(request===state.architectureRequest)$('#architecture-issues').innerHTML=`<div class="empty error">${esc(error.message)} <button id="retry-architecture">Повторить</button></div>`;}
  }
  function switchFileTab(tab) {
    state.fileTab=tab==='issues'?'issues':'code';
    for(const button of document.querySelectorAll('.file-tabs [data-file-tab]')) {
      const active=button.dataset.fileTab===state.fileTab;button.classList.toggle('active',active);button.setAttribute('aria-selected',String(active));button.tabIndex=active?0:-1;
    }
    $('#file-panel-code').classList.toggle('hidden',state.fileTab!=='code');$('#file-panel-issues').classList.toggle('hidden',state.fileTab!=='issues');$('#file-functions').classList.toggle('hidden',state.fileTab!=='code'||!state.fileData?.functions?.length);
    if(state.fileTab==='issues'&&state.fileData)loadFileIssues();
  }
  async function loadFileIssues() {
    if(!state.file)return;
    const request=++state.fileIssuesRequest,path=state.file,p=new URLSearchParams({file:path,page:state.fileIssuesPage,per_page:50,severity:$('#file-severity').value});
    $('#file-issues').innerHTML='<div class="empty">Загрузка замечаний файла…</div>';
    try {
      const data=await api(`/api/violations?${p}`);if(request!==state.fileIssuesRequest||path!==state.file)return;
      state.fileIssuesPage=data.page;state.fileIssuesPages=data.pages;
      const summary=data.summary||{};
      $('#file-issue-count').textContent=num(data.all_total+(data.parse_error_total||0));
      $('#file-issue-summary').textContent=`Ошибки ${num(summary.errors)} · предупреждения ${num(summary.warnings)} · разбор ${num(data.parse_error_total)} · найдено ${num(data.total)}`;
      $('#file-issues').innerHTML=(data.violations||[]).map(violationCard).join('')||'<div class="empty">Замечаний по выбранному уровню нет.</div>';
      if(data.parse_error_total)$('#file-issues').innerHTML+=`<details class="file-parse-errors"><summary>Ошибки разбора: ${num(data.parse_error_total)}</summary>${(data.parse_errors||[]).map(e=>`<div class="parse-error"><button data-jump-file="${esc(path)}" data-jump-line="${Number(e.range?.start_line||1)}">Строка ${num(e.range?.start_line||1)} ↗</button><p>${esc(e.message)}</p></div>`).join('')}${data.parse_error_total>200?'<p>Показаны первые 200 ошибок разбора.</p>':''}</details>`;
      $('#file-issues-page').textContent=`Страница ${num(data.page)} из ${num(data.pages)}`;$('#file-issues-prev').disabled=data.page<=1;$('#file-issues-next').disabled=data.page>=data.pages;
    }catch(error){if(request===state.fileIssuesRequest)$('#file-issues').innerHTML=`<div class="empty error">${esc(error.message)} <button id="retry-file-issues">Повторить</button></div>`;}
  }
  async function loadViolations() {
    const request=++state.qualityRequest,p=new URLSearchParams({kind:'metrics',page:state.qualityPage,per_page:$('#per-page').value,severity:$('#severity-filter').value,rule:$('#rule-filter').value,q:$('#violation-search').value});
    $('#violations').innerHTML='<div class="empty">Загрузка замечаний…</div>';
    try {
      const data=await api(`/api/violations?${p}`);if(request!==state.qualityRequest)return;
      state.qualityPage=data.page;state.qualityPages=data.pages;
      $('#violation-count').textContent=num(data.all_total);$('#quality-total').textContent=`Найдено ${num(data.total)} из ${num(data.all_total)}`;
      const s=data.summary||{};$('#quality-summary').innerHTML=[['Ошибки',s.errors,'error'],['Предупреждения',s.warnings,'warn'],['Ошибки разбора',s.parse_errors,'error'],['Проверено файлов',s.files,'']].map(([title,value,cls])=>`<div class="summary-card ${cls}"><span>${title}</span><b>${num(value)}</b></div>`).join('');
      updateRuleFilter('#rule-filter',data.rules,'Все метрики');
      let previous=null;$('#violations').innerHTML=(data.violations||[]).map(v=>{const file=v.file||'Сводные показатели проекта',group=file!==previous?`<h3 class="file-group">${esc(file)}</h3>`:'';previous=file;
        return group+violationCard(v);}).join('')||'<div class="empty">Нарушений по выбранным фильтрам нет.</div>';
      $('#violations-page').textContent=`Страница ${num(data.page)} из ${num(data.pages)}`;$('#violations-prev').disabled=data.page<=1;$('#violations-next').disabled=data.page>=data.pages;
      $('#parse-errors').classList.toggle('hidden',!data.parse_error_total);$('#parse-summary').textContent=`Ошибки разбора: ${num(data.parse_error_total)} — часть кода могла остаться непроанализированной`;
      $('#parse-body').innerHTML=(data.parse_errors||[]).map(e=>`<div class="parse-error"><button data-jump-file="${esc(e.range?.file)}" data-jump-line="${Number(e.range?.start_line)}">${esc(e.range?.file)}:${e.range?.start_line||1}</button><p>${esc(e.message)}</p></div>`).join('')+(data.parse_error_total>200?'<p>Показаны первые 200 ошибок разбора. Полный список: codefactor scan --format json.</p>':'');
    } catch(error){if(request===state.qualityRequest)$('#violations').innerHTML=`<div class="empty error">${esc(error.message)} <button id="retry-quality">Повторить</button></div>`;}
  }
  function thresholdTable(title,entries) {
    if(!entries.length)return '';
    return `<h3>${esc(title)}</h3><div class="table-scroll"><table class="data-table"><thead><tr><th>Метрика и назначение</th><th>Предупреждение при &gt;</th><th>Ошибка при &gt;</th></tr></thead><tbody>${entries.map(([id,limit])=>{const h=help(id);return `<tr><td><b>${esc(h.title)}</b><br/><code>${esc(id)}</code><p class="metric-explanation">${esc(h.description)}</p></td><td>${limit.warn==null?'Не задано':num(limit.warn)}</td><td>${limit.error==null?'Не задано':num(limit.error)}</td></tr>`;}).join('')}</tbody></table></div>`;
  }
  function settings(value,prefix='',out=[]) {for(const [key,item] of Object.entries(value||{})){const name=prefix?`${prefix}.${key}`:key;if(item&&typeof item==='object'&&!Array.isArray(item))settings(item,name,out);else out.push([name,Array.isArray(item)?item.map(x=>typeof x==='object'?JSON.stringify(x):String(x)).join(', '):String(item??'—')]);}return out;}
  async function loadConfig() {
    try {
      const data=await api('/api/config'),config=data.effective||{},limits=config.limits||{};
      const globals=Object.entries(limits).filter(([key])=>!['by_language','by_path','by_entity_glob','by_entity','aggregates'].includes(key));
      let html=`<section class="panel config-section"><h2>${data.path?'Конфигурация проекта':'Встроенные критерии'}</h2><p>${data.path?esc(data.path):'Файл analyzer.yaml / analyzer.yml не задан. Анализ выполнен со стандартными настройками.'}<br/>Ниже — активные настройки текущего анализа. Исходные файлы сохранены при запуске анализа.</p>${thresholdTable('Общие пороги',globals)}<p>Текущее правило проверки: нарушение фиксируется, когда значение строго больше порога. Порядок применения: общие → язык → путь → шаблон сущности → точная сущность. Поздние правила переопределяют заданные уровни. Порог — критерий проекта, а не универсальная оценка качества.</p></section>`;
      for(const [key,title] of [['by_language','Пороги по языкам'],['by_entity','Пороги по сущностям']]){const tables=Object.entries(limits[key]||{}).map(([name,values])=>thresholdTable(name,Object.entries(values))).join('');if(tables)html+=`<section class="panel config-section"><h2>${title}</h2>${tables}</section>`;}
      const capabilities=data.strategy_capabilities||state.strategyCapabilities||{},fields=[['static_types','Статические типы'],['generics','Обобщённые типы'],['inheritance','Наследование / traits'],['closures','Замыкания'],['guards','Условия']];
      html+=`<section class="panel config-section"><h2>Возможности стратегий</h2><p>Заявленные возможности обработки конструкций языка. Поддержка конструкции не гарантирует полного разрешения динамических типов и вызовов.</p><div class="table-scroll"><table class="data-table"><thead><tr><th>Язык</th>${fields.map(([,label])=>`<th>${label}</th>`).join('')}</tr></thead><tbody>${Object.entries(capabilities).map(([language,caps])=>`<tr><th>${esc(language)}${!config.languages?.enabled?.length||config.languages.enabled.includes(language)?'':' · выключен'}</th>${fields.map(([key])=>`<td>${caps[key]?'Да':'Нет'}</td>`).join('')}</tr>`).join('')}</tbody></table></div></section>`;
      if(limits.by_entity_glob?.length)html+=`<section class="panel config-section"><h2>Пороги по шаблонам сущностей</h2>${limits.by_entity_glob.map(({glob,...values})=>thresholdTable(glob,Object.entries(values))).join('')}</section>`;
      if(limits.by_path?.length)html+=`<section class="panel config-section"><h2>Пороги по путям</h2>${limits.by_path.map(({glob,...values})=>thresholdTable(glob,Object.entries(values))).join('')}</section>`;
      if(Object.keys(limits.aggregates||{}).length)html+=`<section class="panel config-section">${thresholdTable('Пороги сводных значений',Object.entries(limits.aggregates))}</section>`;
      const labels={paths:'Какие файлы анализируются',languages:'Языки анализа',vendor:'Код зависимостей',metrics:'Подсчёт метрик',algorithmic_complexity:'Алгоритмическая сложность',architecture:'Архитектурный анализ',graph:'Разрешение вызовов',cache:'Кеширование',external:'Внешние операции',sets:'Наборы сущностей',output:'Форматы экспорта'};
      for(const [key,title] of Object.entries(labels)){html+=`<section class="panel config-section"><h2>${title}</h2><dl class="settings-list">${settings(config[key]).map(([name,value])=>`<dt>${esc(name)}</dt><dd>${esc(value||'Пустой список')}</dd>`).join('')||'<dd>Дополнительных настроек нет.</dd>'}</dl></section>`;}
      html+=`<section class="panel config-section"><h2>Исходные конфигурации</h2>${(data.sources||[]).map(source=>`<details><summary>${esc(source.path)}</summary><pre class="config-source">${esc(source.source||source.error)}</pre></details>`).join('')||'<p>Использованы встроенные значения.</p>'}<details><summary>Активная конфигурация целиком · YAML</summary><pre class="config-source">${esc(data.yaml)}</pre></details>${data.diagnostics?.length?`<div class="config-diagnostics">${data.diagnostics.map(d=>`<p>${esc(d.path)}: ${esc(d.message)}</p>`).join('')}</div>`:''}</section>`;
      $('#configuration').innerHTML=html;state.configLoaded=true;
    } catch(error){$('#configuration').innerHTML=`<div class="empty error">${esc(error.message)} <button id="retry-config">Повторить</button></div>`;}
  }
  const relationFields={a:'relation-a',b:'relation-b',exclude_a:'relation-exclude-a',exclude_b:'relation-exclude-b',level:'relation-level',direction:'relation-direction',view:'relation-view',transitive:'relation-transitive',internal:'relation-internal',depth:'relation-depth',path_mode:'relation-path-mode',max_paths_per_pair:'relation-max-pair',max_paths_total:'relation-max-total',work_budget:'relation-work'};
  function relationUi(){return Object.fromEntries(Object.entries(relationFields).map(([key,id])=>{const input=$('#'+id);return [key,input.type==='checkbox'?input.checked:input.type==='number'?Number(input.value):input.value];}));}
  function applyRelationUi(ui){for(const [key,id] of Object.entries(relationFields)){const input=$('#'+id);if(input.type==='checkbox')input.checked=ui[key];else input.value=ui[key];}for(const side of ['a','b'])$('#relation-set-'+side).value=ui[side].startsWith('set:')?ui[side].slice(4):'';}
  function relationParams(transitive=true) {
    const ui=relationUi(),params=new URLSearchParams({a:ui.a.trim(),b:ui.b.trim(),level:ui.level,exclude_a:ui.exclude_a.trim(),exclude_b:ui.exclude_b.trim(),internal:String(ui.internal),path_mode:ui.path_mode});
    if(transitive&&ui.transitive){if(!$('#relation-depth').checkValidity())throw new Error('Глубина должна быть целым числом от 1 до 100.');params.set('transitive',String(ui.depth));}
    for(const key of ['max_paths_per_pair','max_paths_total','work_budget']){if(!$('#'+relationFields[key]).checkValidity())throw new Error('Проверьте ограничения путей и бюджета.');params.set(key,String(ui[key]));}
    return params;
  }
  function selectionPreview(data) {
    const byId=new Map((data.nodes||[]).map(node=>[String(node.id),node])),counts=side=>{const count=data['counts_'+side]||{};return `${num(data[side]?.length)} сущностей · ${num(count.functions)} функций · ${num(count.classes)} классов · ${num(count.files)} файлов · vendor ${num(count.vendor)}`;};
    $('#relation-preview').innerHTML=['a','b'].map(side=>`<details><summary>Набор ${side.toUpperCase()}: ${counts(side)}</summary>${(data[side]||[]).slice(0,100).map(id=>{const node=byId.get(String(id));return `<div>${esc(node?.label||id)}${node?.file?` · ${esc(node.file)}`:''}</div>`;}).join('')}${data[side]?.length>100?'<p>Показаны первые 100 сущностей.</p>':''}</details>`).join('')+`<p class="${data.overlap?.length?'tree-warning':''}">Пересечение: ${num(data.overlap?.length)}${data.warnings?.length?' · '+data.warnings.map(esc).join(' · '):''}</p>`;
  }
  async function previewRelations() {
    const request=state.previewRequest=(state.previewRequest||0)+1;
    if(!$('#relation-a').value.trim()||!$('#relation-b').value.trim()){$('#relation-preview').textContent='Укажите селекторы обоих наборов.';return;}
    try{const data=await api(`/api/relations?${relationParams(false)}`);if(request!==state.previewRequest)return;selectionPreview(data);}catch(error){if(request===state.previewRequest)$('#relation-preview').textContent=error.message;}
  }
  function invalidateRelations(){++state.relationRequest;state.previewRequest=(state.previewRequest||0)+1;state.relation=null;$('#relation-details').classList.add('hidden');$('#relations-body').innerHTML='<div class="empty">Состав изменён. Нажмите «Показать связи».</div>';setHash();}
  async function loadRelations() {
    if(!$('#relation-a').value.trim()||!$('#relation-b').value.trim()){toast('Укажите оба набора.');return;}
    const request=++state.relationRequest;state.relation=null;$('#relation-details').classList.add('hidden');setHash();$('#relations-body').innerHTML='<div class="empty">Анализ связей…</div>';
    try{const data=await api(`/api/relations?${relationParams()}`);if(request!==state.relationRequest)return;state.relation=data;state.relationPage=1;selectionPreview(data);renderRelations();}catch(error){if(request===state.relationRequest)$('#relations-body').innerHTML=`<div class="empty error">${esc(error.message)}</div>`;}
  }
  function relationData(){const ui=relationUi();return CodefactorRelations.filtered(state.relation,ui.direction,ui.internal);}
  function relationGraph(data) {
    const layout=CodefactorRelations.layout(data,$('#relation-level').value,state.relationLimit||60);state.relationLayout=layout;
    const height=Math.max(240,Math.max(layout.a.length,layout.b.length,layout.middle.length)*62+90),width=1280,x={a:20,x:460,b:900},positions=new Map();
    for(const side of ['a','x','b'])for(const [index,node] of (side==='x'?layout.middle:layout[side]).entries())positions.set(JSON.stringify({side,id:node.id}),{x:x[side],y:72+index*62});
    const edges=layout.segments.map((edge,index)=>{const from=positions.get(JSON.stringify(edge.from)),to=positions.get(JSON.stringify(edge.to));if(!from||!to)return '';const reverse=from.x>to.x,same=from.x===to.x,fx=from.x+(reverse?0:320),tx=to.x+(same||reverse?320:0),fy=from.y+22,ty=to.y+22,bend=same?from.x+375:(fx+tx)/2,path=same&&from.y===to.y?`M ${fx} ${fy} C ${bend} ${fy-65} ${bend} ${fy+65} ${tx} ${ty}`:`M ${fx} ${fy} C ${bend} ${fy-(Math.abs(from.x-to.x)>440?50+index%3*12:0)} ${bend} ${ty-(Math.abs(from.x-to.x)>440?50+index%3*12:0)} ${tx} ${ty}`;return `<g class="relation-link" data-relation-edge="${index}" tabindex="0" role="button" aria-label="Показать места вызова: ${esc(edge.from.id)} → ${esc(edge.to.id)}"><path d="${path}" class="relation-edge${edge.transitive?' transitive':''}" marker-end="url(#relation-arrow)"/><path d="${path}" class="relation-hit"/><title>${esc(edge.from.id)} → ${esc(edge.to.id)} · ${edge.details.length} вызовов или путей</title></g>`;}).join('');
    const nodes=(list,side)=>list.map((node,index)=>`<g class="relation-node ${side}" transform="translate(${x[side]},${72+index*62})"><title>${esc(node.label)} · ${node.members.length} сущностей</title><rect width="320" height="44" rx="7"/><text x="12" y="20">${esc(short(node.label,43))}</text><text class="muted" x="12" y="35">${node.members.length} сущностей${side==='x'?' · промежуточный код':''}</text></g>`).join('');
    return `<div class="relations-graph"><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" role="img" aria-label="Связи между наборами A и B"><defs><marker id="relation-arrow" markerWidth="9" markerHeight="7" refX="8" refY="3.5" orient="auto"><path d="M0,0 L9,3.5 L0,7 Z" fill="#637eb0"/></marker></defs><rect class="relation-cluster a" x="10" y="5" width="340" height="${height-10}" rx="12"/><rect class="relation-cluster b" x="890" y="5" width="340" height="${height-10}" rx="12"/><text x="25" y="32">Набор A</text><text x="905" y="32">Набор B</text>${layout.middle.length?'<text x="465" y="32">Промежуточный код</text>':''}${edges}${nodes(layout.a,'a')}${nodes(layout.middle,'x')}${nodes(layout.b,'b')}</svg></div><p class="muted">Стрелка — направление вызова. Фиолетовые узлы — промежуточный код. Нажмите на стрелку для мест вызова и полного пути.</p>${layout.omittedA||layout.omittedB||layout.omittedMiddle?`<p>За пределами холста: A ${num(layout.omittedA)}, B ${num(layout.omittedB)}, промежуточных ${num(layout.omittedMiddle)}. <button id="relations-more">Показать ещё 60 групп</button></p>`:''}`;
  }
  function relationCallDetails(items) {
    const labels=new Map((state.relation.nodes||[]).map(node=>[String(node.id),node.label])),label=id=>labels.get(String(id))||id;
    return items.map(item=>`<section class="call-detail"><h3>${item.path?'Транзитивный путь':item.side?.startsWith('internal')?'Внутренний вызов':'Прямой вызов'}: ${esc(label(item.from))} → ${esc(label(item.to))}</h3>${(item.path?.edges||[item.edge]).map(edge=>`<p>${esc(label(edge.from))} → ${esc(label(edge.to))} · ${esc(edge.confidence)}</p>${edge.callsite?.file?`<button data-jump-file="${esc(edge.callsite.file)}" data-jump-line="${Number(edge.callsite.start_line)||1}">${esc(edge.callsite.file)}:${Number(edge.callsite.start_line)||1} ↗</button>`:''}${(edge.guard||[]).map(guard=>`<div><b>${esc(guard.branch||guard.kind)}</b><pre>${esc(guard.normalized||guard.raw)}</pre>${guard.raw!==guard.normalized?`<details><summary>Исходное условие</summary><pre>${esc(guard.raw)}</pre></details>`:''}</div>`).join('')}${edge.in_loop?.length?`<p>В цикле: ${edge.in_loop.map(loop=>esc(loop.header||loop.kind||'цикл')).join(' · ')}</p>`:''}`).join('')}</section>`).join('')||'<p>Связей нет.</p>';
  }
  function inspectRelation(indices,limit=50){state.relationDetailIndices=[...new Set(indices)];state.relationDetailLimit=limit;const items=CodefactorRelations.entries(relationData()),details=state.relationDetailIndices.map(index=>items[index]).filter(Boolean);$('#relation-details').classList.remove('hidden');$('#relation-details-body').innerHTML=`<p>${num(details.length)} вызовов и путей</p>`+relationCallDetails(details.slice(0,limit))+(details.length>limit?'<button id="relation-details-more">Ещё 50</button>':'');$('#relation-details').scrollIntoView({block:'nearest'});}
  function renderRelations() {
    if(!state.relation)return;const data=relationData(),items=CodefactorRelations.entries(data),paths=items.filter(item=>item.path),matrix=CodefactorRelations.matrixRows(data,$('#relation-level').value),page=state.relationPage,view=$('#relation-view').value;state.relationMatrix=matrix;$('#relation-details').classList.add('hidden');
    let body='',pages=1;
    if(view==='graph')body=relationGraph(data);
    if(view==='matrix'){pages=Math.max(1,Math.ceil(matrix.length/50));body=`<h3>Матрица связей · ${esc($('#relation-level').value)}</h3><div class="table-scroll"><table class="data-table"><thead><tr><th>Источник</th><th>Цель</th><th>Маршруты / прямые вызовы</th></tr></thead><tbody>${matrix.slice((page-1)*50,page*50).map((row,index)=>`<tr><td>${esc(row.a)}</td><td>${esc(row.b)}</td><td><button data-relation-cell="${(page-1)*50+index}" aria-label="Места вызова ${esc(row.a)} → ${esc(row.b)}">${num(row.count)} / ${num(row.direct)}</button></td></tr>`).join('')}</tbody></table></div>`;}
    if(view==='calls'){pages=Math.max(1,Math.ceil(items.length/50));body=relationCallDetails(items.slice((page-1)*50,page*50));}
    const callers=new Set(items.map(item=>item.from)),callees=new Set(items.map(item=>item.to));
    $('#relations-body').innerHTML=`<div class="relation-block"><p>Прямые A → B: ${num(data.a_to_b.length)} · B → A: ${num(data.b_to_a.length)} · Транзитивных путей: ${num(paths.length)} · Пересечение: ${num(data.overlap?.length)}</p><p>Уникальных источников: ${num(callers.size)} · целей: ${num(callees.size)}. Режим: ${esc(data.path_mode||'one_shortest')} · уникальных маршрутов: ${num(data.route_count)} · прямых мест вызова: ${num(data.direct_call_count)} · бюджет: ${num(data.work_used)} / ${num(data.options?.work_budget)}.</p>${data.truncated?`<p class="tree-warning">Маршруты ограничены: ${(data.truncation_reasons||[]).map(esc).join(', ')}. Прямые вызовы показаны полностью.</p>`:''}${data.warnings?.length?`<p class="tree-warning">${data.warnings.map(esc).join(' · ')}</p>`:''}${body}${view!=='graph'?`<div class="relations-pages"><button id="relation-prev" ${page<=1?'disabled':''}>← Назад</button><span>Страница ${page} из ${pages}</span><button id="relation-next" ${page>=pages?'disabled':''}>Далее →</button></div>`:''}</div>`;
    if($('#relation-prev'))$('#relation-prev').onclick=()=>{state.relationPage--;renderRelations();};if($('#relation-next'))$('#relation-next').onclick=()=>{state.relationPage++;renderRelations();};if($('#relations-more'))$('#relations-more').onclick=()=>{state.relationLimit=(state.relationLimit||60)+60;renderRelations();};
  }
  async function openRelationPicker(side,exclude){if(!state.treeLoaded)await loadTree(true);const input=$('#relation-'+(exclude?'exclude-':'')+side);state.relationPicker={side,exclude,tree:CodefactorTree.prepare(state.tree),selected:new Set(input.value.split(',').map(value=>value.trim()).filter(Boolean))};$('#relation-tree-title').textContent=`${exclude?'Исключения':'Состав'} набора ${side.toUpperCase()}`;$('#relation-tree-search').value='';$('#relation-tree').scrollTop=0;$('#relation-tree-modal').showModal();renderRelationPicker();}
  function renderRelationPicker(){const picker=state.relationPicker;if(!picker)return;const pane=$('#relation-tree'),rows=CodefactorTree.visible(picker.tree,$('#relation-tree-search').value).rows,height=34,start=Math.max(0,Math.floor(pane.scrollTop/height)-8),end=Math.min(rows.length,start+Math.ceil((pane.clientHeight||400)/height)+16);pane.innerHTML=`<div style="height:${start*height}px"></div>`+rows.slice(start,end).map(({node,depth})=>`<div class="tree-row" style="--depth:${depth}">${node.children.length?`<button data-relation-toggle="${esc(node.id)}" aria-expanded="${!node.collapsed}">${node.collapsed?'›':'⌄'}</button>`:'<span class="tree-disclosure"></span>'}<label><input type="checkbox" data-relation-node="${esc(node.id)}" ${picker.selected.has(CodefactorTree.selector(node))?'checked':''}/><span title="${esc(node.fqn||node.path||node.name)}">${esc(node.name)}</span></label>${node.vendor?'<span class="tree-vendor">vendor</span>':''}</div>`).join('')+`<div style="height:${Math.max(0,rows.length-end)*height}px"></div>`;$('#relation-tree-count').textContent=`Выбрано селекторов: ${picker.selected.size}`;}
  function exportRelations(format){
    if(!state.relation){toast('Сначала запросите связи.');return;}const ui=relationUi(),data={...relationData(),ui_state:ui};
    if(format==='json'){download('codefactor-relations.json','application/json',JSON.stringify(data,null,2));return;}
    if(format==='md'){download('codefactor-relations.md','text/markdown',CodefactorRelations.markdown(data,ui));return;}
    const host=document.createElement('div');host.innerHTML=relationGraph(data);const svg=$('svg',host),box=svg.viewBox.baseVal,width=box.width,height=box.height;svg.setAttribute('width',width);svg.setAttribute('height',height);
    const style=document.createElementNS('http://www.w3.org/2000/svg','style');style.textContent='.relation-cluster{fill:#f2f6ff;stroke:#b9cae7}.relation-cluster.b{fill:#f4f9f5;stroke:#b7d2be}.relation-node rect{fill:#fff;stroke:#9db0cf}.relation-node.x rect{fill:#f2eaff;stroke:#9e7fc5}.relation-node text{font:12px monospace;fill:#2e4569}.relation-node text.muted{font:10px sans-serif;fill:#72819a}.relation-edge{fill:none;stroke:#637eb0;stroke-width:1.5}.relation-edge.transitive{stroke-dasharray:7 4}.relation-hit{display:none}';svg.prepend(style);
    const metadata={ui_state:ui,truncated:data.truncated,truncation_reasons:data.truncation_reasons,revision:data.revision,omitted_a:state.relationLayout.omittedA,omitted_b:state.relationLayout.omittedB,omitted_middle:state.relationLayout.omittedMiddle},tag=document.createElementNS('http://www.w3.org/2000/svg','metadata');tag.textContent=JSON.stringify(metadata);svg.prepend(tag);const xml=new XMLSerializer().serializeToString(svg);
    if(format==='svg'){download('codefactor-relations.svg','image/svg+xml',xml);return;}
    const url=URL.createObjectURL(new Blob([xml],{type:'image/svg+xml;charset=utf-8'})),image=new Image();image.onload=()=>{const ratio=Math.min(1,8192/width,8192/height,Math.sqrt(16000000/(width*height))),canvas=document.createElement('canvas');canvas.width=Math.max(1,Math.round(width*ratio));canvas.height=Math.max(1,Math.round(height*ratio));const ctx=canvas.getContext('2d');ctx.fillStyle='#fff';ctx.fillRect(0,0,canvas.width,canvas.height);ctx.drawImage(image,0,0,canvas.width,canvas.height);canvas.toBlob(async blob=>{if(blob){const result=await CodefactorGraph.pngWithMetadata(blob,metadata),link=document.createElement('a'),blobUrl=URL.createObjectURL(result);link.href=blobUrl;link.download='codefactor-relations.png';link.click();setTimeout(()=>URL.revokeObjectURL(blobUrl),1000);}else toast('Не удалось создать PNG.');},'image/png');URL.revokeObjectURL(url);};image.onerror=()=>{URL.revokeObjectURL(url);toast('Не удалось создать PNG.');};image.src=url;
  }
  const download = CodefactorUI.download;
  function exportSvg() {
    if(!state.graph)return null;
    // Export the visible layout at natural size, independent of viewport pan/zoom.
    const svg=$('#graph svg')?.cloneNode(true);if(!svg)return null;
    const layout=state.graphLayout||layoutGraph(state.graph),points=[...layout.positions].map(([id,p])=>({...p,height:layout.heights.get(String(id))||170})),bounds=layout.frames.map(f=>CodefactorGraph.frameBounds(f,layout.positions));
    const minX=Math.min(...points.map(p=>p.x),...bounds.map(b=>b.x))-60,minY=Math.min(...points.map(p=>p.y),...bounds.map(b=>b.y))-120;
    const width=Math.max(...points.map(p=>p.x+320),...bounds.map(b=>b.x+b.width))-minX+60,height=Math.max(...points.map(p=>p.y+p.height),...bounds.map(b=>b.y+b.height))-minY+60;
    svg.setAttribute('viewBox',`${minX} ${minY} ${width} ${height}`);svg.setAttribute('width',width);svg.setAttribute('height',height);$('.graph-world',svg).removeAttribute('transform');
    const css=document.createElementNS('http://www.w3.org/2000/svg','style');css.textContent='.graph-node rect{fill:#fff;stroke:#9db0cf;stroke-width:1.5}.graph-node text{fill:#253c61;font:13px monospace}.graph-node .node-subtitle{fill:#677b99;font:11px sans-serif}.graph-node .node-cost{fill:#315dcc;font:11px monospace}.graph-node.root rect{fill:#e9f0ff;stroke:#315dcc;stroke-width:2.5}.graph-node.external rect{fill:#fff6e7;stroke:#c59748}.graph-node.vendor rect{fill:#f5edff;stroke:#9c81ba}.graph-edge path{fill:none;stroke:#8a9dbd;stroke-width:1.4}.graph-edge.ambiguous path{stroke:#b38830;stroke-dasharray:8 5}.graph-edge.dynamic path{stroke:#b56581;stroke-dasharray:3 5}.graph-edge .edge-hit{display:none}.edge-label{fill:#516c96;font:10px sans-serif;paint-order:stroke;stroke:#fff;stroke-width:4px}';css.textContent+=' .class-frame-box{fill:#f6f9ff;stroke:#a7bcdf;stroke-width:1.5}.class-frame-heading text{fill:#435c87;font:11px monospace}.graph-node .node-action rect{fill:#f0f4fc;stroke:#9eb2d7;stroke-dasharray:none}.graph-node .node-action text{fill:#315dcc;font:11px sans-serif}.graph-node .node-owner,.graph-node .node-stats,.graph-node .node-hidden{font:11px sans-serif;fill:#536c91}.graph-node.fold .node-box{fill:#f4f7fd;stroke-dasharray:5 3}.graph-node.unknown .node-box{fill:#fff2f2;stroke:#ce8585;stroke-dasharray:3 3}.edge-chip rect{fill:#fff;stroke:#d9e2ee}.edge-chip text{fill:#536c91;font:10px sans-serif}.graph-node.self-method .node-box{fill:#eaf7ef;stroke:#66a87d}.graph-node.boundary .node-box{fill:#fff7e8;stroke:#bbac89;stroke-dasharray:5 3}';css.textContent+=' .graph-node .node-class{font:11px monospace}.graph-node .node-name{font:600 13px monospace}.graph-node .node-field-label{font:9px sans-serif;fill:#72839b}';svg.prepend(css);
    const metadata=document.createElementNS('http://www.w3.org/2000/svg','metadata');metadata.textContent=JSON.stringify(CodefactorGraph.exportMetadata(state.graph));svg.prepend(metadata);
    return {xml:new XMLSerializer().serializeToString(svg),width,height};
  }
  function exportGraph(format) {
    const data=state.graph;if(!data){toast('Сначала дождитесь загрузки графа.');return;}
    if(format==='json')download('codefactor-graph.json','application/json',JSON.stringify(data,null,2));
    else if(format==='md'){const cell=s=>String(s||'—').replace(/\|/g,'\\|').replace(/[\n\r]/g,' '),labels=new Map(data.nodes.map(n=>[n.id,n.label]));download('codefactor-graph.md','text/markdown',`# ${cell(state.symbol.label)}\n\n${data.coverage_scope==='requested'?`Полный охват запроса по разобранному графу: ${data.coverage?.functions?.length||0} функций проекта.\n\n`:''}Метаданные выгрузки: \`${JSON.stringify(CodefactorGraph.exportMetadata(data))}\`.\n\n${data.lazy?`Загруженный подграф; ${data.partial?'выгрузка частичная':'заданные пределы загружены'}; глубина ${data.requested_depth??'∞'}.\n\n`:''}| Функция | Сложность | Статус |\n|---|---|---|\n`+data.nodes.map(n=>`| ${cell(n.label)} | ${cell(n.algorithmic_complexity?.formula)} | ${cell(({tight:'Точный класс',upper_bound:'Верхняя граница',heuristic:'Предположение',unknown:'Неизвестно',unsupported:'Не поддерживается',disabled:'Отключено'})[n.algorithmic_complexity?.status])} |`).join('\n')+'\n\n'+data.edges.filter(e=>e.relation!=='member').map(e=>`- ${labels.get(e.from)||e.from} → ${labels.get(e.to)||e.to} · ${e.callsite?.file||''}:${e.callsite?.start_line||''} ${edgeGuard(e)}${e.argument!=null?' · аргумент: '+cell(e.argument):''}`).join('\n'));}
    else if(format==='mermaid'){const ids=new Map(data.nodes.map((n,i)=>[n.id,`n${i}`])),label=s=>String(s||'').replace(/["\n\r]/g,"'");download('codefactor-graph.mmd','text/plain','%% Codefactor metadata: '+JSON.stringify(CodefactorGraph.exportMetadata(data))+'\nflowchart LR\n'+data.nodes.map(n=>`  ${ids.get(n.id)}["${label(n.label+ (algorithmicLabel(n)?' · '+algorithmicLabel(n)+' · '+n.algorithmic_complexity.status:''))}"]`).join('\n')+'\n'+data.edges.filter(e=>ids.has(e.from)&&ids.has(e.to)).map(e=>`  ${ids.get(e.from)} --> ${ids.get(e.to)}`).join('\n'));}
    else {const svg=exportSvg();if(!svg)return;if(format==='svg'){download('codefactor-graph.svg','image/svg+xml',svg.xml);return;}
      const url=URL.createObjectURL(new Blob([svg.xml],{type:'image/svg+xml;charset=utf-8'})),img=new Image();img.onload=()=>{const canvas=document.createElement('canvas'),ratio=Math.min(1,8192/svg.width,8192/svg.height,Math.sqrt(16000000/(svg.width*svg.height)));canvas.width=Math.max(1,Math.round(svg.width*ratio));canvas.height=Math.max(1,Math.round(svg.height*ratio));const ctx=canvas.getContext('2d');ctx.fillStyle='#fff';ctx.fillRect(0,0,canvas.width,canvas.height);ctx.drawImage(img,0,0,canvas.width,canvas.height);canvas.toBlob(async blob=>{if(blob){blob=await CodefactorGraph.pngWithMetadata(blob,CodefactorGraph.exportMetadata(data));const a=document.createElement('a'),blobUrl=URL.createObjectURL(blob);a.href=blobUrl;a.download='codefactor-graph.png';a.click();setTimeout(()=>URL.revokeObjectURL(blobUrl),1000);}else toast('Не удалось создать PNG. Используйте SVG.');},'image/png');URL.revokeObjectURL(url);};img.onerror=()=>{URL.revokeObjectURL(url);toast('Не удалось создать PNG.');};img.src=url;
    }
  }
  function handleRelationClick(target) {
    const pick=target.closest('[data-relation-pick]');if(pick){openRelationPicker(pick.dataset.relationPick,pick.dataset.relationExclude==='true');return true;}
    const pickToggle=target.closest('[data-relation-toggle]');if(pickToggle){const node=findFileNode(pickToggle.dataset.relationToggle,state.relationPicker.tree);if(node){node.collapsed=!node.collapsed;renderRelationPicker();}return true;}
    const pickNode=target.closest('[data-relation-node]');if(pickNode){const node=findFileNode(pickNode.dataset.relationNode,state.relationPicker.tree),selector=CodefactorTree.selector(node);if(pickNode.checked)state.relationPicker.selected.add(selector);else state.relationPicker.selected.delete(selector);renderRelationPicker();return true;}
    const relationEdge=target.closest('[data-relation-edge]');if(relationEdge){inspectRelation(state.relationLayout.segments[Number(relationEdge.dataset.relationEdge)].details);return true;}
    const relationCell=target.closest('[data-relation-cell]');if(relationCell){inspectRelation(state.relationMatrix[Number(relationCell.dataset.relationCell)].details);return true;}
    const relationExport=target.closest('[data-relation-export]');if(relationExport){exportRelations(relationExport.dataset.relationExport);return true;}
    if(target.closest('#relation-details-more')){inspectRelation(state.relationDetailIndices,state.relationDetailLimit+50);return true;}
    return false;
  }
  function handleTreeClick(target) {
    const fileTab=target.closest('[data-file-tab]');if(fileTab){if(!fileTab.disabled)switchFileTab(fileTab.dataset.fileTab);return true;}
    const tab=target.closest('[data-tab]');if(tab){switchTab(tab.dataset.tab);return true;}
    const toggle=target.closest('[data-tree-toggle]');if(toggle){const node=findFileNode(toggle.dataset.treeToggle);if(node){node.collapsed=!node.collapsed;renderTree();}return true;}
    const fileButton=target.closest('[data-tree-node]');if(fileButton){const node=findFileNode(fileButton.dataset.treeNode);if(!node)return true;state.treeSelection=node.id;if(['directory','dir'].includes(node.kind))node.collapsed=!node.collapsed;else if(node.kind==='file'){node.collapsed=false;loadFile(node.path||node.id);}else{node.collapsed=false;state.imported=null;openGraph({...node,id:node.symbol_id||node.id,label:node.kind==='module_initializer'?node.name:node.fqn||node.name,kind:node.kind==='module_initializer'?'function':['method','function'].includes(node.kind)?node.kind:'class'});}renderTree();return true;}
    const source=target.closest('[data-source-link]');if(source){chooseTargets(state.sourceLinks[Number(source.dataset.sourceLink)]);return true;}
    return false;
  }
  function handleGraphNavigationClick(target) {
    if(Date.now()<state.suppressGraphClickUntil&&target.closest('#graph'))return true;
    const hidden=target.closest('[data-vendor-boundary]');if(hidden){inspectHiddenVendor(hidden.dataset.vendorBoundary);return true;}
    if(target.closest('#show-hidden-vendor')){state.vendor='expanded';$('#vendor-mode').value='expanded';$('#graph-inspector').classList.add('hidden');loadGraph();return true;}
    if(target.closest('#chain-more')){state.graphLimit+=600;renderGraph();return true;}
    const chain=target.closest('[data-chain-go]');if(chain){const node=state.graph?.nodes.find(n=>String(n.id)===chain.dataset.chainGo);if(node){if(node.kind==='unknown')inspectNode(node);else openGraph(node);}return true;}
    const go=target.closest('[data-graph-go]');if(go){const node=state.graph?.nodes.find(n=>String(n.id)===go.dataset.graphGo);if(!node)return true;if(node.kind==='vendor_package'&&!state.imported){api(`/api/vendor/${encodeURIComponent(node.package||node.label)}`).then(()=>{state.vendor='expanded';$('#vendor-mode').value=state.vendor;loadGraph();}).catch(e=>toast(e.message));}else openGraph(node);return true;}
    return handleGraphBranchClick(target);
  }
  function handleGraphBranchClick(target) {
    const branchClose=target.closest('[data-branch-close]');if(branchClose){const id=branchClose.dataset.branchClose;state.branchTokens.set(id,(state.branchTokens.get(id)||0)+1);state.branchBusy.delete(id);if(id===state.graphView.root){state.branchEpoch++;state.branchBusy.clear();}state.graphView.close(id);renderGraph();return true;}
    const branch=target.closest('[data-branch-expand],[data-branch-all]');if(branch){expandBranch(branch.dataset.branchExpand||branch.dataset.branchAll,Boolean(branch.dataset.branchAll));return true;}
    const neighbors=target.closest('[data-neighbors-expand],[data-neighbors-all],[data-neighbors-close]');if(neighbors){const key=neighbors.dataset.neighborsExpand||neighbors.dataset.neighborsAll||neighbors.dataset.neighborsClose;if(neighbors.dataset.neighborsClose)state.graphView.closeNeighbors(key);else state.graphView.expandNeighbors(key,Boolean(neighbors.dataset.neighborsAll));$('#graph-inspector').classList.add('hidden');renderGraph();return true;}
    return false;
  }
  function handleGraphInspectionClick(target) {
    const fold=target.closest('[data-group-expand]');if(fold){state.graphView.groupExpanded.add(fold.dataset.groupExpand);renderGraph();return true;}
    const classGo=target.closest('[data-class-go]');if(classGo){openGraph({id:'type:'+classGo.dataset.classGo,label:classGo.dataset.classGo,kind:'class',language:classGo.dataset.classLanguage});return true;}
    const classInfo=target.closest('[data-class-info]');if(classInfo){const frame=state.graphLayout.frames.find(f=>f.id===classInfo.dataset.classInfo);if(frame)inspectClass(frame);return true;}
    const nodeBox=target.closest('[data-graph-node]');if(nodeBox){const node=state.graphLayout?.nodes.find(n=>String(n.id)===nodeBox.dataset.graphNode);if(node)inspectNode(node);return true;}
    const entry=target.closest('[data-history]');if(entry){goHistory(Number(entry.dataset.history));return true;}
    const edge=target.closest('.conditional-edge[data-edge-index]');if(edge){const bundle=state.graphLayout?.bundles[Number(edge.dataset.edgeIndex)];if(bundle)inspectEdge(bundle);return true;}
    const graphSource=target.closest('[data-graph-source-file]');if(graphSource){graphModal.close();switchTab('code');loadFile(graphSource.dataset.graphSourceFile,graphSource.dataset.jumpLine);return true;}
    if(target.closest('#more-group-members')){inspectNode(state.inspectedNodeObject,state.inspectedMemberLimit+50);return true;}
    if(target.closest('#more-external-calls')){inspectNode(state.inspectedNodeObject,state.inspectedMemberLimit+50);return true;}
    if(target.closest('#more-class-members')){inspectClass(state.inspectedClass,state.inspectedMemberLimit+50);return true;}
    if(target.closest('#more-call-details')){inspectEdge(state.inspectedBundle,state.inspectedCalls+50);return true;}
    return false;
  }
  function handleExportAndRetryClick(target) {
    const jump=target.closest('[data-jump-file]');if(jump){if($('#relations-modal').open)$('#relations-modal').close();switchTab('code');loadFile(jump.dataset.jumpFile,jump.dataset.jumpLine);return true;}
    const exportButton=target.closest('[data-export]');if(exportButton){exportGraph(exportButton.dataset.export);return true;}
    if(target.closest('#graph-more')){state.graphLimit+=600;renderGraph();}
    if(target.closest('#retry-file-issues'))loadFileIssues();if(target.closest('#retry-graph'))loadGraph();if(target.closest('#retry-quality'))loadViolations();if(target.closest('#retry-config'))loadConfig();
    return false;
  }
  document.addEventListener('click', event => {
    const target = event.target;
    for (const handler of [handleRelationClick, handleTreeClick, handleGraphNavigationClick, handleGraphInspectionClick, handleExportAndRetryClick]) {
      if (handler(target)) break;
    }
  });
  $('#file-severity').onchange=()=>{state.fileIssuesPage=1;loadFileIssues();};
  $('#file-issues-prev').onclick=()=>{if(state.fileIssuesPage>1){state.fileIssuesPage--;loadFileIssues();}};
  $('#file-issues-next').onclick=()=>{if(state.fileIssuesPage<state.fileIssuesPages){state.fileIssuesPage++;loadFileIssues();}};
  $('.file-tabs').addEventListener('keydown',event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;event.preventDefault();const next=event.key==='Home'?'code':event.key==='End'?'issues':state.fileTab==='code'?'issues':'code';if(next==='issues'&&$('#file-tab-issues').disabled)return;switchFileTab(next);$(`#file-tab-${next}`).focus();});
  function openRelations(){$('#map-options').open=false;$('#relations-modal').showModal();setHash();previewRelations();}
  $('#open-relations').onclick=openRelations;
  $('[data-open-relations]').onclick=openRelations;
  $('#relations-close').onclick=()=>$('#relations-modal').close();
  $('#relations-modal').addEventListener('close',()=>{++state.relationRequest;state.previewRequest=(state.previewRequest||0)+1;setHash();});
  $('#relations-modal').addEventListener('keydown',event=>{if(['Enter',' '].includes(event.key)&&event.target.closest('.relation-link')){event.preventDefault();event.target.closest('.relation-link').dispatchEvent(new MouseEvent('click',{bubbles:true}));}});
  $('#run-relations').onclick=()=>{state.relationLimit=60;loadRelations();};
  for(const side of ['a','b']){$('#relation-set-'+side).onchange=event=>{if(event.target.value)$('#relation-'+side).value='set:'+event.target.value;invalidateRelations();previewRelations();};$('#relation-'+side).addEventListener('input',()=>{$('#relation-set-'+side).value='';});for(const id of ['relation-'+side,'relation-exclude-'+side]){$('#'+id).addEventListener('input',invalidateRelations);$('#'+id).addEventListener('input',debounce(previewRelations));}}
  for(const id of ['relation-view','relation-direction'])$('#'+id).onchange=()=>{state.relationPage=1;setHash();renderRelations();};
  for(const id of ['relation-level','relation-internal','relation-transitive','relation-depth','relation-path-mode','relation-max-pair','relation-max-total','relation-work'])$('#'+id).onchange=()=>{const reload=Boolean(state.relation);invalidateRelations();if(reload)loadRelations();else previewRelations();};
  $('#relation-details-close').onclick=()=>$('#relation-details').classList.add('hidden');
  $('#relation-tree-close').onclick=()=>$('#relation-tree-modal').close();$('#relation-tree').addEventListener('scroll',renderRelationPicker);$('#relation-tree-search').oninput=()=>{$('#relation-tree').scrollTop=0;renderRelationPicker();};
  $('#relation-tree-clear').onclick=()=>{state.relationPicker.selected.clear();renderRelationPicker();};
  $('#relation-tree-apply').onclick=()=>{const picker=state.relationPicker;$('#relation-'+(picker.exclude?'exclude-':'')+picker.side).value=[...picker.selected].join(',');if(!picker.exclude)$('#relation-set-'+picker.side).value='';$('#relation-tree-modal').close();invalidateRelations();previewRelations();};
  $('#tree').addEventListener('scroll',renderTree);
  $('#search').addEventListener('input',event=>{state.query=event.target.value;$('#tree').scrollTop=0;renderTree();});
  $('#tree-warnings').addEventListener('change',event=>{state.warningsOnly=event.target.checked;$('#tree').scrollTop=0;renderTree();});
  $('#collapse-all').onclick=()=>{const walk=list=>list.forEach(n=>{n.collapsed=true;walk(n.children);});walk(state.tree);renderTree();};
  $('#open-function').onclick=()=>{state.imported=null;const fun=state.fileData?.functions?.[Number($('#function-select').value)];if(fun)openGraph({...fun,file:state.file});};
  $('#target-close').onclick=()=>$('#target-modal').close();
  $('#graph-close').onclick=()=>graphModal.close();
  graphModal.addEventListener('close',()=>{++state.graphRequest;if(state.imported){state.imported=null;state.history=[];state.historyAt=-1;}setHash();});
  $('#graph-source').onclick=()=>{const node=state.symbol;if(node?.file){graphModal.close();switchTab('code');loadFile(node.file,node.line);}};
  $('#history-back').onclick=()=>goHistory(state.historyAt-1);$('#history-forward').onclick=()=>goHistory(state.historyAt+1);
  function graphControls(){state.direction=$('#direction').value;const depth=$('#depth');if(depth.value&&(!depth.checkValidity()||Number(depth.value)<1)){toast('Глубина должна быть целым числом от 1.');return;}state.depth=depth.value;state.external=$('#show-external').checked;state.category=$('#external-category').value;state.vendor=$('#vendor-mode').value;state.raw=$('#raw-guards').checked;state.graphLimit=600;setHash();loadGraph();}
  $('#reload-graph').onclick=graphControls;
  for(const id of ['direction','show-external','external-category','vendor-mode'])$('#'+id).addEventListener('change',graphControls);
  $('#graph-view-mode').onchange=()=>{state.graphLimit=600;renderGraph(true);};
  $('#raw-guards').addEventListener('change',event=>{state.raw=event.target.checked;renderGraph();});
  $('#inspector-close').onclick=()=>{$('#graph-inspector').classList.add('hidden');state.inspectedNode=null;};
  $('#graph-layout').onclick=()=>{state.nodePositions.clear();renderGraph(true);};
  $('#collapse-simple').onchange=event=>{state.collapseSimple=event.target.checked;renderGraph();};
  $('#group-classes').onchange=event=>{state.groupClasses=event.target.checked;renderGraph();};
  $('#zoom-in').onclick=()=>zoom(state.scale*1.25);$('#zoom-out').onclick=()=>zoom(state.scale/1.25);$('#zoom-reset').onclick=()=>{state.scale=1;centerGraph();};$('#graph-center').onclick=centerGraph;
  $('#import-graph').onclick=()=>$('#import-file').click();
  $('#import-file').addEventListener('change',async event=>{const file=event.target.files?.[0];if(!file)return;try{const data=JSON.parse(await file.text());if(!Array.isArray(data.nodes)||!Array.isArray(data.edges)||!data.nodes.length||data.nodes.some(n=>typeof n.id!=='string')||data.edges.some(e=>typeof e.from!=='string'||typeof e.to!=='string'))throw new Error('Ожидается JSON графа с массивами nodes и edges.');const ids=new Set(data.nodes.map(n=>n.id));if(ids.size!==data.nodes.length||data.edges.some(e=>!ids.has(e.from)||!ids.has(e.to)))throw new Error('Граф содержит повторные ID или связи с отсутствующими узлами.');state.imported=data;state.direction=['both','callers','callees'].includes(data.direction)?data.direction:'both';state.depth=Number.isInteger(data.depth)&&data.depth>0?String(data.depth):'';$('#direction').value=state.direction;$('#depth').value=state.depth;state.history=[];state.historyAt=-1;openGraph(data.nodes.find(n=>n.id===(data.root||data.roots?.[0]))||data.nodes[0]);toast('Граф импортирован. Переходы выполняются по импортированным данным.');}catch(error){toast(error.message);}event.target.value='';});
  const qualityFilters=()=>{state.qualityPage=1;loadViolations();};$('#violation-search').addEventListener('input',debounce(qualityFilters));
  for(const id of ['severity-filter','rule-filter','per-page'])$('#'+id).addEventListener('change',qualityFilters);
  $('#violations-prev').onclick=()=>{if(state.qualityPage>1){state.qualityPage--;loadViolations();}};$('#violations-next').onclick=()=>{if(state.qualityPage<state.qualityPages){state.qualityPage++;loadViolations();}};
  const architectureFilters=()=>{state.architecturePage=1;loadArchitecture();};
  $('#architecture-search').addEventListener('input',debounce(architectureFilters));
  for(const id of ['architecture-rule','architecture-principle'])$('#'+id).addEventListener('change',architectureFilters);
  $('#architecture-prev').onclick=()=>{if(state.architecturePage>1){state.architecturePage--;loadArchitecture();}};
  $('#architecture-next').onclick=()=>{if(state.architecturePage<state.architecturePages){state.architecturePage++;loadArchitecture();}};
  $('#architecture-issues').addEventListener('click',event=>{if(event.target.closest('#retry-architecture'))loadArchitecture();});
  $('.tabs').addEventListener('keydown',event=>{if(!['ArrowLeft','ArrowRight','Home','End'].includes(event.key))return;event.preventDefault();const tabs=[...document.querySelectorAll('.tabs [data-tab]')],at=tabs.findIndex(t=>t.dataset.tab===state.tab),next=event.key==='Home'?0:event.key==='End'?tabs.length-1:(at+(event.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;switchTab(tabs[next].dataset.tab);tabs[next].focus();});
  window.addEventListener('resize',debounce(()=>{renderTree();if(graphModal.open)renderGraph(true);},100));
  async function restoreHash(){const params=new URLSearchParams(location.hash.slice(1));state.restoringHash=true;try{applyRelationUi(CodefactorRelations.readState(params));mapUi.readState(params);switchTab(params.get('tab')||'code');if(params.get('file'))await loadFile(params.get('file'));if(params.get('symbol')){state.direction=['both','callers','callees'].includes(params.get('direction'))?params.get('direction'):'both';state.depth=/^[1-9]\d*$/.test(params.get('depth')||'')?params.get('depth'):'';$('#direction').value=state.direction;$('#depth').value=state.depth;openGraph({id:params.get('symbol'),label:params.get('symbol')});}else if(graphModal.open)graphModal.close();if(params.get('relations')==='1'){if(!$('#relations-modal').open)$('#relations-modal').showModal();if($('#relation-a').value&&$('#relation-b').value)await loadRelations();else previewRelations();}else if($('#relations-modal').open)$('#relations-modal').close();}finally{state.restoringHash=false;setHash();}}
  window.addEventListener('hashchange',restoreHash);
  async function initial(){
    const originalHash=location.hash;const initialTab=new URLSearchParams(originalHash.slice(1)).get('tab');if(['code','codemap','quality','architecture','config'].includes(initialTab))state.tab=initialTab;
    // Read the revision first, so analysis published during loading is retried by watch.
    try{state.revision=(await api('/api/status')).revision;}catch(error){console.error(error);}
    try {const [catalog]=await Promise.all([api('/api/metrics/catalog'),loadTree()]);state.catalog=new Map(catalog.map(m=>[m.id,m]));}catch(error){toast(error.message);$('#tree').innerHTML=`<div class="empty error">${esc(error.message)}</div>`;}
    history.replaceState(null,'',originalHash||'#tab=code');await restoreHash();
    let refreshing=false;
    setInterval(async()=>{if(refreshing||document.hidden)return;refreshing=true;try{const status=await api('/api/status');const watchError=$('#watch-error');watchError.textContent=status.watch_error?`Не удалось обновить анализ: ${status.watch_error}. Показан последний успешный отчёт.`:'';watchError.classList.toggle('hidden',!status.watch_error);if(state.revision===null||state.revision!==status.revision){graphResponses.clear();symbolDetails.clear();await loadTree();const pending=state.filePending;if(pending||state.file)await loadFile(pending?.id||state.file,pending?.line);if(state.tab==='quality')await loadViolations();if(state.tab==='architecture')await loadArchitecture();if(state.tab==='codemap')await mapUi.refresh(status.revision);if(graphModal.open&&!state.imported)await loadGraph();if($('#relations-modal').open&&state.relation)await loadRelations();state.revision=status.revision;}}catch(error){console.error('Не удалось обновить анализ:',error);}finally{refreshing=false;}},3000);
  }
  const mapUi=CodefactorMap.create({get:api,onChange:setHash,onSource:(file,line)=>{switchTab('code');loadFile(file,line);}});
  initial();
})();

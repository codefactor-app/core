'use strict';
/* The code map has its own request lifecycle; it never changes function graph state. */
window.CodefactorMap={create({get,onChange,onSource}){
  const M=CodefactorMapModel,$=id=>document.getElementById(id),esc=v=>String(v??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])),num=n=>Number(n||0).toLocaleString('ru-RU');
  const labels={call:'Вызовы',import:'Импорты',inheritance:'Наследование',type:'Типы',data:'Данные'},statuses={project:'Код проекта',vendor:'Разобранная библиотека',package:'Пакет вне анализа',external:'Вне анализа',unresolved:'Цель не определена'};
  let ui=M.defaults(),data=null,request=0,detailRequest=0,detail=null,limit=200,scale=1,pan=[0,0],drag=null,layout=null;
  let revisionFloor=-1,lastQuery=null,lastHighlight=null,dragFrame=0,suppressClickUntil=0,pendingReveal=null;
  const offsets=new Map();
  function syncControls(){controls.sync();}
  function changed(){onChange();}
  function options(){return M.query(ui);}
  function clearDetail(){++detailRequest;detail=null;$('map-inspector').classList.add('hidden');}
  async function refresh(revision){if(Number.isFinite(revision))revisionFloor=Math.max(revisionFloor,revision);const token=++request,query=options();query.set('compact','true');const key=query.toString();if(data&&lastQuery===key&&data.revision>=revisionFloor){render();if(ui.selected&&!detail)await inspect({block:ui.selected,mode:directionMode()},1);return;}const previousDetail=detail;clearDetail();
    $('map-status').textContent='Загрузка карты…';
    try{const result=await get('/api/codemap?'+query);if(token!==request)return;if(result.revision<revisionFloor){return refresh();}revisionFloor=result.revision;data=result;lastQuery=key;
      const ids=new Set(data.blocks.map(b=>b.id));ui.expanded=ui.expanded.filter(id=>ids.has(id));if(ui.selected&&!ids.has(ui.selected)){ui.selected='';ui.direction='both';clearDetail();}
      render();if(previousDetail?.mode==='link'&&M.selection(data,ui).links.some(l=>l.from===previousDetail.from&&l.to===previousDetail.to))await inspect(previousDetail,1);else if(ui.selected)await inspect({block:ui.selected,mode:previousDetail?.block===ui.selected?previousDetail.mode:directionMode()},1);changed();
    }catch(error){if(token===request){$('map-status').textContent=error.message;$('map-content').innerHTML='<div class="empty error">'+esc(error.message)+' <button id="map-retry">Повторить</button></div>';}}
  }
  function coverage(){const c=data.coverage,details=[];if(c.parse_errors)details.push('Ошибки разбора: '+num(c.parse_errors));if(c.files_without_body_facts)details.push('Без фактов тел: '+num(c.files_without_body_facts));if(c.deferred_vendor_files)details.push('Не разобраны файлы библиотек: '+num(c.deferred_vendor_files));if(c.unresolved_dependencies)details.push('Не разрешены зависимости: '+num(c.unresolved_dependencies));
    $('map-coverage').innerHTML='<summary>Охват анализа · '+num(c.files)+' файлов · '+(c.complete?'все доступные факты':'есть ограничения')+'</summary><p>Статические зависимости, обнаруженные анализатором. Стандартные вызовы скрыты. Счётчик стрелки — число свидетельств, а не число модулей.</p>'+details.map(d=>'<p>'+esc(d)+'</p>').join('')+[...(c.diagnostics||[]),...(data.warnings||[])].map(d=>'<p>'+esc(d)+'</p>').join('');
  }
  const text=(lines,x,y,css)=>lines.map((line,i)=>'<text class="'+css+'" x="'+x+'" y="'+(y+i*16)+'">'+esc(line)+'</text>').join('');
  function svgBody(l){for(const node of [...l.cards,...l.frames]){node.drawX=node.x;node.drawY=node.y;node.drawWidth=node.width;}const neighbor=id=>!ui.selected||l.neighbors.has(id)||l.hasAncestor(id,ui.selected);
    const frames=l.frames.map(f=>'<g class="map-frame'+(f.id===ui.selected?' selected':'')+'" data-map-block="'+esc(f.id)+'" tabindex="0" role="button" aria-label="'+esc(f.block.label)+'"><rect x="'+f.x+'" y="'+f.y+'" width="'+f.width+'" height="'+f.height+'" rx="10"/>'+text(f.lines,f.x+12,f.y+22,'map-name')+'<g data-map-toggle="'+esc(f.id)+'" tabindex="0" role="button" aria-label="Свернуть '+esc(f.block.label)+'"><rect x="'+(f.x+f.width-31)+'" y="'+(f.y+8)+'" width="22" height="22" rx="4"/><text x="'+(f.x+f.width-24)+'" y="'+(f.y+24)+'">−</text></g></g>').join('');
    const links=l.links.map((link,i)=>{const a=l.positions.get(link.from),b=l.positions.get(link.to),reverse=a.x>b.x,ax=reverse?a.x:a.x+a.width,bx=reverse?b.x+b.width:b.x,ay=a.y+a.height/2,by=b.y+b.height/2,bend=Math.max(24,Math.abs(bx-ax)/2),path=ax===bx?'M'+ax+','+ay+' C'+(ax+60)+','+ay+' '+(bx+60)+','+by+' '+bx+','+by:'M'+ax+','+ay+' C'+(ax+(reverse?-bend:bend))+','+ay+' '+(bx+(reverse?bend:-bend))+','+by+' '+bx+','+by;
      return '<g class="map-link'+(link.uncertain?' uncertain':'')+(neighbor(link.from)&&neighbor(link.to)?'':' dimmed')+'" data-map-link="'+i+'" tabindex="0" role="button" aria-label="'+esc(a.block.label+' → '+b.block.label+': '+link.count)+'"><title>'+esc(a.block.label+' → '+b.block.label+' · '+link.count+' свидетельств')+'</title><path class="map-link-line" d="'+path+'" marker-end="url(#codemap-arrow)"/><path class="map-link-hit" d="'+path+'"/><text x="'+((ax+bx)/2)+'" y="'+((ay+by)/2-5)+'">'+num(link.count)+'</text></g>';
    }).join('');
    const cards=l.cards.map(c=>{const b=c.block,selected=b.id===ui.selected;return '<g class="map-card '+esc(b.status)+(selected?' selected':'')+(neighbor(c.id)?'':' dimmed')+'" data-map-block="'+esc(c.id)+'" tabindex="0" role="button" aria-label="'+esc(b.label+' · '+b.counts.files+' файлов')+'"><title>'+esc(b.label+' · '+statuses[b.status])+'</title><rect x="'+c.x+'" y="'+c.y+'" width="'+c.width+'" height="'+c.height+'" rx="8"/>'+text(c.lines,c.x+12,c.y+22,'map-name')+'<text class="map-counts" x="'+(c.x+12)+'" y="'+(c.y+c.height-32)+'">'+num(b.counts.files)+' файлов · '+num(b.counts.types)+' типов · '+num(b.counts.functions)+' функций</text><text class="map-counts" x="'+(c.x+12)+'" y="'+(c.y+c.height-14)+'">← '+num(b.incoming)+' · → '+num(b.outgoing)+' · внутри '+num(b.internal)+(b.shared_entities?' · общих '+num(b.shared_entities):'')+'</text>'+(b.children.length?'<g data-map-toggle="'+esc(c.id)+'" tabindex="0" role="button" aria-label="Раскрыть '+esc(b.label)+'"><rect x="'+(c.x+c.width-31)+'" y="'+(c.y+8)+'" width="22" height="22" rx="4"/><text x="'+(c.x+c.width-24)+'" y="'+(c.y+24)+'">+</text></g>':'')+'</g>';}).join('');
    return '<defs><marker id="codemap-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M0 0 L10 5 L0 10 Z" fill="#6a82a6"/></marker></defs>'+frames+links+cards;
  }
  function transform(){const layer=$('map-content').querySelector('.map-layer');if(layer)layer.setAttribute('transform','translate('+pan.join(' ')+') scale('+scale+')');$('map-zoom-value').textContent=Math.round(scale*100)+'%';}
  function fit(){if(!layout)return;const pane=$('map-content');scale=Math.min(1,Math.max(.15,Math.min(pane.clientWidth/layout.width,pane.clientHeight/layout.height)));pan=[-layout.minX*scale,-layout.minY*scale];transform();}
  function reveal(id){const node=layout?.frames.find(n=>n.id===id)||layout?.positions.get(id);if(!node)return;const pane=$('map-content'),width=pane.clientWidth-48,height=pane.clientHeight-72;
    scale=Math.min(scale,Math.max(.15,Math.min(width/node.width,height/node.height)));pan=[24-node.x*scale,24-node.y*scale];transform();
  }
  function render(){if(!data)return;coverage();const s=M.selection(data,ui);$('map-status').textContent=num(s.ids.length)+' блоков · '+num(s.links.filter(l=>l.from!==l.to).length)+' связей';syncControls();controls.resize();
    $('map-content').classList.toggle('matrix',ui.view==='matrix');$('map-more').classList.add('hidden');
    if(!s.ids.length){$('map-content').innerHTML='<div class="empty">Блоки не найдены. Измените группировку, поиск или включите внешние зависимости.</div>';return;}
    if(ui.view==='matrix'){if(!$('map-matrix-spacer'))$('map-content').innerHTML='<div id="map-matrix-spacer"></div>';renderMatrix();}
    else{layout=M.layout(data,ui,limit,offsets);lastHighlight=null;$('map-content').innerHTML='<svg class="map-svg" xmlns="http://www.w3.org/2000/svg" aria-label="Карта зависимостей"><g class="map-layer">'+svgBody(layout)+'</g></svg>';if(layout.omitted){$('map-more').classList.remove('hidden');$('map-more').textContent='Ещё 200 · скрыто '+num(layout.omitted);}transform();highlight();if(pendingReveal){reveal(pendingReveal);pendingReveal=null;}}
  }
  function renderMatrix(){if(!data||ui.view!=='matrix')return;const pane=$('map-content'),spacer=$('map-matrix-spacer');if(!spacer)return;let w=M.matrixWindow(data,ui,pane.scrollTop,pane.scrollLeft,pane.clientHeight,pane.clientWidth);spacer.style.width=w.width+'px';spacer.style.height=w.height+'px';w=M.matrixWindow(data,ui,pane.scrollTop,pane.scrollLeft,pane.clientHeight,pane.clientWidth);
    let html='<div class="map-matrix-corner" style="left:'+pane.scrollLeft+'px;top:'+pane.scrollTop+'px">Источник ↓ / цель →</div>';
    for(let col=w.colStart;col<w.colEnd;col++){const id=w.ids[col],b=w.blocks.get(id);html+='<button class="map-matrix-label map-column" data-map-block="'+esc(id)+'" title="'+esc(b.label)+'" style="left:'+(w.label+col*w.columnWidth)+'px;top:'+pane.scrollTop+'px;width:'+w.columnWidth+'px">'+esc(b.label)+'</button>';}
    for(let row=w.rowStart;row<w.rowEnd;row++){const id=w.ids[row],b=w.blocks.get(id),y=w.header+row*w.rowHeight;html+='<button class="map-matrix-label map-row" data-map-block="'+esc(id)+'" title="'+esc(b.label)+'" style="left:'+pane.scrollLeft+'px;top:'+y+'px">'+esc(b.label)+'</button>';
      for(let col=w.colStart;col<w.colEnd;col++){const to=w.ids[col],link=w.links.get(JSON.stringify([id,to])),count=link?.count||0;html+='<button class="map-cell'+(id===to?' internal':'')+(count?' linked':'')+(link?.uncertain?' uncertain':'')+'" data-map-from="'+esc(id)+'" data-map-to="'+esc(to)+'" '+(count?'':'disabled')+' aria-label="'+esc(b.label+' → '+w.blocks.get(to).label+': '+count)+'" style="left:'+(w.label+col*w.columnWidth)+'px;top:'+y+'px">'+(count?num(count):'·')+'</button>';}}
    spacer.innerHTML=html;
  }
  async function inspect(selection,page=1){if(!data)return;const token=++detailRequest,revision=data.revision;detail={...selection,page};$('map-inspector').classList.remove('hidden');$('map-details').innerHTML='<p>Загрузка…</p>';
    const params=options();params.set('revision',revision);for(const [k,v]of Object.entries(detail))params.set(k,v);
    try{const result=await get('/api/codemap/evidence?'+params);if(token!==detailRequest||data.revision!==revision)return;const entities=new Map(result.entities.map(e=>[e.id,e]));
      const block=data.blocks.find(b=>b.id===selection.block),title=block?.label||((data.blocks.find(b=>b.id===selection.from)?.label||selection.from)+' → '+(data.blocks.find(b=>b.id===selection.to)?.label||selection.to));
      const modes=block?'<div class="map-detail-tabs">'+[['members','Состав'],['incoming','Входящие'],['outgoing','Исходящие'],['internal','Внутренние']].map(([mode,label])=>'<button data-map-mode="'+mode+'" class="'+(mode===selection.mode?'active':'')+'">'+label+'</button>').join('')+'</div>':'';
      const jump=e=>e?.file?'<button data-map-source="'+esc(e.file)+'" data-map-line="'+(e.line||1)+'">'+esc(e.file)+':'+(e.line||1)+' ↗</button>':'';
      const contents=selection.mode==='members'?result.entities.map(e=>'<article class="map-evidence"><b>'+esc(e.label)+'</b><p>'+esc(({file:'Файл',type:'Тип',function:'Функция',initializer:'Инициализация',boundary:'Внешняя цель'})[e.kind]||e.kind)+' · '+esc(statuses[e.status])+'</p>'+jump(e)+'</article>').join(''):(result.witnesses||[]).map(w=>{const from=entities.get(w.from),to=entities.get(w.to);return '<article class="map-evidence"><b>'+esc(from?.label)+' → '+esc(to?.label)+'</b><p>'+esc(labels[w.kind])+' · '+esc(w.detail_kind)+' · '+esc(({resolved:'Подтверждено',ambiguous:'Неоднозначно',dynamic:'Не разрешено'})[w.confidence])+'</p><button data-map-source="'+esc(w.range.file)+'" data-map-line="'+w.range.start_line+'">'+esc(w.range.file)+':'+w.range.start_line+' ↗</button>'+jump(to)+(w.guard||[]).map(g=>'<p>Условие ('+esc(g.branch||g.kind)+'): <code>'+esc(g.normalized||g.raw)+'</code></p>'+(g.raw!==g.normalized?'<details><summary>Исходное условие</summary><pre>'+esc(g.raw)+'</pre></details>':'')).join('')+(w.in_loop?.length?'<p>В цикле: '+w.in_loop.map(l=>esc(l.header||l.kind)).join(' · ')+'</p>':'')+'</article>';}).join('');
      $('map-details').innerHTML='<h2>'+esc(title)+'</h2>'+modes+(block?.shared_entities?'<p>Общих сущностей с другими наборами: '+num(block.shared_entities)+'. Пересечение состава само по себе не является зависимостью.</p>':'')+'<p>'+num(result.total)+' '+(selection.mode==='members'?'сущностей':'свидетельств')+'</p>'+(contents||'<p>Нет записей.</p>')+'<div class="map-detail-pages"><button id="map-detail-prev" '+(page<=1?'disabled':'')+'>←</button><span>'+page+' / '+Math.max(1,Math.ceil(result.total/result.per_page))+'</span><button id="map-detail-next" '+(page*result.per_page>=result.total?'disabled':'')+'>→</button></div>';
    }catch(error){if(token!==detailRequest)return;if(String(error.message).includes('409')){lastQuery=null;get('/api/status').then(status=>refresh(status.revision));}else $('map-details').innerHTML='<p>'+esc(error.message)+'</p><button id="map-detail-retry">Повторить</button>';}
  }
  function directionMode(){return ui.direction==='both'?'members':ui.direction;}
  function choose(id){ui.selected=id;changed();syncControls();if(ui.view==='matrix'||ui.direction!=='both')render();else{lastHighlight=null;$('map-content').querySelectorAll('[data-map-block]').forEach(n=>n.classList.toggle('selected',n.dataset.mapBlock===id));highlight();}inspect({block:id,mode:directionMode()});}
  function toggle(id){const opening=!ui.expanded.includes(id);ui.expanded=opening?[...ui.expanded,id]:ui.expanded.filter(v=>v!==id);pendingReveal=id;clearDetail();changed();refresh();}
  function click(event){if(event.type==='click'&&event.detail!==0&&event.target.closest('#map-content')&&Date.now()<suppressClickUntil){event.preventDefault();return;}const target=event.target;if(target.closest('#map-retry')){refresh();return;}if(target.closest('#map-detail-retry')){inspect(detail,detail.page);return;}
    const source=target.closest('[data-map-source]');if(source){onSource(source.dataset.mapSource,Number(source.dataset.mapLine));return;}
    const mode=target.closest('[data-map-mode]');if(mode){const selected=detail.block,value=mode.dataset.mapMode;ui.selected=selected;ui.direction=['incoming','outgoing'].includes(value)?value:'both';changed();render();inspect({block:selected,mode:value});return;}
    if(target.closest('#map-detail-prev')){inspect(detail,detail.page-1);return;}if(target.closest('#map-detail-next')){inspect(detail,detail.page+1);return;}
    const expansion=target.closest('[data-map-toggle]');if(expansion){toggle(expansion.dataset.mapToggle);return;}
    const link=target.closest('[data-map-link]');if(link){const l=layout.links[Number(link.dataset.mapLink)];inspect({from:l.from,to:l.to,mode:'link'});return;}
    const cell=target.closest('[data-map-from]');if(cell){inspect({from:cell.dataset.mapFrom,to:cell.dataset.mapTo,mode:'link'});return;}
    const block=target.closest('[data-map-block]');if(block)choose(block.dataset.mapBlock);
  }
  const exportStyle='.map-frame>rect{fill:#f2f6fc;stroke:#b8c9e2}.map-card>rect{fill:#fff;stroke:#9bb0cf}.map-card.unresolved>rect{fill:#fff2f2;stroke:#ba7c7c;stroke-dasharray:4 3}.map-card.vendor>rect,.map-card.package>rect{fill:#f4edff;stroke:#9d82bf}.map-card.external>rect{fill:#fff7e6;stroke:#bdac88}.map-name{font:12px monospace;fill:#253e63}.map-counts{font:11px sans-serif;fill:#607796}.map-link-line{fill:none;stroke:#6a82a6;stroke-width:1.5}.map-link.uncertain .map-link-line{stroke-dasharray:5 4}.map-link-hit{fill:none;stroke:transparent;stroke-width:14}.map-link text{font:11px sans-serif;fill:#526c93;paint-order:stroke;stroke:#fff;stroke-width:4px}.map-frame g rect,.map-card g rect{fill:#eaf0fa;stroke:#a9bcdc}.map-frame g text,.map-card g text{font:14px sans-serif;fill:#345c9a}.dimmed{opacity:.2}.selected>rect{stroke:#315dcc;stroke-width:2.5}';
  function download(name,type,body){const url=URL.createObjectURL(new Blob([body],{type})),a=document.createElement('a');a.href=url;a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
  async function exportMap(format){await refresh();const query=options();query.set('compact','true');if(!data||lastQuery!==query.toString()||data.revision<revisionFloor)return;const selection=M.selection(data,ui),metadata={revision:data.revision,ui_state:ui,coverage:data.coverage};
    if(format==='json'){const blockIds=new Set(selection.ids);for(const id of selection.ids){let b=selection.blocks.get(id);while(b?.parent){blockIds.add(b.parent);b=selection.blocks.get(b.parent);}}download('codefactor-codemap.json','application/json',JSON.stringify({...data,blocks:data.blocks.filter(b=>blockIds.has(b.id)),frontier:selection.ids,links:selection.links,ui_state:ui},null,2));return;}
    let body,width,height;
    if(ui.view==='matrix'){const pane=$('map-content'),w=M.matrixWindow(data,ui,pane.scrollTop,pane.scrollLeft,pane.clientHeight,pane.clientWidth),rows=w.ids.slice(w.rowStart,w.rowEnd),cols=w.ids.slice(w.colStart,w.colEnd);width=230+cols.length*120;height=100+rows.length*38;metadata.visible_rows=rows;metadata.visible_columns=cols;metadata.omitted_rows=w.ids.length-rows.length;metadata.omitted_columns=w.ids.length-cols.length;
      body='<rect width="100%" height="100%" fill="white"/>'+cols.map((id,i)=>text(M.wrap(w.blocks.get(id).label,15).slice(0,5),240+i*120,18,'map-name')).join('')+rows.map((id,r)=>text(M.wrap(w.blocks.get(id).label,29).slice(0,2),8,118+r*38,'map-name')+cols.map((to,c)=>'<rect x="'+(230+c*120)+'" y="'+(100+r*38)+'" width="120" height="38" fill="'+(id===to?'#f0edf8':'#f4f7fc')+'" stroke="#d6e0ec"/><text class="map-name" x="'+(285+c*120)+'" y="'+(124+r*38)+'">'+(w.links.get(JSON.stringify([id,to]))?.count||'·')+'</text>').join('')).join('');
    }else{const l=M.layout(data,ui,limit,offsets);width=l.width;height=l.height;body=svgBody(l);metadata.origin=[l.minX,l.minY];metadata.layout_positions=Object.fromEntries(offsets);metadata.omitted_blocks=l.omitted;metadata.visible_blocks=l.cards.map(c=>c.id);}
    const host=document.createElement('div');host.innerHTML='<svg xmlns="http://www.w3.org/2000/svg" width="'+width+'" height="'+height+'" viewBox="'+(metadata.origin?.[0]||0)+' '+(metadata.origin?.[1]||0)+' '+width+' '+height+'"><style>'+exportStyle+'</style>'+body+'</svg>';const svg=host.firstChild,tag=document.createElementNS('http://www.w3.org/2000/svg','metadata');tag.textContent=JSON.stringify(metadata);svg.prepend(tag);const xml=new XMLSerializer().serializeToString(svg);
    if(format==='svg'){download('codefactor-codemap.svg','image/svg+xml',xml);return;}
    const url=URL.createObjectURL(new Blob([xml],{type:'image/svg+xml;charset=utf-8'})),image=new Image();image.onload=()=>{const ratio=Math.min(1,8192/width,8192/height,Math.sqrt(16000000/(width*height))),canvas=document.createElement('canvas');canvas.width=Math.max(1,Math.round(width*ratio));canvas.height=Math.max(1,Math.round(height*ratio));const ctx=canvas.getContext('2d');ctx.fillStyle='#fff';ctx.fillRect(0,0,canvas.width,canvas.height);ctx.drawImage(image,0,0,canvas.width,canvas.height);canvas.toBlob(async blob=>{if(blob){const result=await CodefactorGraph.pngWithMetadata(blob,metadata);download('codefactor-codemap.png','image/png',result);}else $('map-status').textContent='Не удалось создать PNG.';},'image/png');URL.revokeObjectURL(url);};image.onerror=()=>{URL.revokeObjectURL(url);$('map-status').textContent='Не удалось создать PNG.';};image.src=url;
  }
  $('page-codemap').addEventListener('click',click);$('page-codemap').addEventListener('keydown',event=>{if(['Enter',' '].includes(event.key)&&event.target.matches('svg [role="button"]')){event.preventDefault();click(event);}});
  let searchTimer;
  function changeControl(key,value){
    ui[key]=value;
    if(key==='group'){
      if(['directory','module'].includes(value))ui.structure=value;
      ui.expanded=[];ui.selected='';ui.direction='both';limit=200;scale=1;pan=[0,0];pendingReveal=null;offsets.clear();clearDetail();
      syncControls();changed();refresh();return;
    }
    syncControls();changed();
    if(key==='search'){clearTimeout(searchTimer);searchTimer=setTimeout(render,150);}
    else if(key==='kinds'||key==='external')refresh();
    else{if(key==='arrangement')offsets.clear();render();if(key==='direction'&&ui.selected)inspect({block:ui.selected,mode:directionMode()});}
  }
  function controlAction(name,value){
    if(name==='overview'){ui.selected='';ui.search='';ui.direction='both';clearDetail();syncControls();changed();render();fit();}
    else if(name==='collapse'){ui.expanded=[];clearDetail();changed();refresh();}
    else if(name==='zoom-in'){scale=Math.min(8,scale*1.2);transform();}
    else if(name==='zoom-out'){scale=Math.max(.15,scale/1.2);transform();}
    else if(name==='zoom-value'){scale=1;pan=[0,0];transform();}
    else if(name==='layout'){offsets.clear();render();}
    else if(name==='fullscreen'){const active=document.body.classList.toggle('map-fullscreen');$('map-fullscreen').setAttribute('aria-pressed',String(active));$('map-fullscreen').title=active?'Вернуться к обычному виду':'Развернуть карту на весь экран';controls.resize();}
    else if(name==='export')exportMap(value);
    else if(name==='resize'&&ui.view==='matrix')renderMatrix();
  }
  const controls=CodefactorMapControls.create({state:()=>ui,change:changeControl,action:controlAction});
  $('map-inspector-close').onclick=()=>{ui.selected='';ui.direction='both';syncControls();clearDetail();changed();render();};
  $('map-more').onclick=()=>{limit+=200;render();};
  const pane=$('map-content');pane.addEventListener('scroll',()=>{if(ui.view==='matrix')renderMatrix();});
  pane.addEventListener('wheel',event=>{if(ui.view==='matrix')return;event.preventDefault();const rect=pane.getBoundingClientRect(),x=event.clientX-rect.left,y=event.clientY-rect.top,next=Math.min(8,Math.max(.15,scale*(event.deltaY<0?1.12:1/1.12)));pan=[x-(x-pan[0])*next/scale,y-(y-pan[1])*next/scale];scale=next;transform();},{passive:false});
  function updatePositions(){
    if(!layout)return;M.applyOffsets(layout,offsets);
    const elements=new Map([...pane.querySelectorAll('[data-map-block]')].map(n=>[n.dataset.mapBlock,n]));
    for(const node of [...layout.cards,...layout.frames]){
      const element=elements.get(node.id);if(!element)continue;
      element.setAttribute('transform',`translate(${node.x-node.drawX} ${node.y-node.drawY})`);
      if(node.items){const rect=element.querySelector('rect');rect.setAttribute('width',node.width);rect.setAttribute('height',node.height);element.querySelector('[data-map-toggle]')?.setAttribute('transform',`translate(${node.width-node.drawWidth} 0)`);}
    }
    pane.querySelectorAll('[data-map-link]').forEach(element=>{const link=layout.links[Number(element.dataset.mapLink)],a=layout.positions.get(link.from),b=layout.positions.get(link.to),reverse=a.x>b.x,ax=reverse?a.x:a.x+a.width,bx=reverse?b.x+b.width:b.x,ay=a.y+a.height/2,by=b.y+b.height/2,bend=Math.max(24,Math.abs(bx-ax)/2),path=`M${ax},${ay} C${ax+(reverse?-bend:bend)},${ay} ${bx+(reverse?bend:-bend)},${by} ${bx},${by}`;element.querySelectorAll('path').forEach(p=>p.setAttribute('d',path));const label=element.querySelector('text');label.setAttribute('x',(ax+bx)/2);label.setAttribute('y',(ay+by)/2-5);});
  }
  function applyDrag(){dragFrame=0;if(!drag||!drag.moved)return;const dx=drag.latestX-drag.x,dy=drag.latestY-drag.y;if(drag.id){offsets.set(drag.id,[drag.offset[0]+dx/scale,drag.offset[1]+dy/scale]);updatePositions();}else{pan=[drag.pan[0]+dx,drag.pan[1]+dy];transform();}}
  pane.addEventListener('pointerdown',event=>{
    if(ui.view==='matrix'||event.button!==0||event.target.closest('[data-map-toggle],[data-map-link]'))return;
    const block=event.target.closest('[data-map-block]');drag={id:block?.dataset.mapBlock,x:event.clientX,y:event.clientY,latestX:event.clientX,latestY:event.clientY,pan:[...pan],offset:[...(offsets.get(block?.dataset.mapBlock)||[0,0])],moved:false};(block||pane).setPointerCapture(event.pointerId);
  });
  pane.addEventListener('pointermove',event=>{if(drag){drag.latestX=event.clientX;drag.latestY=event.clientY;if(Math.hypot(event.clientX-drag.x,event.clientY-drag.y)>4)drag.moved=true;if(drag.moved&&!dragFrame)dragFrame=requestAnimationFrame(applyDrag);return;}highlight(event.target.closest('[data-map-block]')?.dataset.mapBlock);});
  function endDrag(){if(dragFrame){cancelAnimationFrame(dragFrame);applyDrag();}if(drag?.moved)suppressClickUntil=Date.now()+300;drag=null;}
  for(const event of ['pointerup','pointercancel','lostpointercapture'])pane.addEventListener(event,endDrag);
  pane.addEventListener('pointerleave',()=>highlight());pane.addEventListener('focusin',event=>highlight(event.target.closest('[data-map-block]')?.dataset.mapBlock));pane.addEventListener('focusout',()=>highlight());
  function highlight(id){if(!layout||ui.view==='matrix')return;const selected=ui.selected||id,key=JSON.stringify([selected,ui.direction]);if(lastHighlight===key)return;lastHighlight=key;const focus=M.neighborhood(layout,selected,ui.direction);
    pane.querySelectorAll('.map-card').forEach(node=>node.classList.toggle('dimmed',Boolean(selected)&&!focus.neighbors.has(node.dataset.mapBlock)));
    pane.querySelectorAll('.map-frame').forEach(node=>node.classList.toggle('dimmed',Boolean(selected)&&![...focus.neighbors].some(id=>layout.hasAncestor(id,node.dataset.mapBlock))));
    pane.querySelectorAll('.map-link').forEach(node=>node.classList.toggle('dimmed',Boolean(selected)&&!focus.edges.has(Number(node.dataset.mapLink))));
  }
  syncControls();
  return {refresh,writeState:params=>M.writeState(params,ui),readState:params=>{++request;clearTimeout(searchTimer);ui=M.readState(params);limit=200;scale=1;pan=[0,0];pendingReveal=null;offsets.clear();syncControls();clearDetail();},getState:()=>ui,setNamedSets:sets=>controls.setNamedSets(sets)};
}};

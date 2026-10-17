((root,factory)=>{const model=factory();if(typeof module==='object'&&module.exports)module.exports=model;else root.CodefactorMapModel=model;})(globalThis,()=>{
  'use strict';
  const kinds=['call','import','inheritance','type','data'];
  const defaults=()=>({group:'directory',structure:'directory',view:'map',arrangement:'grid',expanded:[],kinds:[...kinds],external:false,selected:'',search:'',direction:'both'});
  function readState(params){const ui=defaults();for(const key of ['group','structure','view','arrangement','selected','search','direction'])if(params.has('map_'+key))ui[key]=params.get('map_'+key);
    if(!['directory','file','module','set'].includes(ui.group))ui.group='directory';if(!['directory','module'].includes(ui.structure))ui.structure='directory';if(['directory','module'].includes(ui.group))ui.structure=ui.group;if(!['map','matrix'].includes(ui.view))ui.view='map';if(!['both','incoming','outgoing'].includes(ui.direction))ui.direction='both';
    if(!['grid','list'].includes(ui.arrangement))ui.arrangement='grid';
    try{const value=JSON.parse(params.get('map_expanded')||'[]');ui.expanded=Array.isArray(value)?value.filter(v=>typeof v==='string'):[];}catch{}
    if(params.has('map_kinds'))ui.kinds=params.get('map_kinds').split(',').filter(k=>kinds.includes(k));ui.external=params.get('map_external')==='1';return ui;
  }
  function writeState(params,ui){for(const key of ['group','structure','view','arrangement','selected','search','direction']){params.delete('map_'+key);if(ui[key])params.set('map_'+key,ui[key]);}params.delete('map_expanded');if(ui.expanded.length)params.set('map_expanded',JSON.stringify(ui.expanded));params.set('map_kinds',ui.kinds.join(','));params.delete('map_external');if(ui.external)params.set('map_external','1');}
  function query(ui){return new URLSearchParams({group:ui.group,expanded:JSON.stringify(ui.expanded),kinds:ui.kinds.length?ui.kinds.join(','):'none',external:String(ui.external)});}
  const prepared=new WeakMap();
  function selection(data,ui){
    let index=prepared.get(data);
    if(!index){index={blocks:new Map(data.blocks.map(b=>[b.id,b])),selections:new Map()};prepared.set(data,index);}
    const key=JSON.stringify([ui.search,ui.selected,ui.direction]);if(index.selections.has(key))return index.selections.get(key);
    const blocks=index.blocks,needle=ui.search.toLowerCase(),hasAncestor=(id,parent)=>{let b=blocks.get(id);while(b){if(b.id===parent)return true;b=blocks.get(b.parent);}return false;};
    const ids=data.frontier.filter(id=>{let b=blocks.get(id);if(!needle)return true;while(b){if(b.label.toLowerCase().includes(needle))return true;b=blocks.get(b.parent);}return false;});
    const selected=new Set(ids.filter(id=>hasAncestor(id,ui.selected))),shown=new Set(ids);
    let links=data.links.filter(l=>shown.has(l.from)&&shown.has(l.to));
    if(ui.selected&&ui.direction!=='both')links=links.filter(l=>ui.direction==='incoming'?!selected.has(l.from)&&selected.has(l.to):selected.has(l.from)&&!selected.has(l.to));
    const neighbors=new Set(selected);for(const l of links)if(selected.has(l.from)||selected.has(l.to)){neighbors.add(l.from);neighbors.add(l.to);}
    const result={ids,links,blocks,selected,neighbors,hasAncestor,linkLookup:new Map(links.map(l=>[JSON.stringify([l.from,l.to]),l]))};
    if(index.selections.size>=8)index.selections.delete(index.selections.keys().next().value);index.selections.set(key,result);return result;
  }
  function wrap(value,max=34){const chars=Array.from(String(value)),lines=[];for(let i=0;i<chars.length;i+=max)lines.push(chars.slice(i,i+max).join(''));return lines.length?lines:[''];}
  // Pack each level independently: mixed-size nested frames do not widen every column.
  function pack(items,mode,gap=14,root=false){
    if(!items.length)return {width:0,height:0,positions:[]};
    const minWidth=Math.min(...items.map(i=>i.width)),maxWidth=Math.max(...items.map(i=>i.width));
    const area=items.reduce((sum,i)=>sum+(i.width+gap)*(i.height+gap),0);
    const minColumns=root&&items.length<=3?items.length:items.length>=4?2:1;
    const target=mode==='list'?maxWidth:Math.max(maxWidth,Math.sqrt(area*1.5),minColumns*minWidth+(minColumns-1)*gap);
    let x=0,y=0,rowHeight=0,width=0;const positions=[];
    for(const item of items){
      if(x&&(mode==='list'||x+item.width>target)){y+=rowHeight+gap;x=0;rowHeight=0;}
      positions.push({x,y});width=Math.max(width,x+item.width);rowHeight=Math.max(rowHeight,item.height);x+=item.width+gap;
    }
    return {width,height:y+rowHeight,positions};
  }
  function layout(data,ui,limit=200,offsets=new Map()){const s=selection(data,ui),priority=ui.selected?[...s.selected,...s.neighbors]:[],visible=new Set([...new Set([...priority,...s.ids])].slice(0,limit)),ids=s.ids.filter(id=>visible.has(id)),included=new Set(ids),activeParents=new Set();
    for(const id of ids){let b=s.blocks.get(id);while(b?.parent){activeParents.add(b.parent);included.add(b.parent);b=s.blocks.get(b.parent);}}
    const children=new Map();for(const id of included){const b=s.blocks.get(id),parent=included.has(b.parent)?b.parent:null;if(!children.has(parent))children.set(parent,[]);children.get(parent).push(id);}
    const cards=[],frames=[];
    function measure(id){const b=s.blocks.get(id),nested=children.get(id)||[],lines=wrap(b.label);if(!activeParents.has(id)||!nested.length)return {id,width:310,height:64+lines.length*16,lines};
      const items=nested.map(measure),packing=pack(items,ui.arrangement),header=34+lines.length*16;return {id,width:Math.max(310,packing.width)+28,height:header+packing.height+14,lines,items,packing,header};}
    function place(item,x,y){const node={...item,x,y,block:s.blocks.get(item.id)};if(item.items){frames.push(node);item.items.forEach((child,i)=>{const p=item.packing.positions[i];place(child,x+14+p.x,y+item.header+p.y);});}else cards.push(node);}
    const roots=(children.get(null)||[]).map(measure),packing=pack(roots,ui.arrangement,30,true);
    roots.forEach((item,i)=>{const p=packing.positions[i];place(item,24+p.x,28+p.y);});
    const positions=new Map(cards.map(c=>[c.id,c])),links=s.links.filter(l=>positions.has(l.from)&&positions.has(l.to)&&l.from!==l.to);
    const result={...s,cards,frames,positions,links,omitted:s.ids.length-ids.length};for(const node of [...cards,...frames]){node.baseX=node.x;node.baseY=node.y;node.baseWidth=node.width;node.baseHeight=node.height;}applyOffsets(result,offsets);return result;
  }
  function applyOffsets(layout,offsets){
    const all=new Map([...layout.cards,...layout.frames].map(n=>[n.id,n]));
    if(!layout.directChildren){layout.directChildren=new Map();for(const node of all.values()){const parent=node.block.parent;if(!layout.directChildren.has(parent))layout.directChildren.set(parent,[]);layout.directChildren.get(parent).push(node);}}
    for(const node of all.values()){
      let dx=0,dy=0,id=node.id;while(id){const offset=offsets.get(id)||[0,0];dx+=offset[0];dy+=offset[1];id=layout.blocks.get(id)?.parent;}
      node.x=node.baseX+dx;node.y=node.baseY+dy;node.width=node.baseWidth;node.height=node.baseHeight;
    }
    for(const frame of [...layout.frames].reverse()){
      const children=layout.directChildren.get(frame.id)||[];if(!children.length)continue;
      const right=Math.max(frame.x+frame.width,...children.map(n=>n.x+n.width+14)),bottom=Math.max(frame.y+frame.height,...children.map(n=>n.y+n.height+14));
      frame.x=Math.min(frame.x,...children.map(n=>n.x-14));frame.y=Math.min(frame.y,...children.map(n=>n.y-34-frame.lines.length*16));
      frame.width=right-frame.x;frame.height=bottom-frame.y;
    }
    layout.minX=Math.min(0,...[...all.values()].map(n=>n.x-24));layout.minY=Math.min(0,...[...all.values()].map(n=>n.y-24));
    layout.width=Math.max(100,...[...all.values()].map(n=>n.x+n.width+24))-layout.minX;layout.height=Math.max(100,...[...all.values()].map(n=>n.y+n.height+24))-layout.minY;
  }
  function neighborhood(layout,id,direction='both'){
    const base=new Set(layout.cards.filter(c=>layout.hasAncestor(c.id,id)).map(c=>c.id)),neighbors=new Set(base),edges=new Set();
    layout.links.forEach((link,i)=>{const from=base.has(link.from),to=base.has(link.to);if(direction==='both'?(from||to):direction==='incoming'?(!from&&to):(from&&!to)){neighbors.add(link.from);neighbors.add(link.to);edges.add(i);}});
    return {base,neighbors,edges};
  }
  function matrixWindow(data,ui,scrollTop=0,scrollLeft=0,height=550,width=900){const s=selection(data,ui),rowHeight=38,columnWidth=120,header=100,label=230;
    const rowCount=Math.ceil(height/rowHeight)+5,colCount=Math.ceil(width/columnWidth)+5,rowStart=Math.min(Math.max(0,s.ids.length-rowCount),Math.max(0,Math.floor((scrollTop-header)/rowHeight)-2)),rowEnd=Math.min(s.ids.length,rowStart+rowCount),colStart=Math.min(Math.max(0,s.ids.length-colCount),Math.max(0,Math.floor((scrollLeft-label)/columnWidth)-2)),colEnd=Math.min(s.ids.length,colStart+colCount);
    const links=s.linkLookup;return {...s,rowStart,rowEnd,colStart,colEnd,rowHeight,columnWidth,header,label,links,width:label+s.ids.length*columnWidth,height:header+s.ids.length*rowHeight};
  }
  return {kinds,defaults,readState,writeState,query,selection,wrap,layout,applyOffsets,neighborhood,matrixWindow};
});
